//! The resolved template tree shared by the interpreter and `#[derive(Mold)]`'s code generator.
//!
//! Not a stable API: it exists so `smeltery-mold-macros` can generate code from exactly the tree the interpreter
//! walks, which is how both modes stay byte-identical.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

/// A position in a `.mold.html` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// The file as displayed in errors.
    pub file: Arc<str>,
    /// 1-based line.
    pub line: u32,
    /// 1-based column in characters.
    pub col: u32,
}

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// `||`
    Or,
    /// `&&`
    And,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
}

impl BinOp {
    /// The operator as written.
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Or => "||",
            BinOp::And => "&&",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
        }
    }

    /// `== != < <= > >=`
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }
}

/// A unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// `!`
    Not,
    /// `-`
    Neg,
}

/// A literal.
#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    /// `null`
    Null,
    /// `true` / `false`
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A float.
    Float(f64),
    /// A string.
    Str(String),
}

/// A filter (`expr | name(args)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    /// `upper`
    Upper,
    /// `lower`
    Lower,
    /// `trim`
    Trim,
    /// `title`
    Title,
    /// `len`
    Len,
    /// `default(x)`
    Default,
    /// `join(sep)`
    Join,
    /// `json`: the value as a JavaScript literal, safe inside HTML ([`crate::rt::json`]).
    Json,
    /// `url`: the text when it is a relative or `http`/`https`/`mailto`/`tel` URL, `#` otherwise
    /// ([`crate::rt::url`]).
    Url,
}

impl Filter {
    pub(crate) fn from_name(name: &str) -> Option<(Filter, usize)> {
        Some(match name {
            "upper" => (Filter::Upper, 0),
            "lower" => (Filter::Lower, 0),
            "trim" => (Filter::Trim, 0),
            "title" => (Filter::Title, 0),
            "len" => (Filter::Len, 0),
            "default" => (Filter::Default, 1),
            "join" => (Filter::Join, 1),
            "json" => (Filter::Json, 0),
            "url" => (Filter::Url, 0),
            _ => return None,
        })
    }
}

/// A built-in function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Func {
    /// `old("field")`
    Old,
    /// `route("name", { param: expr })`
    Route,
    /// `csrf_token()`
    CsrfToken,
    /// `session("key")`
    Session,
}

/// An expression with its position.
#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    /// What it is.
    pub kind: ExprKind,
    /// Where it starts.
    pub span: Span,
}

/// The expression kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    /// A literal.
    Lit(Lit),
    /// A variable.
    Var(String),
    /// `a.b`
    Field(Box<Expr>, String),
    /// `a[b]`
    Index(Box<Expr>, Box<Expr>),
    /// `!a`, `-a`
    Unary(UnOp, Box<Expr>),
    /// `a op b`
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `a | filter(args)`
    Filter(Filter, Box<Expr>, Vec<Expr>),
    /// `old(..)`, `session(..)`, `csrf_token()`; `route(name)` with its parameters in `params`.
    Call(Func, Vec<Expr>, Vec<(String, Expr)>),
}

/// A node of the resolved (flat) tree: layouts, includes and components are already inlined.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// Literal output.
    Text(String),
    /// `{{ expr }}` (escape) or `{!! expr !!}`.
    Echo {
        /// The value.
        expr: Expr,
        /// Whether to HTML-escape.
        escape: bool,
    },
    /// `@if` / `@elseif` / `@else` (and `@unless`).
    If {
        /// Conditions with their bodies, in order.
        branches: Vec<(Expr, Vec<Node>)>,
        /// The `@else` body.
        otherwise: Vec<Node>,
    },
    /// `@for`.
    For {
        /// The key variable of `@for(k, v in …)`.
        key: Option<String>,
        /// The item variable.
        value: String,
        /// What is iterated.
        expr: Expr,
        /// The body.
        body: Vec<Node>,
        /// The `@empty` body.
        empty: Vec<Node>,
    },
    /// An inlined `@include` (`isolated == false`) or component (`isolated == true`, the body sees only `vars` and
    /// `slots`). `slots` and `vars` are evaluated in the outer scope, before the body.
    Scope {
        /// Whether outer variables are hidden from the body.
        isolated: bool,
        /// Slot bodies rendered (in the outer scope) into `Safe` variables.
        slots: Vec<(String, Vec<Node>)>,
        /// Variables bound for the body.
        vars: Vec<(String, Expr)>,
        /// The inlined template.
        body: Vec<Node>,
    },
    /// `@csrf`.
    Csrf(Span),
    /// `@error("field")`: the body runs with `message` bound when the field has errors.
    Error {
        /// The field name.
        field: String,
        /// The body.
        body: Vec<Node>,
    },
    /// `@auth` (`guest == false`) or `@guest`.
    Auth {
        /// Whether this is `@guest`.
        guest: bool,
        /// The body.
        body: Vec<Node>,
        /// The `@else` body.
        otherwise: Vec<Node>,
    },
    /// `@spark("name", { props })`.
    Spark {
        /// The component name.
        name: String,
        /// The props.
        props: Vec<(String, Expr)>,
        /// The directive position.
        span: Span,
    },
    /// `@sparksScripts`.
    SparksScripts,
    /// `@alloy` / `@alloy("id")`: the Alloy (Inertia) page element, rendered by [`crate::Host::alloy_page`].
    Alloy {
        /// The root element id (`app` for a bare `@alloy`).
        id: String,
        /// The directive position.
        span: Span,
    },
    /// `@alloyHead`: the head tags of the Alloy page ([`crate::Host::alloy_head`]).
    AlloyHead,
    /// `@vite` / `@vite("entry", …)`: the Vite asset tags ([`crate::Host::vite`]).
    Vite {
        /// The entries; empty for a bare `@vite` (the host's configured entries).
        entries: Vec<String>,
        /// The directive position.
        span: Span,
    },
}

/// A file a resolved template was built from.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// The template name (`posts/index`).
    pub name: String,
    /// The file as displayed in errors.
    pub display: Arc<str>,
    /// The file on disk.
    pub path: PathBuf,
    /// Its contents.
    pub text: Arc<str>,
    /// Its modification time when read.
    pub mtime: Option<SystemTime>,
}

/// A template after parsing and resolving: one flat tree plus every file it came from.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The tree.
    pub nodes: Vec<Node>,
    /// The files read, the entry template first.
    pub files: Vec<SourceFile>,
}

impl Resolved {
    /// The text of the file shown as `display`, for error excerpts.
    pub fn source(&self, display: &str) -> &str {
        self.files
            .iter()
            .find(|f| &*f.display == display)
            .map_or("", |f| &f.text)
    }

    /// An error at `span`, with an excerpt.
    pub fn error(&self, span: &Span, message: impl Into<String>) -> crate::Error {
        crate::Error::at(
            &span.file,
            self.source(&span.file),
            span.line,
            span.col,
            message,
        )
    }
}

pub use crate::resolve::resolve_dir;
