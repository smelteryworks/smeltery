#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]

#[doc(hidden)]
pub mod ast;
mod engine;
mod error;
mod interp;
mod parse;
mod resolve;
#[doc(hidden)]
pub mod rt;
mod value;

pub use engine::{Engine, Host, NoHost, Template, set_global_views_dir, views_dir};
pub use error::Error;
/// Every Mold directive name (`if`, `endfor`, `sparksScripts`, …): for tools that check templates (the CLI's template
/// tests), not part of the public API.
#[doc(hidden)]
pub use parse::DIRECTIVES;
pub use value::{Value, ValueError, to_value};
