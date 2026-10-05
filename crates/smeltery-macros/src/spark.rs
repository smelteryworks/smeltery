//! `#[derive(Spark)]` and `#[actions]`: Sparks live components.

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{Data, DeriveInput, Fields, FnArg, ImplItem, ItemImpl, LitInt, LitStr, Pat, Token, Type};

/// The path of the Sparks crate from `crate = "…"`, default `::smeltery::sparks`.
fn crate_path(lit: Option<&LitStr>) -> syn::Result<(TokenStream2, String)> {
    match lit {
        Some(lit) => {
            let path = lit.parse::<syn::Path>()?;
            Ok((quote!(#path), lit.value()))
        }
        None => Ok((quote!(::smeltery::sparks), "::smeltery::sparks".to_owned())),
    }
}

/// `CounterPanel` → `counter_panel`.
fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
        })
}

struct Upload {
    field: String,
    max: u64,
    mimes: Vec<String>,
}

/// The `serde` options that give a field another key in the state than its Rust name. The Sparks allow-lists
/// (`MODEL`, `wire:model`, `form.title`) name fields by their Rust names and the runtime writes the state's keys,
/// so with these a model name could reach another field (sweep W6-05): they are refused.
const RENAMES: &[&str] = &[
    "rename",
    "rename_all",
    "rename_all_fields",
    "alias",
    "flatten",
];

/// The first of [`RENAMES`] in `tokens` (a `#[serde(…)]` list, nested lists included).
fn renaming(tokens: TokenStream2) -> Option<proc_macro2::Ident> {
    tokens.into_iter().find_map(|tree| match tree {
        proc_macro2::TokenTree::Ident(ident) if RENAMES.iter().any(|r| ident == r) => Some(ident),
        proc_macro2::TokenTree::Group(group) => renaming(group.stream()),
        _ => None,
    })
}

/// Refuse `#[serde(rename…)]`, `alias` and `flatten` on a Spark and its fields.
fn check_serde_names(input: &DeriveInput) -> syn::Result<()> {
    let mut attrs: Vec<&syn::Attribute> = input.attrs.iter().collect();
    if let Data::Struct(data) = &input.data {
        attrs.extend(data.fields.iter().flat_map(|f| f.attrs.iter()));
    }
    for attr in attrs.into_iter().filter(|a| a.path().is_ident("serde")) {
        if let syn::Meta::List(list) = &attr.meta
            && let Some(ident) = renaming(list.tokens.clone())
        {
            return Err(syn::Error::new(
                ident.span(),
                format!(
                    "`#[serde({ident} …)]` is not supported on a Spark: its state keys are its field names, which \
                     `wire:model` and `#[spark(model)]` use"
                ),
            ));
        }
    }
    Ok(())
}

pub(crate) fn derive(input: &DeriveInput) -> syn::Result<TokenStream2> {
    check_serde_names(input)?;
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "`#[derive(Spark)]` does not support generic structs",
        ));
    }
    let mut name: Option<LitStr> = None;
    let mut view: Option<LitStr> = None;
    let mut dir: Option<LitStr> = None;
    let mut krate: Option<LitStr> = None;
    let mut stream = false;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("spark")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                name = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("view") {
                view = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("dir") {
                dir = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("crate") {
                krate = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("stream") {
                stream = true;
            } else {
                return Err(meta.error(
                    "expected `name = \"…\"`, `view = \"…\"`, `stream`, `dir = \"…\"` or `crate = \"…\"`",
                ));
            }
            Ok(())
        })?;
    }
    let ident = &input.ident;
    let name_value = name
        .as_ref()
        .map_or_else(|| snake(&ident.to_string()), LitStr::value);
    if !valid_name(&name_value) {
        let span = name.as_ref().map_or_else(|| ident.span(), LitStr::span);
        return Err(syn::Error::new(
            span,
            format!(
                "invalid Spark name `{name_value}`: use lowercase letters, digits, `_`, `.` and `-`"
            ),
        ));
    }
    let view = view.unwrap_or_else(|| {
        LitStr::new(
            &format!("sparks/{}", name_value.replace('.', "/")),
            ident.span(),
        )
    });
    let (k, k_text) = crate_path(krate.as_ref())?;

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            ident,
            "`#[derive(Spark)]` needs a struct with named fields",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new_spanned(
            ident,
            "`#[derive(Spark)]` needs a struct with named fields",
        ));
    };
    let mut model = Vec::new();
    let mut uploads = Vec::new();
    for field in &named.named {
        let Some(fid) = &field.ident else { continue };
        let fname = syn::ext::IdentExt::unraw(fid).to_string();
        for attr in field.attrs.iter().filter(|a| a.path().is_ident("spark")) {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("model") {
                    if meta.input.peek(syn::token::Paren) {
                        // `model(fields = "title, body")`: a struct field the page writes key by key, only these.
                        let mut keys: Option<Vec<String>> = None;
                        meta.parse_nested_meta(|inner| {
                            if inner.path.is_ident("fields") {
                                let lit = inner.value()?.parse::<LitStr>()?;
                                let list: Vec<String> = lit
                                    .value()
                                    .split(',')
                                    .map(|k| k.trim().to_owned())
                                    .filter(|k| !k.is_empty())
                                    .collect();
                                let valid = |k: &String| {
                                    k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                                };
                                if list.is_empty() || !list.iter().all(valid) {
                                    return Err(syn::Error::new(
                                        lit.span(),
                                        "`fields` lists the keys the page may set: `fields = \"title, body\"`",
                                    ));
                                }
                                keys = Some(list);
                                Ok(())
                            } else {
                                Err(inner.error("expected `fields = \"key, key\"`"))
                            }
                        })?;
                        let keys = keys.ok_or_else(|| {
                            meta.error("expected `model(fields = \"key, key\")`")
                        })?;
                        model.extend(keys.into_iter().map(|k| format!("{fname}.{k}")));
                    } else {
                        model.push(fname.clone());
                    }
                } else if meta.path.is_ident("upload") {
                    let mut max = 10_240u64;
                    let mut mimes = Vec::new();
                    if meta.input.peek(syn::token::Paren) {
                        meta.parse_nested_meta(|inner| {
                            if inner.path.is_ident("max") {
                                max = inner.value()?.parse::<LitInt>()?.base10_parse()?;
                            } else if inner.path.is_ident("mimes") {
                                let list = inner.value()?.parse::<LitStr>()?.value();
                                mimes = list
                                    .split(',')
                                    .map(|m| m.trim().trim_start_matches('.').to_ascii_lowercase())
                                    .filter(|m| !m.is_empty())
                                    .collect();
                            } else {
                                return Err(inner.error(
                                    "expected `max = <kilobytes>` or `mimes = \"png,jpg\"`",
                                ));
                            }
                            Ok(())
                        })?;
                    }
                    uploads.push(Upload {
                        field: fname.clone(),
                        max,
                        mimes,
                    });
                } else {
                    return Err(meta.error(
                        "expected `model`, `model(fields = \"…\")` or `upload(max = …, mimes = \"…\")`",
                    ));
                }
                Ok(())
            })?;
        }
    }
    let upload_rules = uploads.iter().map(|u| {
        let (f, max) = (&u.field, u.max);
        let mimes = &u.mimes;
        quote!(#k::UploadRule::new(#f, #max, &[#(#mimes),*]))
    });

    // The view: the same codegen as `#[derive(Mold)]`, reached through the Sparks crate's re-exports.
    let mold_crate = format!("{k_text}::__private::mold");
    let mold_attr = match &dir {
        Some(dir) => quote!(#[mold(#view, crate = #mold_crate, dir = #dir)]),
        None => quote!(#[mold(#view, crate = #mold_crate)]),
    };
    let mut plain = input.clone();
    plain.attrs = vec![syn::parse_quote!(#mold_attr)];
    if let Data::Struct(s) = &mut plain.data {
        for f in &mut s.fields {
            f.attrs.clear();
        }
    }
    let name_lit = LitStr::new(&name_value, Span::call_site());
    Ok(quote! {
        #k::__private::mold_template! { #plain }

        #[automatically_derived]
        impl #k::Spark for #ident {
            const NAME: &'static str = #name_lit;
            const MODEL: &'static [&'static str] = &[#(#model),*];
            const UPLOADS: &'static [#k::UploadRule] = &[#(#upload_rules),*];
            const STREAM: bool = #stream;

            fn render_view(
                &self,
                engine: &#k::__private::mold::Engine,
                host: &dyn #k::__private::mold::Host,
            ) -> ::core::result::Result<::std::string::String, #k::__private::mold::Error> {
                if ::core::cfg!(debug_assertions) {
                    #k::__private::mold::Template::render_runtime_with(self, engine, host)
                } else {
                    #k::__private::mold::Template::render_compiled(self, host)
                }
            }
        }
    })
}

/// Whether `ty` is `&mut SparkCtx` (any path ending in `SparkCtx`).
fn is_ctx(ty: &Type) -> bool {
    let Type::Reference(r) = ty else { return false };
    if r.mutability.is_none() {
        return false;
    }
    let Type::Path(p) = &*r.elem else {
        return false;
    };
    p.path
        .segments
        .last()
        .is_some_and(|s| s.ident == "SparkCtx")
}

/// The most listeners (`#[on]` attributes) one component declares.
const MAX_LISTENERS: usize = 16;

/// A byte a channel name may hold (Pusher's rule): letters, digits and `_ - = @ , . ;`.
fn channel_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'=' | b'@' | b',' | b'.' | b';')
}

/// Check `#[on("anvil:<channel>", "<event>")]`; the channel template without its source prefix.
fn listener(channel: &LitStr, event: &LitStr) -> syn::Result<String> {
    let text = channel.value();
    let Some(template) = text.strip_prefix("anvil:") else {
        return Err(syn::Error::new(
            channel.span(),
            "a listener names its source and channel: `#[on(\"anvil:private-orders.{order_id}\", \"OrderShipped\")]`",
        ));
    };
    let invalid = |why: &str| {
        syn::Error::new(
            channel.span(),
            format!("the channel `{template}` is invalid: {why}"),
        )
    };
    if template.is_empty() || template.len() > 164 {
        return Err(invalid("a channel name is 1 to 164 characters"));
    }
    if template.starts_with("private-encrypted-") {
        return Err(invalid("encrypted channels are not supported"));
    }
    let mut rest = template;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('{') {
            let Some(end) = after.find('}') else {
                return Err(invalid("a `{` without its `}`"));
            };
            let name = after.get(..end).unwrap_or_default();
            let ok = name
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            if !ok {
                return Err(invalid("`{field}` names a field: letters, digits and `_`"));
            }
            rest = after.get(end + 1..).unwrap_or_default();
        } else {
            let len = rest.find('{').unwrap_or(rest.len());
            let literal = rest.get(..len).unwrap_or_default();
            if !literal.bytes().all(channel_byte) {
                return Err(invalid(
                    "a channel name holds letters, digits and `_ - = @ , . ;` (and `{field}`)",
                ));
            }
            rest = rest.get(len..).unwrap_or_default();
        }
    }
    let name = event.value();
    if name.is_empty()
        || name.len() > 200
        || name.chars().any(char::is_control)
        || name.starts_with("pusher:")
        || name.starts_with("pusher_internal:")
    {
        return Err(syn::Error::new(
            event.span(),
            "the event name is 1 to 200 characters without control characters (and not `pusher:…`)",
        ));
    }
    Ok(template.to_owned())
}

pub(crate) fn actions(attr: TokenStream2, mut item: ItemImpl) -> syn::Result<TokenStream2> {
    let mut krate: Option<LitStr> = None;
    if !attr.is_empty() {
        let parser = syn::meta::parser(|meta| {
            if meta.path.is_ident("crate") {
                krate = Some(meta.value()?.parse()?);
                Ok(())
            } else {
                Err(meta.error("expected `crate = \"path\"`"))
            }
        });
        syn::parse::Parser::parse2(parser, attr)?;
    }
    let (k, _) = crate_path(krate.as_ref())?;
    if item.trait_.is_some() {
        return Err(syn::Error::new_spanned(
            &item.self_ty,
            "`#[actions]` goes on an inherent impl block (`impl Counter { … }`)",
        ));
    }
    let self_ty = item.self_ty.clone();
    let mut infos = Vec::new();
    let mut arms = Vec::new();
    let mut mount = None;
    let mut updated = None;
    let mut rendering = None;
    let mut can_stream = None;
    let mut listeners = Vec::new();
    let mut listen_arms = Vec::new();
    for impl_item in &mut item.items {
        let ImplItem::Fn(f) = impl_item else { continue };
        let mut guards = Vec::new();
        let mut keep = Vec::new();
        let mut on = Vec::new();
        for a in f.attrs.drain(..) {
            if a.path().is_ident("on") {
                let args = a.parse_args_with(Punctuated::<LitStr, Token![,]>::parse_terminated)?;
                let (Some(channel), Some(event), 2) = (args.first(), args.get(1), args.len())
                else {
                    return Err(syn::Error::new_spanned(
                        &a,
                        "expected `#[on(\"anvil:<channel>\", \"<event>\")]`",
                    ));
                };
                on.push((listener(channel, event)?, event.value()));
            } else if a.path().is_ident("guard") {
                a.parse_nested_meta(|meta| {
                    if meta.path.is_ident("auth") {
                        guards.push(quote!(#k::Guard::Auth));
                        Ok(())
                    } else if meta.path.is_ident("guest") {
                        guards.push(quote!(#k::Guard::Guest));
                        Ok(())
                    } else {
                        Err(meta.error("expected `auth` or `guest`"))
                    }
                })?;
            } else {
                keep.push(a);
            }
        }
        f.attrs = keep;
        let sig = &f.sig;
        let fname = sig.ident.clone();
        if !on.is_empty() {
            if !guards.is_empty() {
                return Err(syn::Error::new_spanned(
                    &sig.ident,
                    "a listener takes no `#[guard]`: it runs only for channels the viewer may receive; check \
                     `ctx.auth()` inside for more",
                ));
            }
            let receiver_mut = matches!(sig.inputs.first(), Some(FnArg::Receiver(r)) if r.mutability.is_some() && r.reference.is_some());
            let is_hook = fname == "mount"
                || fname == "updated"
                || fname == "rendering"
                || fname == "can_stream";
            if sig.asyncness.is_none() || !receiver_mut || is_hook {
                return Err(syn::Error::new_spanned(
                    &sig.ident,
                    "a listener is `async fn name(&mut self, ctx: &mut SparkCtx, event: Payload) -> Result<()>` (not a hook)",
                ));
            }
            let method = fname.to_string();
            let mut args = Vec::new();
            let mut payloads = 0usize;
            for input in sig.inputs.iter().skip(1) {
                let FnArg::Typed(t) = input else { continue };
                if is_ctx(&t.ty) {
                    args.push(quote!(ctx));
                    continue;
                }
                if matches!(&*t.ty, Type::Reference(_)) || payloads > 0 {
                    return Err(syn::Error::new_spanned(
                        &t.ty,
                        "a listener takes `&mut SparkCtx` and at most one owned argument: the event's data, deserialized",
                    ));
                }
                let ty = &t.ty;
                args.push(quote!(#k::__private::listen_payload::<#ty>(payload, #method)?));
                payloads += 1;
            }
            listen_arms.push(quote! {
                #method => {
                    let _ = &payload;
                    <#self_ty>::#fname(self, #(#args),*).await
                }
            });
            for (channel, event) in on {
                listeners.push(quote!(#k::ListenerInfo::new(#method, #channel, #event)));
            }
            if listeners.len() > MAX_LISTENERS {
                return Err(syn::Error::new_spanned(
                    &sig.ident,
                    format!("a component declares at most {MAX_LISTENERS} listeners"),
                ));
            }
            continue;
        }
        let is_hook =
            fname == "mount" || fname == "updated" || fname == "rendering" || fname == "can_stream";
        let receiver_mut = matches!(sig.inputs.first(), Some(FnArg::Receiver(r)) if r.mutability.is_some() && r.reference.is_some());
        let public = matches!(f.vis, syn::Visibility::Public(_));
        if !is_hook && !(public && sig.asyncness.is_some() && receiver_mut) {
            if !guards.is_empty() {
                return Err(syn::Error::new_spanned(
                    &sig.ident,
                    "`#[guard]` goes on actions: `pub async fn name(&mut self, ctx: &mut SparkCtx, …)`",
                ));
            }
            continue;
        }
        if is_hook {
            let ok = sig.asyncness.is_some()
                && receiver_mut
                && sig
                    .inputs
                    .iter()
                    .nth(1)
                    .is_some_and(|a| matches!(a, FnArg::Typed(t) if is_ctx(&t.ty)))
                && if fname == "mount" || fname == "rendering" || fname == "can_stream" {
                    sig.inputs.len() == 2
                } else {
                    sig.inputs.len() == 3
                };
            if !ok {
                let expected = if fname == "mount" {
                    "async fn mount(&mut self, ctx: &mut SparkCtx) -> Result<()>"
                } else if fname == "rendering" {
                    "async fn rendering(&mut self, ctx: &mut SparkCtx) -> Result<()>"
                } else if fname == "can_stream" {
                    "async fn can_stream(&mut self, ctx: &mut SparkCtx) -> Result<bool>"
                } else {
                    "async fn updated(&mut self, ctx: &mut SparkCtx, field: &str) -> Result<()>"
                };
                return Err(syn::Error::new_spanned(
                    &sig.ident,
                    format!("the hook's signature is `{expected}`"),
                ));
            }
            if !guards.is_empty() {
                return Err(syn::Error::new_spanned(
                    &sig.ident,
                    "hooks take no `#[guard]`",
                ));
            }
            if fname == "mount" {
                mount = Some(
                    quote!(::std::boxed::Box::pin(async move { <#self_ty>::mount(self, ctx).await })),
                );
            } else if fname == "can_stream" {
                can_stream = Some(
                    quote!(::std::boxed::Box::pin(async move { <#self_ty>::can_stream(self, ctx).await })),
                );
            } else if fname == "rendering" {
                rendering = Some(
                    quote!(::std::boxed::Box::pin(async move { <#self_ty>::rendering(self, ctx).await })),
                );
            } else {
                updated = Some(
                    quote!(::std::boxed::Box::pin(async move { <#self_ty>::updated(self, ctx, field).await })),
                );
            }
            continue;
        }
        let method = fname.to_string();
        let mut args = Vec::new();
        let mut lets = Vec::new();
        let mut index = 0usize;
        for input in sig.inputs.iter().skip(1) {
            let FnArg::Typed(t) = input else { continue };
            if is_ctx(&t.ty) {
                args.push(quote!(ctx));
                continue;
            }
            if matches!(&*t.ty, Type::Reference(_)) {
                return Err(syn::Error::new_spanned(
                    &t.ty,
                    "action parameters are owned types deserialized from the call (`String`, `i64`, …)",
                ));
            }
            let ty = &t.ty;
            let var = match &*t.pat {
                Pat::Ident(p) => format_ident!("__p_{}", p.ident),
                _ => format_ident!("__p{}", index),
            };
            lets.push(quote!(let #var: #ty = #k::__private::param(&params, #index, #method)?;));
            args.push(quote!(#var));
            index += 1;
        }
        let count = index;
        arms.push(quote! {
            #method => {
                #k::__private::param_count(&params, #count, #method)?;
                #(#lets)*
                <#self_ty>::#fname(self, #(#args),*).await
            }
        });
        infos.push(quote!(#k::ActionInfo::new(#method, &[#(#guards),*])));
    }
    let mount = mount.map(|body| {
        quote! {
            fn mount_hook<'a>(&'a mut self, ctx: &'a mut #k::SparkCtx) -> #k::__private::BoxFuture<'a, #k::__private::Result<()>> {
                #body
            }
        }
    });
    let updated = updated.map(|body| {
        quote! {
            fn updated_hook<'a>(&'a mut self, ctx: &'a mut #k::SparkCtx, field: &'a str) -> #k::__private::BoxFuture<'a, #k::__private::Result<()>> {
                #body
            }
        }
    });
    let rendering = rendering.map(|body| {
        quote! {
            fn rendering_hook<'a>(&'a mut self, ctx: &'a mut #k::SparkCtx) -> #k::__private::BoxFuture<'a, #k::__private::Result<()>> {
                #body
            }
        }
    });
    let can_stream = can_stream.map(|body| {
        quote! {
            fn stream_hook<'a>(&'a mut self, ctx: &'a mut #k::SparkCtx) -> #k::__private::BoxFuture<'a, #k::__private::Result<bool>> {
                #body
            }
        }
    });
    let listen = (!listen_arms.is_empty()).then(|| {
        quote! {
            const LISTENERS: &'static [#k::ListenerInfo] = &[#(#listeners),*];

            #[allow(unused_variables, unreachable_code, clippy::let_unit_value)]
            fn listen<'a>(
                &'a mut self,
                method: &'a str,
                payload: #k::__private::serde_json::Value,
                ctx: &'a mut #k::SparkCtx,
            ) -> #k::__private::BoxFuture<'a, #k::__private::Result<()>> {
                ::std::boxed::Box::pin(async move {
                    match method {
                        #(#listen_arms)*
                        _ => ::core::result::Result::Err(#k::__private::unknown_action(method)),
                    }
                })
            }
        }
    });
    Ok(quote! {
        #item

        #[automatically_derived]
        impl #k::Actions for #self_ty {
            const ACTIONS: &'static [#k::ActionInfo] = &[#(#infos),*];

            #[allow(unused_variables, unreachable_code, clippy::let_unit_value)]
            fn call<'a>(
                &'a mut self,
                method: &'a str,
                params: ::std::vec::Vec<#k::__private::serde_json::Value>,
                ctx: &'a mut #k::SparkCtx,
            ) -> #k::__private::BoxFuture<'a, #k::__private::Result<()>> {
                ::std::boxed::Box::pin(async move {
                    match method {
                        #(#arms)*
                        _ => ::core::result::Result::Err(#k::__private::unknown_action(method)),
                    }
                })
            }

            #mount
            #updated
            #rendering
            #can_stream
            #listen
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    fn expand(item: ItemImpl) -> String {
        match actions(TokenStream2::new(), item) {
            Ok(tokens) => tokens.to_string(),
            Err(e) => format!("error: {e}"),
        }
    }

    /// Sweep W6-05: a serde rename would make the model allow-list (Rust names) and the state (serde names) disagree.
    #[test]
    fn serde_renames_are_refused_on_sparks() {
        let refused = |input: DeriveInput, what: &str| {
            let err = derive(&input)
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default();
            assert!(
                err.contains(&format!("`#[serde({what} …)]` is not supported")),
                "{what}: {err}"
            );
        };
        refused(
            parse_quote! { struct A { #[spark(model)] #[serde(rename = "role")] label: String, role: String } },
            "rename",
        );
        refused(
            parse_quote! { #[serde(rename_all = "camelCase")] struct A { #[spark(model)] is_admin: bool } },
            "rename_all",
        );
        refused(
            parse_quote! { struct A { #[serde(alias = "x")] y: i64 } },
            "alias",
        );
        refused(
            parse_quote! { struct A { #[serde(default, flatten)] extra: Extra } },
            "flatten",
        );
        refused(
            parse_quote! { struct A { #[serde(rename(deserialize = "x"))] y: i64 } },
            "rename",
        );
        // Other serde options stay allowed.
        let ok: DeriveInput = parse_quote! {
            struct A { #[spark(model)] #[serde(default, skip_serializing_if = "String::is_empty")] title: String }
        };
        assert!(derive(&ok).is_ok());
    }

    #[test]
    fn listeners_expand_and_are_not_actions() {
        let out = expand(parse_quote! {
            impl Order {
                #[on("anvil:private-orders.{order_id}", "OrderShipped")]
                #[on("anvil:news", "Posted")]
                pub async fn shipped(&mut self, ctx: &mut SparkCtx, event: Shipped) -> Result<()> { Ok(()) }
                pub async fn save(&mut self) -> Result<()> { Ok(()) }
            }
        });
        assert!(out.contains("const LISTENERS"), "{out}");
        assert!(
            out.contains(
                r#"ListenerInfo :: new ("shipped" , "private-orders.{order_id}" , "OrderShipped")"#
            ),
            "{out}"
        );
        assert!(
            out.contains(r#"ListenerInfo :: new ("shipped" , "news" , "Posted")"#),
            "{out}"
        );
        assert!(out.contains("listen_payload :: < Shipped >"), "{out}");
        assert!(out.contains(r#"ActionInfo :: new ("save""#), "{out}");
        assert!(
            !out.contains(r#"ActionInfo :: new ("shipped""#),
            "a listener is not an action: {out}"
        );
        let plain = expand(
            parse_quote! { impl Counter { pub async fn inc(&mut self) -> Result<()> { Ok(()) } } },
        );
        assert!(!plain.contains("LISTENERS"), "{plain}");
    }

    #[test]
    fn listener_mistakes_are_compile_errors() {
        let cases: Vec<(ItemImpl, &str)> = vec![
            (
                parse_quote! { impl A { #[on("private-orders.{id}", "E")] async fn f(&mut self) -> Result<()> { Ok(()) } } },
                "names its source",
            ),
            (
                parse_quote! { impl A { #[on("anvil:orders.{1d}", "E")] async fn f(&mut self) -> Result<()> { Ok(()) } } },
                "names a field",
            ),
            (
                parse_quote! { impl A { #[on("anvil:orders.{id", "E")] async fn f(&mut self) -> Result<()> { Ok(()) } } },
                "without its `}`",
            ),
            (
                parse_quote! { impl A { #[on("anvil:a b", "E")] async fn f(&mut self) -> Result<()> { Ok(()) } } },
                "letters, digits",
            ),
            (
                parse_quote! { impl A { #[on("anvil:private-encrypted-x", "E")] async fn f(&mut self) -> Result<()> { Ok(()) } } },
                "encrypted",
            ),
            (
                parse_quote! { impl A { #[on("anvil:news", "pusher:x")] async fn f(&mut self) -> Result<()> { Ok(()) } } },
                "event name",
            ),
            (
                parse_quote! { impl A { #[on("anvil:news")] async fn f(&mut self) -> Result<()> { Ok(()) } } },
                "expected",
            ),
            (
                parse_quote! { impl A { #[on("anvil:news", "E")] #[guard(auth)] async fn f(&mut self) -> Result<()> { Ok(()) } } },
                "no `#[guard]`",
            ),
            (
                parse_quote! { impl A { #[on("anvil:news", "E")] fn f(&mut self) -> Result<()> { Ok(()) } } },
                "a listener is",
            ),
            (
                parse_quote! { impl A { #[on("anvil:news", "E")] async fn f(&mut self, a: A, b: B) -> Result<()> { Ok(()) } } },
                "at most one owned argument",
            ),
            (
                parse_quote! { impl A { #[on("anvil:news", "E")] async fn mount(&mut self, ctx: &mut SparkCtx) -> Result<()> { Ok(()) } } },
                "not a hook",
            ),
        ];
        for (item, expected) in cases {
            let out = expand(item);
            assert!(
                out.starts_with("error:") && out.contains(expected),
                "{expected}: {out}"
            );
        }
        let many: Vec<syn::Attribute> = (0..17)
            .map(|i| {
                let event = format!("E{i}");
                parse_quote! { #[on("anvil:news", #event)] }
            })
            .collect();
        let item: ItemImpl =
            parse_quote! { impl A { #(#many)* async fn f(&mut self) -> Result<()> { Ok(()) } } };
        assert!(expand(item).contains("at most 16"));
    }
}
