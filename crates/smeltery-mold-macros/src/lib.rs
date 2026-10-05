//! `#[derive(Mold)]`: compiles a Mold template (`resources/views/<name>.mold.html`) to Rust at build time.
//!
//! ```
//! use smeltery::mold::Template;
//! # #[derive(serde::Serialize)]
//! # struct Post {
//! #     title: String,
//! # }
//!
//! #[derive(smeltery::Mold)]
//! #[mold("posts/index")]
//! struct PostsIndex {
//!     title: String,
//!     posts: Vec<Post>,
//! }
//! # fn main() {}
//! ```
//!
//! The attribute takes the template name and, optionally, `crate = "path"` (the path of the Mold crate, default
//! `::smeltery::mold`) and `dir = "path"` (the views directory relative to `CARGO_MANIFEST_DIR`, default
//! `resources/views`). The derive implements `Template`: `render_runtime` renders through the runtime engine with
//! the struct's fields converted by `to_value` (each field must implement `serde::Serialize`), `render_compiled`
//! runs code generated from the same resolved template, and `render_runtime_with` renders with a given engine. With
//! the default crate path the derive also implements `smeltery::http::IntoResponse` (through
//! `smeltery::view::view`), so a handler can return the struct. Template errors and template variables without a matching
//! field are compile errors naming the `.mold.html` file, line and column.
#![cfg_attr(docsrs, feature(doc_cfg))]

// The README's sample runs as a doctest without becoming the crate docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

use proc_macro::TokenStream;
use proc_macro2::{Literal, TokenStream as Ts};
use quote::{format_ident, quote};
use smeltery_mold::ast::{BinOp, Expr, ExprKind, Filter, Func, Lit, Node, Resolved, Span, UnOp};
use std::path::{Path, PathBuf};
use syn::ext::IdentExt;
use syn::parse::{Parse, ParseStream};
use syn::{DeriveInput, Ident, LitStr, Token};

/// Derives `Template` for a struct with named fields from the template named in `#[mold("…")]`.
#[proc_macro_derive(Mold, attributes(mold))]
pub fn derive_mold(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Implements `Template` for the struct it is given, exactly like `#[derive(Mold)]` (the struct itself is not
/// emitted). Other derives use it to give their struct a view: `#[derive(Spark)]` expands to
/// `template! { #[mold("…", crate = "…")] struct … }`.
#[doc(hidden)]
#[proc_macro]
pub fn template(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

struct Args {
    name: LitStr,
    krate: Option<syn::Path>,
    dir: Option<String>,
}

impl Parse for Args {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let name: LitStr = input.parse()?;
        let mut args = Args {
            name,
            krate: None,
            dir: None,
        };
        while !input.is_empty() {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let key = Ident::parse_any(input)?;
            input.parse::<Token![=]>()?;
            let value: LitStr = input.parse()?;
            match key.to_string().as_str() {
                "crate" => args.krate = Some(value.parse()?),
                "dir" => args.dir = Some(value.value()),
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        "expected `crate = \"…\"` or `dir = \"…\"`",
                    ));
                }
            }
        }
        Ok(args)
    }
}

fn expand(input: &DeriveInput) -> syn::Result<Ts> {
    let attr = input
        .attrs
        .iter()
        .find(|a| a.path().is_ident("mold"))
        .ok_or_else(|| {
            syn::Error::new_spanned(&input.ident, "missing `#[mold(\"template/name\")]`")
        })?;
    let args: Args = attr.parse_args()?;
    let syn::Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "`#[derive(Mold)]` needs a struct with named fields",
        ));
    };
    let syn::Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "`#[derive(Mold)]` needs a struct with named fields",
        ));
    };
    let fields: Vec<Ident> = named.named.iter().filter_map(|f| f.ident.clone()).collect();

    let krate = args
        .krate
        .as_ref()
        .map_or_else(|| quote!(::smeltery::mold), |p| quote!(#p));
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| syn::Error::new(args.name.span(), "CARGO_MANIFEST_DIR is not set"))?;
    let rel = args
        .dir
        .clone()
        .unwrap_or_else(|| "resources/views".to_owned());
    let views = PathBuf::from(&manifest).join(&rel);
    let name = args.name.value();
    let resolved = smeltery_mold::ast::resolve_dir(&views, Path::new(&rel), &name)
        .map_err(|e| syn::Error::new(args.name.span(), e.to_string()))?;

    let mut g = Gen {
        k: krate.clone(),
        resolved: &resolved,
        fields: &fields,
        scopes: Vec::new(),
        counter: 0,
        lit_span: args.name.span(),
        type_name: input.ident.to_string(),
    };
    let body = g.nodes(&resolved.nodes)?;

    let srcs = resolved.files.iter().enumerate().map(|(i, f)| {
        let c = format_ident!("__MOLD_SRC_{}", i);
        // `f.path` is absolute (under CARGO_MANIFEST_DIR); not canonicalized, which would add `\\?\` on Windows.
        let path = f.path.display().to_string();
        quote!(const #c: &str = ::core::include_str!(#path);)
    });
    let to_values = fields.iter().map(|f| {
        let key = f.unraw().to_string();
        quote!((::std::string::String::from(#key), #krate::to_value(&self.#f)?))
    });
    let engine = match &args.dir {
        None => quote!(#krate::Engine::global()),
        Some(dir) => quote!({
            static __MOLD_ENGINE: ::std::sync::OnceLock<#krate::Engine> = ::std::sync::OnceLock::new();
            __MOLD_ENGINE.get_or_init(|| {
                #krate::Engine::new(::std::path::Path::new(::core::env!("CARGO_MANIFEST_DIR")).join(#dir))
            })
        }),
    };
    let cap = resolved
        .nodes
        .iter()
        .map(|n| if let Node::Text(t) = n { t.len() } else { 16 })
        .sum::<usize>();

    let ident = &input.ident;
    let (impl_g, ty_g, where_c) = input.generics.split_for_impl();
    // With the default crate path the struct is also a response: handlers return it and the view middleware
    // renders it (`::smeltery::view::view`).
    let response = if args.krate.is_none() {
        let preds = where_c
            .map(|w| w.predicates.iter().collect::<Vec<_>>())
            .unwrap_or_default();
        quote! {
            #[automatically_derived]
            impl #impl_g ::smeltery::http::IntoResponse for #ident #ty_g
            where
                #(#preds,)*
                Self: ::core::marker::Send + 'static,
            {
                fn into_response(self) -> ::smeltery::Response {
                    ::smeltery::view::view(self)
                }
            }
        }
    } else {
        Ts::new()
    };
    Ok(quote! {
        #response

        #[automatically_derived]
        #[allow(unused_mut, unused_variables, unused_braces, clippy::all, clippy::pedantic, clippy::nursery)]
        impl #impl_g #krate::Template for #ident #ty_g #where_c {
            const NAME: &'static str = #name;

            fn render_runtime(
                &self,
                __host: &dyn #krate::Host,
            ) -> ::core::result::Result<::std::string::String, #krate::Error> {
                #krate::Template::render_runtime_with(self, #engine, __host)
            }

            fn render_runtime_with(
                &self,
                __engine: &#krate::Engine,
                __host: &dyn #krate::Host,
            ) -> ::core::result::Result<::std::string::String, #krate::Error> {
                let __data = #krate::Value::Map(::std::vec![#(#to_values),*]);
                __engine.render(#name, &__data, __host)
            }

            fn render_compiled(
                &self,
                __host: &dyn #krate::Host,
            ) -> ::core::result::Result<::std::string::String, #krate::Error> {
                #[allow(unused_imports)]
                use #krate::rt::{ViaAny as _, ViaOption as _, ViaTruthy as _};
                #(#srcs)*
                let mut __out = ::std::string::String::with_capacity(#cap);
                #body
                ::core::result::Result::Ok(__out)
            }
        }
    })
}

struct Frame {
    isolated: bool,
    names: Vec<String>,
}

struct Gen<'a> {
    k: Ts,
    resolved: &'a Resolved,
    fields: &'a [Ident],
    scopes: Vec<Frame>,
    counter: usize,
    lit_span: proc_macro2::Span,
    type_name: String,
}

const NOT_RAW: &[&str] = &["self", "Self", "super", "crate", "_"];

impl Gen<'_> {
    fn fail(&self, span: &Span, msg: impl Into<String>) -> syn::Error {
        syn::Error::new(self.lit_span, self.resolved.error(span, msg).to_string())
    }

    /// `|m| Error` at `span`, for `.map_err`.
    fn err(&self, span: &Span) -> Ts {
        let k = &self.k;
        let idx = self
            .resolved
            .files
            .iter()
            .position(|f| f.display == span.file)
            .unwrap_or(0);
        let src = format_ident!("__MOLD_SRC_{}", idx);
        let file = &*span.file;
        let (line, col) = (span.line, span.col);
        quote!(|__m| #k::rt::error_at(#file, #src, #line, #col, __m))
    }

    fn fresh(&mut self, prefix: &str) -> Ident {
        self.counter += 1;
        format_ident!("__{}{}", prefix, self.counter)
    }

    fn scoped<T>(
        &mut self,
        isolated: bool,
        names: Vec<String>,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.scopes.push(Frame { isolated, names });
        let r = f(self);
        self.scopes.pop();
        r
    }

    fn nodes(&mut self, nodes: &[Node]) -> syn::Result<Ts> {
        let mut out = Ts::new();
        for n in nodes {
            out.extend(self.node(n)?);
        }
        Ok(out)
    }

    fn node(&mut self, node: &Node) -> syn::Result<Ts> {
        let k = self.k.clone();
        Ok(match node {
            Node::Text(t) => quote!(__out.push_str(#t);),
            Node::Echo { expr, escape } => {
                let e = self.expr(expr)?;
                quote!(#k::rt::Render::render(&(#e), &mut __out, #escape);)
            }
            Node::If {
                branches,
                otherwise,
            } => {
                let mut out = Ts::new();
                for (i, (cond, body)) in branches.iter().enumerate() {
                    let c = self.truthy(cond)?;
                    let b = self.nodes(body)?;
                    if i > 0 {
                        out.extend(quote!(else));
                    }
                    out.extend(quote!(if #c { #b }));
                }
                if !otherwise.is_empty() {
                    let b = self.nodes(otherwise)?;
                    out.extend(quote!(else { #b }));
                }
                out
            }
            Node::For {
                key,
                value,
                expr,
                body,
                empty,
            } => {
                let e = self.expr(expr)?;
                let empty = self.nodes(empty)?;
                let mut names = vec![value.clone(), "loop".to_owned()];
                names.extend(key.clone());
                let v = format_ident!("__v_{}", value);
                let bind_key = key.as_ref().map(|key| {
                    let kv = format_ident!("__v_{}", key);
                    quote!(let #kv = &__k;)
                });
                let body = self.scoped(false, names, |g| g.nodes(body))?;
                quote! {{
                    let __it = &(#e);
                    let __count = #k::rt::Iterate::mold_count(__it);
                    if __count == 0 {
                        #empty
                    } else {
                        for (__i, (__k, __val)) in #k::rt::Iterate::mold_iter(__it).enumerate() {
                            let __v_loop = &#k::rt::Loop::new(__i, __count);
                            let #v = __val;
                            #bind_key
                            #body
                        }
                    }
                }}
            }
            Node::Scope {
                isolated,
                slots,
                vars,
                body,
            } => {
                let mut pre = Ts::new();
                let mut bind = Ts::new();
                let mut names = Vec::new();
                for (name, slot) in slots {
                    let tmp = self.fresh("s");
                    let v = format_ident!("__v_{}", name);
                    if slot.is_empty() {
                        pre.extend(quote!(let #tmp = #k::rt::Safe(::std::string::String::new());));
                    } else {
                        let b = self.nodes(slot)?;
                        pre.extend(quote! {
                            let #tmp = {
                                let mut __out = ::std::string::String::new();
                                #b
                                #k::rt::Safe(__out)
                            };
                        });
                    }
                    bind.extend(quote!(let #v = &#tmp;));
                    names.push(name.clone());
                }
                for (name, e) in vars {
                    let tmp = self.fresh("t");
                    let v = format_ident!("__v_{}", name);
                    let e = self.expr(e)?;
                    pre.extend(quote!(let #tmp = &(#e);));
                    bind.extend(quote!(let #v = #tmp;));
                    names.push(name.clone());
                }
                let body = self.scoped(*isolated, names, |g| g.nodes(body))?;
                quote!({ #pre #bind #body })
            }
            Node::Csrf(span) => {
                let err = self.err(span);
                quote!(__out.push_str(&#k::rt::csrf_field(__host).map_err(#err)?);)
            }
            Node::Error { field, body } => {
                let body = self.scoped(false, vec!["message".to_owned()], |g| g.nodes(body))?;
                quote! {
                    if let ::core::option::Option::Some(__first) = __host.errors(#field).first() {
                        let __v_message = __first;
                        #body
                    }
                }
            }
            Node::Auth {
                guest,
                body,
                otherwise,
            } => {
                let b = self.nodes(body)?;
                let o = self.nodes(otherwise)?;
                let cond = if *guest {
                    quote!(!__host.authenticated())
                } else {
                    quote!(__host.authenticated())
                };
                quote!(if #cond { #b } else { #o })
            }
            Node::Spark { name, props, span } => {
                let err = self.err(span);
                let mut entries = Vec::new();
                for (key, e) in props {
                    let ex = self.expr(e)?;
                    let perr = self.err(&e.span);
                    entries.push(quote! {
                        (::std::string::String::from(#key),
                         #k::to_value(&(#ex)).map_err(|__e| ::std::string::ToString::to_string(&__e)).map_err(#perr)?)
                    });
                }
                quote! {
                    __out.push_str(&__host.spark(#name, &#k::Value::Map(::std::vec![#(#entries),*])).map_err(#err)?);
                }
            }
            Node::SparksScripts => quote!(__out.push_str(&__host.sparks_scripts());),
            Node::Alloy { id, span } => {
                let err = self.err(span);
                quote!(__out.push_str(&__host.alloy_page(#id).map_err(#err)?);)
            }
            Node::AlloyHead => quote!(__out.push_str(&__host.alloy_head());),
            Node::Vite { entries, span } => {
                let err = self.err(span);
                quote!(__out.push_str(&__host.vite(&[#(#entries),*]).map_err(#err)?);)
            }
        })
    }

    fn truthy(&mut self, e: &Expr) -> syn::Result<Ts> {
        let k = self.k.clone();
        let ex = self.expr(e)?;
        Ok(quote!((&&#k::rt::Probe(&(#ex))).mold_truthy()))
    }

    fn scalar(&mut self, e: &Expr) -> syn::Result<Ts> {
        let k = self.k.clone();
        let ex = self.expr(e)?;
        Ok(quote!(#k::rt::ToScalar::to_scalar(&(#ex))))
    }

    fn display(&mut self, e: &Expr) -> syn::Result<Ts> {
        let k = self.k.clone();
        let ex = self.expr(e)?;
        Ok(quote!(#k::rt::display(&(#ex))))
    }

    fn field_ident(&self, name: &str, span: &Span) -> syn::Result<Ident> {
        if NOT_RAW.contains(&name) {
            return Err(self.fail(span, format!("`{name}` cannot be used as a field name")));
        }
        Ok(syn::parse_str::<Ident>(name)
            .unwrap_or_else(|_| Ident::new_raw(name, proc_macro2::Span::call_site())))
    }

    fn var(&self, name: &str, span: &Span) -> syn::Result<Ts> {
        for frame in self.scopes.iter().rev() {
            if frame.names.iter().any(|n| n == name) {
                let v = format_ident!("__v_{}", name);
                return Ok(quote!((*#v)));
            }
            if frame.isolated {
                return Err(self.fail(span, format!("unknown variable `{name}`")));
            }
        }
        match self.fields.iter().find(|f| f.unraw() == name) {
            Some(f) => Ok(quote!(self.#f)),
            None => Err(self.fail(
                span,
                format!(
                    "unknown variable `{name}` (`{}` has no field `{name}`)",
                    self.type_name
                ),
            )),
        }
    }

    fn expr(&mut self, e: &Expr) -> syn::Result<Ts> {
        let k = self.k.clone();
        Ok(match &e.kind {
            ExprKind::Lit(lit) => match lit {
                Lit::Null => quote!(#k::rt::Null),
                Lit::Bool(b) => quote!(#b),
                Lit::Int(i) => {
                    let l = Literal::i64_suffixed(*i);
                    quote!(#l)
                }
                Lit::Float(f) => {
                    if !f.is_finite() {
                        return Err(self.fail(&e.span, "float literal is out of range"));
                    }
                    let l = Literal::f64_suffixed(*f);
                    quote!(#l)
                }
                Lit::Str(s) => quote!(#s),
            },
            ExprKind::Var(name) => self.var(name, &e.span)?,
            ExprKind::Field(inner, name) => {
                let i = self.expr(inner)?;
                let f = self.field_ident(name, &e.span)?;
                quote!((#i).#f)
            }
            ExprKind::Index(inner, idx) => {
                let i = self.expr(inner)?;
                let key = self.scalar(idx)?;
                let err = self.err(&e.span);
                quote!((*#k::rt::Indexable::mold_index(&(#i), &#key).map_err(#err)?))
            }
            ExprKind::Unary(UnOp::Not, inner) => {
                let t = self.truthy(inner)?;
                quote!((!#t))
            }
            ExprKind::Unary(UnOp::Neg, inner) => {
                let s = self.scalar(inner)?;
                let err = self.err(&e.span);
                quote!(#k::rt::neg(&#s).map_err(#err)?)
            }
            ExprKind::Binary(op @ (BinOp::And | BinOp::Or), a, b) => {
                let (ta, tb) = (self.truthy(a)?, self.truthy(b)?);
                if *op == BinOp::And {
                    quote!((#ta && #tb))
                } else {
                    quote!((#ta || #tb))
                }
            }
            ExprKind::Binary(op, a, b) => {
                let (sa, sb) = (self.scalar(a)?, self.scalar(b)?);
                let err = self.err(&e.span);
                let op_ts = binop(&k, *op);
                if op.is_comparison() {
                    quote!(#k::rt::compare(#op_ts, &#sa, &#sb).map_err(#err)?)
                } else {
                    quote!(#k::rt::arith(#op_ts, &#sa, &#sb).map_err(#err)?)
                }
            }
            ExprKind::Filter(filter, inner, args) => match filter {
                Filter::Upper | Filter::Lower | Filter::Trim | Filter::Title => {
                    let d = self.display(inner)?;
                    let f = match filter {
                        Filter::Upper => quote!(upper),
                        Filter::Lower => quote!(lower),
                        Filter::Trim => quote!(trim),
                        _ => quote!(title),
                    };
                    quote!(#k::rt::#f(&#d))
                }
                Filter::Len => {
                    let i = self.expr(inner)?;
                    let err = self.err(&e.span);
                    quote!(#k::rt::Len::mold_len(&(#i)).map_err(#err)?)
                }
                Filter::Default => {
                    let a = self.scalar(inner)?;
                    let b = self.scalar(arg(args, 0, e, self)?)?;
                    quote!(#k::rt::default(#a, #b))
                }
                Filter::Join => {
                    let i = self.expr(inner)?;
                    let sep = self.display(arg(args, 0, e, self)?)?;
                    quote!(#k::rt::join(&(#i), &#sep))
                }
                Filter::Json => {
                    let i = self.expr(inner)?;
                    let err = self.err(&e.span);
                    quote!(#k::rt::json(&(#i)).map_err(#err)?)
                }
                Filter::Url => {
                    let d = self.display(inner)?;
                    quote!(#k::rt::url(&#d))
                }
            },
            ExprKind::Call(func, args, params) => {
                let err = self.err(&e.span);
                match func {
                    Func::Old => {
                        let f = self.display(arg(args, 0, e, self)?)?;
                        quote!(__host.old(&#f).unwrap_or(""))
                    }
                    Func::Session => {
                        let f = self.display(arg(args, 0, e, self)?)?;
                        quote!(__host.session(&#f).unwrap_or_default())
                    }
                    Func::CsrfToken => quote!(#k::rt::csrf_token(__host).map_err(#err)?),
                    Func::Route => {
                        let name = self.display(arg(args, 0, e, self)?)?;
                        let mut ps = Vec::new();
                        for (key, pe) in params {
                            let d = self.display(pe)?;
                            ps.push(quote!((::std::string::String::from(#key), #d)));
                        }
                        quote!(__host.route(&#name, &[#(#ps),*]).map_err(#err)?)
                    }
                }
            }
        })
    }
}

fn arg<'e>(args: &'e [Expr], i: usize, e: &Expr, g: &Gen<'_>) -> syn::Result<&'e Expr> {
    args.get(i)
        .ok_or_else(|| g.fail(&e.span, "missing argument"))
}

fn binop(k: &Ts, op: BinOp) -> Ts {
    let v = match op {
        BinOp::Or => quote!(Or),
        BinOp::And => quote!(And),
        BinOp::Eq => quote!(Eq),
        BinOp::Ne => quote!(Ne),
        BinOp::Lt => quote!(Lt),
        BinOp::Le => quote!(Le),
        BinOp::Gt => quote!(Gt),
        BinOp::Ge => quote!(Ge),
        BinOp::Add => quote!(Add),
        BinOp::Sub => quote!(Sub),
        BinOp::Mul => quote!(Mul),
        BinOp::Div => quote!(Div),
        BinOp::Rem => quote!(Rem),
    };
    quote!(#k::ast::BinOp::#v)
}
