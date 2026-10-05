//! Derive macros of the [Smeltery](https://github.com/smelteryworks/smeltery) framework.
//!
//! `#[derive(Validate)]` implements `smeltery::validation::Validate` from `#[validate(...)]`
//! field attributes. Apps use it as `smeltery::Validate`. `#[derive(Spark)]` and `#[actions]` make a
//! Sparks live component (`smeltery::Spark`, `smeltery::actions`). `#[derive(Alloy)]` makes a struct an Alloy
//! (Inertia) page (`smeltery::Alloy`). `#[derive(BroadcastEvent)]` makes a struct an Anvil event
//! (`smeltery::anvil::BroadcastEvent`).
#![cfg_attr(docsrs, feature(doc_cfg))]

// The README's sample runs as a doctest without becoming the crate docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

mod alloy;
mod broadcast;
mod spark;

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::punctuated::Punctuated;
use syn::{Data, DeriveInput, Fields, Ident, Lit, LitStr, Token, parse_macro_input};

/// Implement `Validate` from `#[validate(...)]` attributes.
///
/// Field rules: `required`, `email`, `url`, `min = n`, `max = n`, `between(a, b)`,
/// `numeric`, `integer`, `alpha`, `alpha_num`, `alpha_dash`, `in_list("a", "b")`,
/// `confirmed`, `same = "other"`, `accepted`, `unique(table = "t", column = "c")` (with
/// `except_id` for the route's last parameter or `except_id = "field"`),
/// `exists(table = "t", column = "c")`, and `message = "…"` to replace the messages of
/// that field. On the struct, `#[validate(crate = "path")]` sets the path of the validation
/// module (default `::smeltery::validation`).
#[proc_macro_derive(Validate, attributes(validate))]
pub fn derive_validate(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Make a struct a Sparks live component: implements `smeltery::sparks::Spark` and compiles its Mold view.
///
/// On the struct, `#[spark(...)]` takes `name = "counter"` (default: the struct name in snake case),
/// `view = "sparks/counter"` (default: `sparks/<name>`, under `resources/views`), `stream` (the component
/// receives `Broadcast` pushes), `dir = "…"` (the views directory relative to the crate) and `crate = "…"`
/// (default `::smeltery::sparks`). On fields, `#[spark(model)]` lets `wire:model` set the field and
/// `#[spark(upload(max = 2048, mimes = "png,jpg"))]` makes it a file upload (`Option<TemporaryUpload>`, `max` in
/// kilobytes). The component also needs an `#[actions]` impl block.
#[proc_macro_derive(Spark, attributes(spark))]
pub fn derive_spark(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match spark::derive(&input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Make a struct an Anvil event: implements `smeltery::anvil::BroadcastEvent` from `#[broadcast(...)]`.
///
/// `public = "news"` and `private = "orders.{order_id}"` name the channels (several allowed; a private channel is
/// written without its `private-` prefix); `{field}` is the field's `Display` value, and a field the struct does not
/// have is a compile error. A value goes into the name as it is: one holding a `.` adds a segment, so it matches
/// another pattern; use fields whose values have no dots (ids). `as = "order.shipped"` sets the event name (default
/// `App\Events\<TypeName>`); `crate = "…"` sets the path of the Anvil crate (default `::smeltery::anvil`).
/// The struct must also implement `serde::Serialize`: its JSON is the event's data.
///
/// ```
/// use serde::Serialize;
/// use smeltery::anvil::{BroadcastEvent, Channel};
///
/// #[derive(Serialize, BroadcastEvent)]
/// #[broadcast(private = "orders.{order_id}", as = "order.shipped")]
/// struct OrderShipped {
///     order_id: i64,
/// }
///
/// let event = OrderShipped { order_id: 7 };
/// assert_eq!(event.channels(), vec![Channel::private("orders.7")]);
/// assert_eq!(event.name(), "order.shipped");
/// ```
#[proc_macro_derive(BroadcastEvent, attributes(broadcast))]
pub fn derive_broadcast_event(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match broadcast::derive(&input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Make a struct an Alloy (Inertia) page: `#[alloy("posts/index")]` names the React / Vue component.
///
/// Implements `smeltery::alloy::Component` (the struct must implement `serde::Serialize` and serialize to a JSON
/// object; its fields become the props) and makes the struct a response, so a handler returns it. Lazy props go
/// through `Component::into_page`.
///
/// ```
/// #[derive(serde::Serialize, smeltery::Alloy)]
/// #[alloy("posts/index")]
/// struct PostsIndex {
///     titles: Vec<String>,
/// }
///
/// async fn index() -> PostsIndex {
///     PostsIndex { titles: vec!["Hello".into()] }
/// }
/// ```
#[proc_macro_derive(Alloy, attributes(alloy))]
pub fn derive_alloy(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match alloy::derive(&input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// The actions of a Sparks component: every `pub async fn` with a `&mut self` receiver in this impl block is
/// callable from the page (`wire:click="increment"`); other methods are not.
///
/// An action takes `&mut self`, optionally `ctx: &mut SparkCtx`, then parameters deserialized from the call
/// (`wire:click="remove(3)"`), and returns `smeltery::Result<()>`. `#[guard(auth)]` on an action requires a
/// signed-in user (`#[guard(guest)]` the opposite). `async fn mount(&mut self, ctx: &mut SparkCtx)` runs when the
/// component is first rendered and `async fn updated(&mut self, ctx: &mut SparkCtx, field: &str)` after a
/// `wire:model` update. `#[on("anvil:private-orders.{order_id}", "OrderShipped")]` on an
/// `async fn (&mut self, ctx: &mut SparkCtx, event: T)` makes it a listener: it runs when that event is broadcast on
/// that channel (`{field}` is the state field's value) and is not callable as an action.
/// `#[actions(crate = "…")]` sets the Sparks crate path (default `::smeltery::sparks`).
#[proc_macro_attribute]
pub fn actions(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as syn::ItemImpl);
    match spark::actions(attr.into(), item) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

enum Rule {
    Required,
    Email,
    Url,
    Min(f64),
    Max(f64),
    Between(f64, f64),
    Numeric,
    Integer,
    Alpha,
    AlphaNum,
    AlphaDash,
    InList(Vec<String>),
    Mimes(Vec<String>),
    Confirmed,
    Same(String),
    Accepted,
    Unique {
        table: String,
        column: String,
        except: Except,
    },
    Exists {
        table: String,
        column: String,
    },
}

enum Except {
    None,
    RouteKey,
    Field(String),
}

struct FieldRules {
    name: Ident,
    rules: Vec<Rule>,
    message: Option<String>,
}

fn number(lit: &Lit) -> syn::Result<f64> {
    match lit {
        Lit::Int(i) => i.base10_parse::<f64>(),
        Lit::Float(f) => f.base10_parse::<f64>(),
        other => Err(syn::Error::new_spanned(other, "expected a number")),
    }
}

fn table_column(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<(String, String, Except)> {
    let mut table = None;
    let mut column = None;
    let mut except = Except::None;
    meta.parse_nested_meta(|inner| {
        if inner.path.is_ident("table") {
            table = Some(inner.value()?.parse::<LitStr>()?.value());
        } else if inner.path.is_ident("column") {
            column = Some(inner.value()?.parse::<LitStr>()?.value());
        } else if inner.path.is_ident("except_id") {
            except = if inner.input.peek(Token![=]) {
                Except::Field(inner.value()?.parse::<LitStr>()?.value())
            } else {
                Except::RouteKey
            };
        } else {
            return Err(inner.error("expected `table`, `column` or `except_id`"));
        }
        Ok(())
    })?;
    let table = table.ok_or_else(|| meta.error("missing `table = \"…\"`"))?;
    let column = column.ok_or_else(|| meta.error("missing `column = \"…\"`"))?;
    Ok((table, column, except))
}

fn parse_field(field: &syn::Field) -> syn::Result<Option<FieldRules>> {
    let Some(name) = field.ident.clone() else {
        return Ok(None);
    };
    let mut rules = Vec::new();
    let mut message = None;
    for attr in field.attrs.iter().filter(|a| a.path().is_ident("validate")) {
        attr.parse_nested_meta(|meta| {
            let p = &meta.path;
            let simple = match p.get_ident().map(ToString::to_string).as_deref() {
                Some("required") => Some(Rule::Required),
                Some("email") => Some(Rule::Email),
                Some("url") => Some(Rule::Url),
                Some("numeric") => Some(Rule::Numeric),
                Some("integer") => Some(Rule::Integer),
                Some("alpha") => Some(Rule::Alpha),
                Some("alpha_num") => Some(Rule::AlphaNum),
                Some("alpha_dash") => Some(Rule::AlphaDash),
                Some("confirmed") => Some(Rule::Confirmed),
                Some("accepted") => Some(Rule::Accepted),
                _ => None,
            };
            if let Some(rule) = simple {
                rules.push(rule);
            } else if p.is_ident("min") {
                rules.push(Rule::Min(number(&meta.value()?.parse()?)?));
            } else if p.is_ident("max") {
                rules.push(Rule::Max(number(&meta.value()?.parse()?)?));
            } else if p.is_ident("between") {
                let content;
                syn::parenthesized!(content in meta.input);
                let nums = Punctuated::<Lit, Token![,]>::parse_terminated(&content)?;
                let nums: Vec<f64> = nums.iter().map(number).collect::<syn::Result<_>>()?;
                let [a, b] = nums.as_slice() else {
                    return Err(meta.error("`between` takes two numbers: between(1, 120)"));
                };
                rules.push(Rule::Between(*a, *b));
            } else if p.is_ident("in_list") {
                let content;
                syn::parenthesized!(content in meta.input);
                let items = Punctuated::<LitStr, Token![,]>::parse_terminated(&content)?;
                rules.push(Rule::InList(items.iter().map(LitStr::value).collect()));
            } else if p.is_ident("mimes") {
                let lit = meta.value()?.parse::<LitStr>()?;
                let exts: Vec<String> = lit
                    .value()
                    .split(',')
                    .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
                    .filter(|e| !e.is_empty())
                    .collect();
                if exts.is_empty() {
                    return Err(syn::Error::new_spanned(
                        lit,
                        "`mimes` takes extensions: mimes = \"png,jpg\"",
                    ));
                }
                rules.push(Rule::Mimes(exts));
            } else if p.is_ident("same") {
                rules.push(Rule::Same(meta.value()?.parse::<LitStr>()?.value()));
            } else if p.is_ident("message") {
                message = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if p.is_ident("unique") {
                let (table, column, except) = table_column(&meta)?;
                rules.push(Rule::Unique {
                    table,
                    column,
                    except,
                });
            } else if p.is_ident("exists") {
                let (table, column, except) = table_column(&meta)?;
                if !matches!(except, Except::None) {
                    return Err(meta.error("`except_id` belongs to `unique`"));
                }
                rules.push(Rule::Exists { table, column });
            } else {
                return Err(meta.error(
                    "unknown rule (rules: required, email, url, min, max, between, numeric, integer, \
                     alpha, alpha_num, alpha_dash, in_list, mimes, confirmed, same, accepted, unique, exists, \
                     message)",
                ));
            }
            Ok(())
        })?;
    }
    if rules.is_empty() && message.is_none() {
        return Ok(None);
    }
    Ok(Some(FieldRules {
        name,
        rules,
        message,
    }))
}

fn struct_crate(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let mut path: TokenStream2 = quote!(::smeltery::validation);
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("validate")) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate") {
                let lit = meta.value()?.parse::<LitStr>()?;
                let parsed = lit.parse::<syn::Path>()?;
                path = quote!(#parsed);
                Ok(())
            } else {
                Err(meta.error("expected `crate = \"path\"`"))
            }
        })?;
    }
    Ok(path)
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(Validate)] works on structs with named fields",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(Validate)] works on structs with named fields",
        ));
    };
    let k = struct_crate(input)?;
    let field_names: Vec<String> = named
        .named
        .iter()
        .filter_map(|f| f.ident.as_ref().map(ToString::to_string))
        .collect();
    let mut fields = Vec::new();
    for f in &named.named {
        if let Some(rules) = parse_field(f)? {
            fields.push(rules);
        }
    }

    let mut typed = Vec::new();
    let mut raw = Vec::new();
    let mut type_messages = Vec::new();
    for f in &fields {
        let ident = &f.name;
        let field = ident.to_string();
        let label = field.replace('_', " ");
        let custom = match &f.message {
            Some(m) => quote!(::core::option::Option::Some(#m)),
            None => quote!(::core::option::Option::None::<&str>),
        };
        let add = quote! {
            __errors.add(#field, match __custom { ::core::option::Option::Some(c) => c.to_owned(), ::core::option::Option::None => __m });
        };
        let required = f.rules.iter().any(|r| matches!(r, Rule::Required));
        let mut checks = Vec::new();
        let mut raw_checks = Vec::new();
        for rule in &f.rules {
            let check = match rule {
                Rule::Required => quote!(#k::rules::required(&__s, #label)),
                Rule::Email => quote!(#k::rules::email(&__s, #label)),
                Rule::Url => quote!(#k::rules::url(&__s, #label)),
                Rule::Min(n) => quote!(#k::rules::min(&__s, #label, #n)),
                Rule::Max(n) => quote!(#k::rules::max(&__s, #label, #n)),
                Rule::Between(a, b) => quote!(#k::rules::between(&__s, #label, #a, #b)),
                Rule::Numeric => quote!(#k::rules::numeric(&__s, #label)),
                Rule::Integer => quote!(#k::rules::integer(&__s, #label)),
                Rule::Alpha => quote!(#k::rules::alpha(&__s, #label)),
                Rule::AlphaNum => quote!(#k::rules::alpha_num(&__s, #label)),
                Rule::AlphaDash => quote!(#k::rules::alpha_dash(&__s, #label)),
                Rule::InList(items) => quote!(#k::rules::in_list(&__s, #label, &[#(#items),*])),
                Rule::Accepted => quote!(#k::rules::accepted(&__s, #label)),
                Rule::Mimes(exts) => quote!(#k::rules::mimes(&__s, #label, &[#(#exts),*])),
                Rule::Confirmed => {
                    let other = format!("{field}_confirmation");
                    let value = other_value(&k, &field_names, &other);
                    quote!(#k::rules::confirmed(&__s, #label, #value))
                }
                Rule::Same(other) => {
                    let other_label = other.replace('_', " ");
                    let value = other_value(&k, &field_names, other);
                    quote!(#k::rules::same(&__s, #label, #other_label, #value))
                }
                Rule::Unique {
                    table,
                    column,
                    except,
                } => {
                    let except = match except {
                        Except::None => {
                            quote!(::core::option::Option::None::<::std::string::String>)
                        }
                        Except::RouteKey => {
                            quote!(__ctx.route_key().map(::std::borrow::ToOwned::to_owned))
                        }
                        Except::Field(name) => {
                            if !field_names.contains(name) {
                                return Err(syn::Error::new(
                                    Span::call_site(),
                                    format!(
                                        "`except_id = \"{name}\"`: the struct has no field `{name}`"
                                    ),
                                ));
                            }
                            let id = Ident::new(name, Span::call_site());
                            quote!(#k::rules::text_of(&#k::rules::AsSubject::as_subject(&self.#id)))
                        }
                    };
                    quote!({
                        let __except: ::core::option::Option<::std::string::String> = #except;
                        #k::rules::unique(__ctx, &__s, #label, #table, #column, __except.as_deref()).await
                    })
                }
                Rule::Exists { table, column } => {
                    quote!(#k::rules::exists(__ctx, &__s, #label, #table, #column).await)
                }
            };
            if matches!(
                rule,
                Rule::Required | Rule::Email | Rule::Url | Rule::Numeric | Rule::Integer
            ) {
                raw_checks.push(check.clone());
            }
            checks.push(check);
        }
        typed.push(quote! {
            {
                let __custom = #custom;
                let __s = #k::rules::AsSubject::as_subject(&self.#ident);
                if __s.is_absent() {
                    if #required {
                        let __m = #k::rules::msg_required(#label);
                        #add
                    }
                } else {
                    #(
                        if let ::core::option::Option::Some(__m) = #checks { #add }
                    )*
                }
            }
        });
        raw.push(quote! {
            {
                let __custom = #custom;
                let __s = #k::rules::raw(__input, #field);
                if __s.is_absent() {
                    if #required {
                        let __m = #k::rules::msg_required(#label);
                        #add
                    }
                } else {
                    #(
                        if let ::core::option::Option::Some(__m) = #raw_checks { #add }
                    )*
                }
            }
        });
        let type_message = if f.rules.iter().any(|r| matches!(r, Rule::Integer)) {
            Some(format!("The {label} field must be an integer."))
        } else if f.rules.iter().any(|r| matches!(r, Rule::Numeric)) {
            Some(format!("The {label} field must be a number."))
        } else {
            None
        };
        if let Some(m) = type_message.or_else(|| f.message.clone()) {
            type_messages.push(
                quote!(#field => ::core::option::Option::Some(::std::string::String::from(#m)),),
            );
        }
    }

    let ident = &input.ident;
    let (impl_g, ty_g, where_g) = input.generics.split_for_impl();
    Ok(quote! {
        #[automatically_derived]
        impl #impl_g #k::Validate for #ident #ty_g #where_g {
            #[allow(clippy::all, unused_variables, unused_mut)]
            fn validate(
                &self,
                __ctx: &#k::ValidationContext<'_>,
            ) -> impl ::core::future::Future<
                Output = ::core::result::Result<(), #k::ValidationErrors>,
            > + ::core::marker::Send {
                async move {
                    let mut __errors = #k::ValidationErrors::new();
                    #(#typed)*
                    __errors.into_result()
                }
            }

            #[allow(clippy::all, unused_variables, unused_mut)]
            fn validate_input(__input: &#k::Input) -> #k::ValidationErrors {
                let mut __errors = #k::ValidationErrors::new();
                #(#raw)*
                __errors
            }

            #[allow(clippy::all)]
            fn type_message(__field: &str) -> ::core::option::Option<::std::string::String> {
                match __field {
                    #(#type_messages)*
                    _ => ::core::option::Option::None,
                }
            }
        }
    })
}

/// The value of `other` as text: the struct's field when it has one, else the raw input.
fn other_value(k: &TokenStream2, field_names: &[String], other: &str) -> TokenStream2 {
    if field_names.iter().any(|f| f == other) {
        let id = Ident::new(other, Span::call_site());
        quote!(#k::rules::text_of(&#k::rules::AsSubject::as_subject(&self.#id)))
    } else {
        quote!(__ctx.input(#other).map(::std::borrow::ToOwned::to_owned))
    }
}
