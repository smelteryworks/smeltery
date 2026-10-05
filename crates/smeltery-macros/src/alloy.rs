//! `#[derive(Alloy)]`: a struct as an Alloy (Inertia) page.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DeriveInput, Fields, LitStr};

const NAMED_FIELDS: &str =
    "`#[derive(Alloy)]` needs a struct with named fields (its fields are the props)";

pub(crate) fn derive(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let fields_ok = matches!(&input.data, Data::Struct(s) if matches!(s.fields, Fields::Named(_)));
    if !fields_ok {
        return Err(syn::Error::new_spanned(&input.ident, NAMED_FIELDS));
    }
    let mut attrs = input.attrs.iter().filter(|a| a.path().is_ident("alloy"));
    let attr = attrs.next().ok_or_else(|| {
        syn::Error::new_spanned(&input.ident, "missing `#[alloy(\"component/name\")]`")
    })?;
    if let Some(second) = attrs.next() {
        return Err(syn::Error::new_spanned(
            second,
            "`#[alloy(…)]` is given twice: one component name per struct",
        ));
    }
    let name: LitStr = attr.parse_args().map_err(|e| {
        syn::Error::new(
            e.span(),
            "expected the component name: `#[alloy(\"posts/index\")]`",
        )
    })?;
    if name.value().trim().is_empty() {
        return Err(syn::Error::new(name.span(), "the component name is empty"));
    }
    let ident = &input.ident;
    let (impl_g, ty_g, where_c) = input.generics.split_for_impl();
    let preds = where_c
        .map(|w| w.predicates.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(quote! {
        #[automatically_derived]
        impl #impl_g ::smeltery::alloy::Component for #ident #ty_g #where_c {
            const NAME: &'static str = #name;
        }

        #[automatically_derived]
        impl #impl_g ::smeltery::http::IntoResponse for #ident #ty_g
        where
            #(#preds,)*
            Self: ::core::marker::Send + 'static,
        {
            fn into_response(self) -> ::smeltery::Response {
                ::smeltery::http::IntoResponse::into_response(
                    ::smeltery::alloy::Component::into_page(self),
                )
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    fn error(input: DeriveInput) -> String {
        match derive(&input) {
            Ok(tokens) => format!("expanded: {tokens}"),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn a_struct_with_named_fields_expands() {
        let tokens = derive(&parse_quote! {
            #[alloy("posts/index")]
            struct PostsIndex<T: Clone> where T: Send { titles: Vec<T> }
        })
        .map(|t| t.to_string())
        .unwrap_or_default();
        assert!(tokens.contains("Component for PostsIndex"), "{tokens}");
        assert!(tokens.contains("\"posts/index\""), "{tokens}");
        assert!(tokens.contains("IntoResponse for PostsIndex"), "{tokens}");
    }

    #[test]
    fn only_structs_with_named_fields() {
        for input in [
            parse_quote! { #[alloy("x")] enum E { A } },
            parse_quote! { #[alloy("x")] struct T(u32); },
            parse_quote! { #[alloy("x")] struct U; },
            parse_quote! { #[alloy("x")] union N { a: u32 } },
        ] {
            assert_eq!(error(input), NAMED_FIELDS);
        }
    }

    #[test]
    fn the_attribute_errors() {
        assert_eq!(
            error(parse_quote! { struct P { a: u32 } }),
            "missing `#[alloy(\"component/name\")]`"
        );
        assert_eq!(
            error(parse_quote! { #[alloy(3)] struct P { a: u32 } }),
            "expected the component name: `#[alloy(\"posts/index\")]`"
        );
        assert_eq!(
            error(parse_quote! { #[alloy(" ")] struct P { a: u32 } }),
            "the component name is empty"
        );
        assert_eq!(
            error(parse_quote! { #[alloy("a")] #[alloy("b")] struct P { a: u32 } }),
            "`#[alloy(…)]` is given twice: one component name per struct"
        );
    }
}
