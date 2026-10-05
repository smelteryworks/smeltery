//! `#[derive(BroadcastEvent)]`: an event for Anvil, its channels and its name from `#[broadcast(...)]`.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::ext::IdentExt as _;
use syn::parse::{Parse, ParseStream};
use syn::{Data, DeriveInput, Fields, Ident, LitStr, Token};

const NAMED_FIELDS: &str = "`#[derive(BroadcastEvent)]` needs a struct with named fields";
const KEYS: &str = "expected `public = \"…\"`, `private = \"…\"`, `presence = \"…\"`, `as = \"…\"` or `crate = \"…\"`";

/// One `key = "value"` of `#[broadcast(...)]`.
struct Item {
    key: Ident,
    value: LitStr,
}

struct Items(Vec<Item>);

impl Parse for Items {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut items = Vec::new();
        while !input.is_empty() {
            let key = Ident::parse_any(input).map_err(|e| syn::Error::new(e.span(), KEYS))?;
            input
                .parse::<Token![=]>()
                .map_err(|e| syn::Error::new(e.span(), KEYS))?;
            let value: LitStr = input.parse()?;
            items.push(Item { key, value });
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        Ok(Self(items))
    }
}

/// A channel template's pieces.
enum Piece {
    Text(String),
    Field(Ident),
}

fn channel_byte(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '=' | '@' | ',' | '.' | ';')
}

/// Split `orders.{order_id}` into text and fields; every field must be one of `fields`.
fn template(lit: &LitStr, fields: &[Ident]) -> syn::Result<Vec<Piece>> {
    let text = lit.value();
    if text.is_empty() {
        return Err(syn::Error::new(lit.span(), "the channel name is empty"));
    }
    let mut pieces = Vec::new();
    let mut rest = text.as_str();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('{') {
            let Some(end) = after.find('}') else {
                return Err(syn::Error::new(lit.span(), "a `{` without its `}`"));
            };
            let name = after.get(..end).unwrap_or_default();
            let Some(field) = fields.iter().find(|f| f.unraw() == name) else {
                return Err(syn::Error::new(
                    lit.span(),
                    format!("the struct has no field `{name}` for `{{{name}}}`"),
                ));
            };
            pieces.push(Piece::Field(field.clone()));
            rest = after.get(end + 1..).unwrap_or_default();
        } else {
            let end = rest.find('{').unwrap_or(rest.len());
            let literal = rest.get(..end).unwrap_or_default();
            if let Some(bad) = literal.chars().find(|c| !channel_byte(*c)) {
                return Err(syn::Error::new(
                    lit.span(),
                    format!(
                        "`{bad}` cannot be in a channel name (letters, digits and `_ - = @ , . ;`)"
                    ),
                ));
            }
            pieces.push(Piece::Text(literal.to_owned()));
            rest = rest.get(end..).unwrap_or_default();
        }
    }
    Ok(pieces)
}

pub(crate) fn derive(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let fields: Vec<Ident> = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => named.named.iter().filter_map(|f| f.ident.clone()).collect(),
            _ => return Err(syn::Error::new_spanned(&input.ident, NAMED_FIELDS)),
        },
        _ => return Err(syn::Error::new_spanned(&input.ident, NAMED_FIELDS)),
    };
    let mut channels = Vec::new();
    let mut name: Option<LitStr> = None;
    let mut krate: Option<syn::Path> = None;
    for attr in input
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("broadcast"))
    {
        for item in attr.parse_args::<Items>()?.0 {
            match item.key.to_string().as_str() {
                kind @ ("public" | "private" | "presence") => {
                    channels.push((kind.to_owned(), template(&item.value, &fields)?));
                }
                "as" => {
                    if name.is_some() {
                        return Err(syn::Error::new(item.key.span(), "`as` is given twice"));
                    }
                    let value = item.value.value();
                    if value.starts_with("pusher:") || value.starts_with("pusher_internal:") {
                        return Err(syn::Error::new(
                            item.value.span(),
                            "the prefixes `pusher:` and `pusher_internal:` are reserved by the protocol",
                        ));
                    }
                    if value.is_empty() || value.len() > 200 || value.chars().any(char::is_control)
                    {
                        return Err(syn::Error::new(
                            item.value.span(),
                            "the event name is 1 to 200 bytes without control characters",
                        ));
                    }
                    name = Some(item.value);
                }
                "crate" => {
                    if krate.is_some() {
                        return Err(syn::Error::new(item.key.span(), "`crate` is given twice"));
                    }
                    krate = Some(item.value.parse()?);
                }
                _ => return Err(syn::Error::new(item.key.span(), KEYS)),
            }
        }
    }
    if channels.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "missing a channel: `#[broadcast(private = \"orders.{order_id}\")]` (or `public` / `presence`)",
        ));
    }
    let krate = krate.unwrap_or_else(|| syn::parse_quote!(::smeltery::anvil));
    let ident = &input.ident;
    let name = name.map_or_else(
        || format!("App\\Events\\{}", ident.unraw()),
        |lit| lit.value(),
    );
    let built = channels.iter().map(|(kind, pieces)| {
        let mut format = String::new();
        let mut args = Vec::new();
        for piece in pieces {
            match piece {
                Piece::Text(text) => format.push_str(text),
                Piece::Field(field) => {
                    format.push_str("{}");
                    args.push(quote!(self.#field));
                }
            }
        }
        let ctor = match kind.as_str() {
            "private" => quote!(#krate::Channel::private),
            "presence" => quote!(#krate::Channel::presence),
            _ => quote!(#krate::Channel::public),
        };
        quote!(#ctor(::std::format!(#format #(, #args)*)))
    });
    let (impl_g, ty_g, where_c) = input.generics.split_for_impl();
    Ok(quote! {
        #[automatically_derived]
        impl #impl_g #krate::BroadcastEvent for #ident #ty_g #where_c {
            fn channels(&self) -> ::std::vec::Vec<#krate::Channel> {
                ::std::vec![#(#built),*]
            }

            fn name(&self) -> ::std::borrow::Cow<'static, str> {
                ::std::borrow::Cow::Borrowed(#name)
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    fn expand(input: DeriveInput) -> String {
        match derive(&input) {
            Ok(tokens) => tokens.to_string(),
            Err(e) => format!("error: {e}"),
        }
    }

    #[test]
    fn channels_and_names_expand() {
        let out = expand(parse_quote! {
            #[broadcast(private = "orders.{order_id}", public = "orders")]
            #[broadcast(as = "order.shipped")]
            struct OrderShipped { order_id: i64 }
        });
        assert!(out.contains("Channel :: private"), "{out}");
        let presence = expand(parse_quote! {
            #[broadcast(presence = "rooms.{room}")]
            struct Moved { room: i64 }
        });
        assert!(presence.contains("Channel :: presence"), "{presence}");
        assert!(out.contains("\"orders.{}\""), "{out}");
        assert!(out.contains("self . order_id"), "{out}");
        assert!(out.contains("\"order.shipped\""), "{out}");
        let default = expand(parse_quote! {
            #[broadcast(public = "news")]
            struct Posted { id: i64 }
        });
        assert!(default.contains(r#""App\\Events\\Posted""#), "{default}");
    }

    #[test]
    fn mistakes_are_compile_errors() {
        let cases: Vec<(DeriveInput, &str)> = vec![
            (parse_quote! { struct A { x: i64 } }, "missing a channel"),
            (
                parse_quote! { #[broadcast(private = "o.{y}")] struct A { x: i64 } },
                "no field `y`",
            ),
            (
                parse_quote! { #[broadcast(private = "o.{x")] struct A { x: i64 } },
                "without its `}`",
            ),
            (
                parse_quote! { #[broadcast(private = "o/x")] struct A { x: i64 } },
                "cannot be in a channel name",
            ),
            (
                parse_quote! { #[broadcast(secret = "o")] struct A { x: i64 } },
                "expected `public",
            ),
            (
                parse_quote! { #[broadcast(public = "o", as = "a", as = "b")] struct A { x: i64 } },
                "twice",
            ),
            (
                parse_quote! { #[broadcast(public = "o")] struct A(i64); },
                "named fields",
            ),
            (
                parse_quote! { #[broadcast(public = "")] struct A { x: i64 } },
                "empty",
            ),
        ];
        for (input, expected) in cases {
            let out = expand(input);
            assert!(out.contains(expected), "{expected}: {out}");
        }
    }
}
