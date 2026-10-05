//! Lexer and parser: `.mold.html` text to a tree of [`PNode`]s with spans.

use crate::Error;
use crate::ast::{BinOp, Expr, ExprKind, Filter, Func, Lit, Span, UnOp};
use std::sync::Arc;

/// A parsed (not yet resolved) node.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PNode {
    Text(String),
    Echo(Expr, bool),
    If(Vec<(Expr, Vec<PNode>)>, Vec<PNode>),
    For {
        key: Option<String>,
        value: String,
        expr: Expr,
        body: Vec<PNode>,
        empty: Vec<PNode>,
    },
    Extends(String, Span),
    Section(String, Vec<PNode>, Span),
    SectionInline(String, Expr),
    Yield(String, Option<String>),
    Include(String, Vec<(String, Expr)>, Span),
    Slot(String, Vec<PNode>, Span),
    Component {
        name: String,
        props: Vec<(String, Expr)>,
        body: Vec<PNode>,
        span: Span,
    },
    Csrf(Span),
    Method(String),
    ErrorBlock(String, Vec<PNode>),
    Auth {
        guest: bool,
        body: Vec<PNode>,
        otherwise: Vec<PNode>,
    },
    Spark(String, Vec<(String, Expr)>, Span),
    SparksScripts,
    Alloy(String, Span),
    AlloyHead,
    Vite(Vec<String>, Span),
}

/// A directive with its arguments, before its body is parsed.
#[derive(Debug)]
enum Head {
    If(Expr),
    ElseIf(Expr),
    Else,
    EndIf,
    Unless(Expr),
    EndUnless,
    For(Option<String>, String, Expr),
    Empty,
    EndFor,
    Extends(String),
    Section(String, Option<Expr>),
    EndSection,
    Yield(String, Option<String>),
    Include(String, Vec<(String, Expr)>),
    Slot(String),
    EndSlot,
    Csrf,
    Method(String),
    Error(String),
    EndError,
    Auth,
    EndAuth,
    Guest,
    EndGuest,
    Spark(String, Vec<(String, Expr)>),
    SparksScripts,
    Alloy(String),
    AlloyHeadTags,
    Vite(Vec<String>),
}

/// Every directive name (`@if`, `@endfor`, …), for tools that check templates.
pub const DIRECTIVES: &[&str] = &[
    "if",
    "elseif",
    "else",
    "endif",
    "unless",
    "endunless",
    "for",
    "empty",
    "endfor",
    "extends",
    "section",
    "endsection",
    "yield",
    "include",
    "slot",
    "endslot",
    "csrf",
    "method",
    "error",
    "enderror",
    "auth",
    "endauth",
    "guest",
    "endguest",
    "spark",
    "sparksScripts",
    "alloy",
    "alloyHead",
    "vite",
];

/// Directives whose line is dropped when they stand alone on it.
const TRIMMED: &[&str] = &[
    "if",
    "elseif",
    "else",
    "endif",
    "unless",
    "endunless",
    "for",
    "empty",
    "endfor",
    "extends",
    "section",
    "endsection",
    "slot",
    "endslot",
    "error",
    "enderror",
    "auth",
    "endauth",
    "guest",
    "endguest",
];

/// How a block ended.
enum End {
    Eof,
    Close(String, Span),
    Dir(Head, Span),
}

/// Parses a whole template.
pub(crate) fn parse(src: &str, file: &Arc<str>) -> Result<Vec<PNode>, Error> {
    let mut p = Parser::new(src, file.clone());
    let (nodes, end) = p.block()?;
    match end {
        End::Eof => Ok(nodes),
        End::Close(name, span) => Err(p.err(&span, format!("unexpected `</x-{name}>`"))),
        End::Dir(head, span) => Err(p.err(&span, format!("unexpected `@{}`", head_name(&head)))),
    }
}

fn head_name(h: &Head) -> &'static str {
    match h {
        Head::If(_) => "if",
        Head::ElseIf(_) => "elseif",
        Head::Else => "else",
        Head::EndIf => "endif",
        Head::Unless(_) => "unless",
        Head::EndUnless => "endunless",
        Head::For(..) => "for",
        Head::Empty => "empty",
        Head::EndFor => "endfor",
        Head::Extends(_) => "extends",
        Head::Section(..) => "section",
        Head::EndSection => "endsection",
        Head::Yield(..) => "yield",
        Head::Include(..) => "include",
        Head::Slot(_) => "slot",
        Head::EndSlot => "endslot",
        Head::Csrf => "csrf",
        Head::Method(_) => "method",
        Head::Error(_) => "error",
        Head::EndError => "enderror",
        Head::Auth => "auth",
        Head::EndAuth => "endauth",
        Head::Guest => "guest",
        Head::EndGuest => "endguest",
        Head::Spark(..) => "spark",
        Head::SparksScripts => "sparksScripts",
        Head::Alloy(_) => "alloy",
        Head::AlloyHeadTags => "alloyHead",
        Head::Vite(_) => "vite",
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),
    Op(&'static str),
    Eof,
}

const OPS: &[&str] = &[
    "||", "&&", "==", "!=", "<=", ">=", "<", ">", "+", "-", "*", "/", "%", "!", "(", ")", "[", "]",
    "{", "}", ".", ",", ":", "|",
];

pub(crate) struct Parser<'s> {
    src: &'s str,
    pos: usize,
    /// Expressions stop here (end of an attribute value or of a line).
    limit: usize,
    file: Arc<str>,
    line_starts: Vec<usize>,
    /// For each open block, innermost last: the directives that separate its branches (`else`, `elseif`, `empty`).
    separators: Vec<&'static [&'static str]>,
}

impl<'s> Parser<'s> {
    fn new(src: &'s str, file: Arc<str>) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(src.match_indices('\n').map(|(i, _)| i + 1));
        Self {
            src,
            pos: 0,
            limit: src.len(),
            file,
            line_starts,
            separators: Vec::new(),
        }
    }

    fn span(&self, offset: usize) -> Span {
        let line = self.line_starts.partition_point(|&s| s <= offset);
        let start = self
            .line_starts
            .get(line.saturating_sub(1))
            .copied()
            .unwrap_or(0);
        let col = self.src.get(start..offset).map_or(0, |s| s.chars().count());
        Span {
            file: self.file.clone(),
            line: u32::try_from(line).unwrap_or(u32::MAX),
            col: u32::try_from(col + 1).unwrap_or(u32::MAX),
        }
    }

    fn err(&self, span: &Span, msg: impl Into<String>) -> Error {
        Error::at(&span.file, self.src, span.line, span.col, msg)
    }

    fn err_here(&self, msg: impl Into<String>) -> Error {
        self.err(&self.span(self.pos), msg)
    }

    fn rest(&self) -> &'s str {
        self.src.get(self.pos..).unwrap_or("")
    }

    // ---- template level ------------------------------------------------------------------------------------

    fn block(&mut self) -> Result<(Vec<PNode>, End), Error> {
        let mut nodes = Vec::new();
        let mut text = String::new();
        let flush = |text: &mut String, nodes: &mut Vec<PNode>| {
            if !text.is_empty() {
                nodes.push(PNode::Text(std::mem::take(text)));
            }
        };
        loop {
            let rest = self.rest();
            if rest.is_empty() {
                flush(&mut text, &mut nodes);
                return Ok((nodes, End::Eof));
            }
            if rest.starts_with("{{--") {
                let start = self.pos;
                match rest.find("--}}") {
                    Some(i) => self.pos += i + 4,
                    None => return Err(self.err(&self.span(start), "unclosed comment `{{--`")),
                }
            } else if rest.starts_with("{{") || rest.starts_with("{!!") {
                let raw = rest.starts_with("{!!");
                let start = self.pos;
                self.pos += if raw { 3 } else { 2 };
                let expr = self.expr()?;
                self.skip_ws();
                let close = if raw { "!!}" } else { "}}" };
                if !self.rest().starts_with(close) {
                    return Err(self.err_here(format!(
                        "expected `{close}` to close the `{}` at {}:{}",
                        if raw { "{!!" } else { "{{" },
                        self.span(start).line,
                        self.span(start).col
                    )));
                }
                self.pos += close.len();
                flush(&mut text, &mut nodes);
                nodes.push(PNode::Echo(expr, !raw));
            } else if rest.starts_with("@{{") {
                text.push_str("{{");
                self.pos += 3;
            } else if rest.starts_with("@@") {
                text.push('@');
                self.pos += 2;
            } else if let Some(word) = self.glued_separator() {
                let at = self.pos;
                let glued = self
                    .src
                    .get(at.saturating_sub(1)..at + 1 + word.len())
                    .unwrap_or("");
                return Err(self.err(
                    &self.span(at),
                    format!(
                        "`{glued}` is text, not `@{word}`: an `@` right after an ASCII letter, digit or `_` never \
                         starts a directive, so the block would show this branch's text too; put a space before the \
                         `@` (or write `@@{word}` for the text)"
                    ),
                ));
            } else if rest.starts_with('@')
                && !self.after_word_char()
                && self.directive_word().is_some()
            {
                let at = self.pos;
                let word = self.directive_word().unwrap_or_default();
                let span = self.span(at);
                self.pos += 1 + word.len();
                let head = self.head(word, &span)?;
                if TRIMMED.contains(&word)
                    && let Some((line_start, after)) = self.standalone(at)
                {
                    text.truncate(text.len().saturating_sub(at - line_start));
                    self.pos = after;
                }
                flush(&mut text, &mut nodes);
                match self.open(head, span)? {
                    Ok(node) => {
                        if let Some(node) = node {
                            nodes.push(node);
                        }
                    }
                    Err(end) => return Ok((nodes, end)),
                }
            } else if let Some(after) = rest.strip_prefix("</x-") {
                let start = self.pos;
                let len = name_len(after);
                let name = after.get(..len).unwrap_or("").to_owned();
                self.pos += 4 + len;
                self.skip_ws();
                if !self.rest().starts_with('>') {
                    return Err(self.err_here(format!("expected `>` to close `</x-{name}`")));
                }
                self.pos += 1;
                flush(&mut text, &mut nodes);
                return Ok((nodes, End::Close(name, self.span(start))));
            } else if rest
                .strip_prefix("<x-")
                .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_alphanumeric()))
            {
                flush(&mut text, &mut nodes);
                let node = self.component()?;
                nodes.push(node);
            } else {
                let c = rest.chars().next().unwrap_or(' ');
                text.push(c);
                self.pos += c.len_utf8();
            }
        }
    }

    /// Whether the character before `self.pos` is a word character (`[A-Za-z0-9_]`). An `@` there never starts a
    /// directive (D-284), so `me@auth.example` stays text.
    fn after_word_char(&self) -> bool {
        self.src
            .get(..self.pos)
            .and_then(|s| s.chars().next_back())
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    }

    /// At `@`: a separator of the innermost open block (`@else`, `@elseif`, `@empty`) glued to a word character and
    /// followed by a space, a line break, `(` or the end. D-284 makes it text, which would silently merge the block's
    /// branches (the other branch's markup shown to this branch's audience), so it is an error instead. An e-mail
    /// address such as `bob@else.org` (a `.` follows) stays text.
    fn glued_separator(&self) -> Option<&'static str> {
        if !self.rest().starts_with('@') || !self.after_word_char() {
            return None;
        }
        let word = self.directive_word()?;
        if !self.separators.last().is_some_and(|s| s.contains(&word)) {
            return None;
        }
        let after = self.rest().get(1 + word.len()..)?.chars().next();
        after
            .is_none_or(|c| c.is_whitespace() || c == '(')
            .then_some(word)
    }

    /// A block whose branches `separators` separate (empty: none), parsed with them as the innermost open block's.
    fn sub_block(
        &mut self,
        separators: &'static [&'static str],
    ) -> Result<(Vec<PNode>, End), Error> {
        self.separators.push(separators);
        let parsed = self.block();
        self.separators.pop();
        parsed
    }

    /// The directive name at `@`, when it is a Mold directive (any other `@word` stays text).
    fn directive_word(&self) -> Option<&'static str> {
        let rest = self.rest().get(1..)?;
        let len = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let word = rest.get(..len)?;
        DIRECTIVES.iter().copied().find(|d| *d == word)
    }

    /// When the directive starting at `at` (and ending at `self.pos`) is alone on its line: the line start and the
    /// position after the line break.
    fn standalone(&self, at: usize) -> Option<(usize, usize)> {
        let before = self.src.get(..at)?;
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        if !before
            .get(line_start..)?
            .chars()
            .all(|c| c == ' ' || c == '\t')
        {
            return None;
        }
        let rest = self.rest();
        let trimmed = rest.trim_start_matches([' ', '\t']);
        let skipped = rest.len() - trimmed.len();
        let after = if trimmed.is_empty() {
            0
        } else if trimmed.starts_with("\r\n") {
            2
        } else if trimmed.starts_with('\n') {
            1
        } else {
            return None;
        };
        Some((line_start, self.pos + skipped + after))
    }

    fn head(&mut self, word: &str, span: &Span) -> Result<Head, Error> {
        Ok(match word {
            "if" => Head::If(self.paren_expr()?),
            "elseif" => Head::ElseIf(self.paren_expr()?),
            "unless" => Head::Unless(self.paren_expr()?),
            "else" => Head::Else,
            "endif" => Head::EndIf,
            "endunless" => Head::EndUnless,
            "empty" => Head::Empty,
            "endfor" => Head::EndFor,
            "endsection" => Head::EndSection,
            "endslot" => Head::EndSlot,
            "csrf" => Head::Csrf,
            "enderror" => Head::EndError,
            "auth" => Head::Auth,
            "endauth" => Head::EndAuth,
            "guest" => Head::Guest,
            "endguest" => Head::EndGuest,
            "sparksScripts" => Head::SparksScripts,
            "alloyHead" => {
                self.no_args("alloyHead")?;
                Head::AlloyHeadTags
            }
            "alloy" => self.alloy_head()?,
            "vite" => self.vite_head()?,
            "for" => self.for_head(span)?,
            _ => {
                self.open_paren(word)?;
                let name = self.string_arg()?;
                let head = match word {
                    "extends" => Head::Extends(name),
                    "section" => Head::Section(
                        name,
                        if self.comma()? {
                            Some(self.expr()?)
                        } else {
                            None
                        },
                    ),
                    "yield" => Head::Yield(
                        name,
                        if self.comma()? {
                            Some(self.string_arg()?)
                        } else {
                            None
                        },
                    ),
                    "include" => Head::Include(
                        name,
                        if self.comma()? {
                            self.map()?
                        } else {
                            Vec::new()
                        },
                    ),
                    "spark" => Head::Spark(
                        name,
                        if self.comma()? {
                            self.map()?
                        } else {
                            Vec::new()
                        },
                    ),
                    "slot" => Head::Slot(name),
                    "method" => Head::Method(name),
                    _ => Head::Error(name),
                };
                self.close_paren()?;
                head
            }
        })
    }

    fn for_head(&mut self, span: &Span) -> Result<Head, Error> {
        let rest = self.rest();
        let ws = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        let saved_limit = self.limit;
        let paren = self.eat_paren();
        if !paren {
            if ws == 0 {
                return Err(self.err(span, "expected `(` or a space after `@for`"));
            }
            self.limit = self
                .rest()
                .find('\n')
                .map_or(self.src.len(), |i| self.pos + i);
        }
        let first = self.ident("a loop variable")?;
        let (key, value) = if self.eat_op(",")? {
            (Some(first), self.ident("a loop variable")?)
        } else {
            (None, first)
        };
        match self.next()? {
            Tok::Ident(w) if w == "in" => {}
            _ => return Err(self.err_here("expected `in` in `@for`")),
        }
        let expr = self.expr()?;
        if paren {
            self.close_paren()?;
        } else {
            if self.peek()?.0 != Tok::Eof {
                return Err(self.err_here("unexpected text after the `@for` expression"));
            }
            self.pos = self.limit;
            self.limit = saved_limit;
        }
        Ok(Head::For(key, value, expr))
    }

    /// Fails when an argument list follows a directive that takes none (`@alloyHead("x")`).
    fn no_args(&self, word: &str) -> Result<(), Error> {
        let rest = self.rest();
        let trimmed = rest.trim_start_matches([' ', '\t']);
        if trimmed.starts_with('(') {
            let at = self.pos + rest.len() - trimmed.len();
            return Err(self.err(&self.span(at), format!("`@{word}` takes no arguments")));
        }
        Ok(())
    }

    /// `@alloy` or `@alloy("id")` (spaces or tabs may stand before the `(`, as for every directive).
    fn alloy_head(&mut self) -> Result<Head, Error> {
        const ARGS: &str =
            "`@alloy` takes one string literal, the root element id, such as `@alloy(\"app\")`";
        if !self.eat_paren() {
            return Ok(Head::Alloy("app".to_owned()));
        }
        let (tok, start, end) = self.peek()?;
        let Tok::Str(id) = tok else {
            return Err(self.err(&self.span(start), ARGS));
        };
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(self.err(
                &self.span(start),
                format!("invalid `@alloy` id `{id}`: use letters, digits, `-` and `_`"),
            ));
        }
        self.pos = end;
        if let (Tok::Op(","), at, _) = self.peek()? {
            return Err(self.err(&self.span(at), ARGS));
        }
        self.close_paren()?;
        Ok(Head::Alloy(id))
    }

    /// `@vite` (the host's configured entries) or `@vite("a", …)` with one or more entries.
    fn vite_head(&mut self) -> Result<Head, Error> {
        const ARGS: &str = "`@vite` takes one or more non-empty string literals (Vite entries), such as \
                            `@vite(\"resources/js/app.tsx\")`; a bare `@vite` renders the configured entries";
        if !self.eat_paren() {
            return Ok(Head::Vite(Vec::new()));
        }
        let mut entries = Vec::new();
        loop {
            match self.peek()? {
                (Tok::Str(s), _, end) if !s.is_empty() => {
                    entries.push(s);
                    self.pos = end;
                }
                (_, start, _) => return Err(self.err(&self.span(start), ARGS)),
            }
            if !self.comma()? {
                break;
            }
        }
        self.close_paren()?;
        Ok(Head::Vite(entries))
    }

    /// Parses the body of a block directive; `Ok(Err(end))` hands a terminator back to the caller.
    #[allow(clippy::type_complexity)]
    fn open(&mut self, head: Head, span: Span) -> Result<Result<Option<PNode>, End>, Error> {
        let node = match head {
            Head::If(cond) => {
                let mut branches = Vec::new();
                let mut cond = cond;
                loop {
                    let (body, end) = self.sub_block(&["else", "elseif"])?;
                    branches.push((cond, body));
                    match end {
                        End::Dir(Head::ElseIf(c), _) => cond = c,
                        End::Dir(Head::Else, _) => {
                            let (otherwise, end) = self.sub_block(&[])?;
                            self.expect_end(end, "endif", "@if", &span)?;
                            break PNode::If(branches, otherwise);
                        }
                        end => {
                            self.expect_end(end, "endif", "@if", &span)?;
                            break PNode::If(branches, Vec::new());
                        }
                    }
                }
            }
            Head::Unless(cond) => {
                let (body, end) = self.sub_block(&[])?;
                self.expect_end(end, "endunless", "@unless", &span)?;
                let not = Expr {
                    span: cond.span.clone(),
                    kind: ExprKind::Unary(UnOp::Not, Box::new(cond)),
                };
                PNode::If(vec![(not, body)], Vec::new())
            }
            Head::For(key, value, expr) => {
                let (body, end) = self.sub_block(&["empty"])?;
                let empty = if let End::Dir(Head::Empty, _) = end {
                    let (empty, end) = self.sub_block(&[])?;
                    self.expect_end(end, "endfor", "@for", &span)?;
                    empty
                } else {
                    self.expect_end(end, "endfor", "@for", &span)?;
                    Vec::new()
                };
                PNode::For {
                    key,
                    value,
                    expr,
                    body,
                    empty,
                }
            }
            Head::Section(name, Some(expr)) => PNode::SectionInline(name, expr),
            Head::Section(name, None) => {
                let (body, end) = self.sub_block(&[])?;
                self.expect_end(end, "endsection", "@section", &span)?;
                PNode::Section(name, body, span)
            }
            Head::Slot(name) => {
                let (body, end) = self.sub_block(&[])?;
                self.expect_end(end, "endslot", "@slot", &span)?;
                PNode::Slot(name, body, span)
            }
            Head::Error(field) => {
                let (body, end) = self.sub_block(&[])?;
                self.expect_end(end, "enderror", "@error", &span)?;
                PNode::ErrorBlock(field, body)
            }
            Head::Auth | Head::Guest => {
                let guest = matches!(head, Head::Guest);
                let (close, what) = if guest {
                    ("endguest", "@guest")
                } else {
                    ("endauth", "@auth")
                };
                let (body, end) = self.sub_block(&["else"])?;
                let otherwise = if let End::Dir(Head::Else, _) = end {
                    let (otherwise, end) = self.sub_block(&[])?;
                    self.expect_end(end, close, what, &span)?;
                    otherwise
                } else {
                    self.expect_end(end, close, what, &span)?;
                    Vec::new()
                };
                PNode::Auth {
                    guest,
                    body,
                    otherwise,
                }
            }
            Head::Extends(name) => PNode::Extends(name, span),
            Head::Yield(name, default) => PNode::Yield(name, default),
            Head::Include(name, vars) => PNode::Include(name, vars, span),
            Head::Csrf => PNode::Csrf(span),
            Head::Method(m) => PNode::Method(m),
            Head::Spark(name, props) => PNode::Spark(name, props, span),
            Head::SparksScripts => PNode::SparksScripts,
            Head::Alloy(id) => PNode::Alloy(id, span),
            Head::AlloyHeadTags => PNode::AlloyHead,
            Head::Vite(entries) => PNode::Vite(entries, span),
            end @ (Head::ElseIf(_)
            | Head::Else
            | Head::EndIf
            | Head::EndUnless
            | Head::Empty
            | Head::EndFor
            | Head::EndSection
            | Head::EndSlot
            | Head::EndError
            | Head::EndAuth
            | Head::EndGuest) => return Ok(Err(End::Dir(end, span))),
        };
        Ok(Ok(Some(node)))
    }

    fn expect_end(
        &self,
        end: End,
        want: &str,
        opener: &str,
        open_span: &Span,
    ) -> Result<(), Error> {
        if !matches!(&end, End::Dir(h, _) if head_name(h) == want) {
            let to = match &end {
                End::Dir(_, span) | End::Close(_, span) => self.offset(span),
                End::Eof => self.src.len(),
            };
            if let Some(at) = self.glued(self.offset(open_span), to, want) {
                let glued = self
                    .src
                    .get(at.saturating_sub(1)..at + 1 + want.len())
                    .unwrap_or("");
                return Err(self.err(
                    &self.span(at),
                    format!(
                        "`{glued}` is text, not `@{want}`: an `@` right after an ASCII letter, digit or `_` never \
                         starts a directive, so the `{opener}` at line {} is unclosed; put a space before the `@`",
                        open_span.line
                    ),
                ));
            }
        }
        match end {
            End::Dir(h, _) if head_name(&h) == want => Ok(()),
            End::Dir(h, span) => Err(self.err(
                &span,
                format!("unexpected `@{}`, expected `@{want}` to close the `{opener}` at line {}", head_name(&h), open_span.line),
            )),
            End::Close(name, span) => Err(self.err(
                &span,
                format!("unexpected `</x-{name}>`, expected `@{want}` to close the `{opener}` at line {}", open_span.line),
            )),
            End::Eof => Err(self.err(open_span, format!("unclosed `{opener}` (missing `@{want}`)"))),
        }
    }

    /// The byte offset of `span` (a position in this file).
    fn offset(&self, span: &Span) -> usize {
        let line_start = self
            .line_starts
            .get(span.line.saturating_sub(1) as usize)
            .copied()
            .unwrap_or(0);
        let line = self.src.get(line_start..).unwrap_or("");
        let col = span.col.saturating_sub(1) as usize;
        line_start + line.char_indices().nth(col).map_or(line.len(), |(i, _)| i)
    }

    /// The offset of the first `@<want>` in `from..to` glued to a word character (so it is text, D-284), such as
    /// the `@endif` of `yes@endif`.
    fn glued(&self, from: usize, to: usize, want: &str) -> Option<usize> {
        let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let hay = self.src.get(from..to)?;
        let needle = format!("@{want}");
        hay.match_indices(&needle)
            .map(|(i, _)| from + i)
            .find(|&at| {
                let before = self.src.get(..at).and_then(|s| s.chars().next_back());
                let after = self
                    .src
                    .get(at + needle.len()..)
                    .and_then(|s| s.chars().next());
                before.is_some_and(word) && !after.is_some_and(word)
            })
    }

    fn component(&mut self) -> Result<PNode, Error> {
        let start = self.pos;
        let span = self.span(start);
        self.pos += 3;
        let len = name_len(self.rest());
        let tag = self.rest().get(..len).unwrap_or("").to_owned();
        self.pos += len;
        let mut props = Vec::new();
        loop {
            self.skip_ws();
            let rest = self.rest();
            if rest.starts_with("/>") {
                self.pos += 2;
                return Ok(PNode::Component {
                    name: tag.replace('.', "/"),
                    props,
                    body: Vec::new(),
                    span,
                });
            }
            if rest.starts_with('>') {
                self.pos += 1;
                break;
            }
            if rest.is_empty() {
                return Err(self.err(&span, format!("unclosed `<x-{tag}` tag")));
            }
            let expr_attr = rest.starts_with(':');
            if expr_attr {
                self.pos += 1;
            }
            let attr_span = self.span(self.pos);
            let len = self
                .rest()
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(self.rest().len());
            if len == 0 {
                return Err(self.err_here(format!("unexpected character in `<x-{tag}>`")));
            }
            let name = self.rest().get(..len).unwrap_or("").replace('-', "_");
            if !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
                return Err(self.err(&attr_span, format!("invalid attribute name `{name}`")));
            }
            self.pos += len;
            if !self.rest().starts_with('=') {
                if expr_attr {
                    return Err(self.err_here(format!("expected `=\"…\"` after `:{name}`")));
                }
                props.push((
                    name,
                    Expr {
                        kind: ExprKind::Lit(Lit::Bool(true)),
                        span: attr_span,
                    },
                ));
                continue;
            }
            self.pos += 1;
            let quote = match self.rest().chars().next() {
                Some(q @ ('"' | '\'')) => q,
                _ => return Err(self.err_here("expected a quoted attribute value")),
            };
            self.pos += 1;
            let close = self
                .rest()
                .find(quote)
                .map(|i| self.pos + i)
                .ok_or_else(|| self.err(&attr_span, "unclosed attribute value"))?;
            let value = if expr_attr {
                let saved = self.limit;
                self.limit = close;
                let e = self.expr()?;
                if self.peek()?.0 != Tok::Eof {
                    return Err(self.err_here("unexpected text in the attribute expression"));
                }
                self.limit = saved;
                e
            } else {
                let vspan = self.span(self.pos);
                Expr {
                    kind: ExprKind::Lit(Lit::Str(
                        self.src.get(self.pos..close).unwrap_or("").to_owned(),
                    )),
                    span: vspan,
                }
            };
            self.pos = close + 1;
            props.push((name, value));
        }
        let (body, end) = self.sub_block(&[])?;
        match end {
            End::Close(name, _) if name == tag => {}
            End::Close(name, span) => {
                return Err(self.err(&span, format!("expected `</x-{tag}>`, found `</x-{name}>`")));
            }
            End::Dir(h, span) => {
                return Err(self.err(
                    &span,
                    format!("unexpected `@{}` inside `<x-{tag}>`", head_name(&h)),
                ));
            }
            End::Eof => {
                return Err(self.err(
                    &span,
                    format!("unclosed `<x-{tag}>` (missing `</x-{tag}>`)"),
                ));
            }
        }
        Ok(PNode::Component {
            name: tag.replace('.', "/"),
            props,
            body,
            span,
        })
    }

    // ---- directive arguments --------------------------------------------------------------------------------

    fn open_paren(&mut self, word: &str) -> Result<(), Error> {
        if self.eat_paren() {
            Ok(())
        } else {
            Err(self.err_here(format!("expected `(` after `@{word}`")))
        }
    }

    fn close_paren(&mut self) -> Result<(), Error> {
        if self.eat_op(")")? {
            Ok(())
        } else {
            Err(self.err_here("expected `)`"))
        }
    }

    fn paren_expr(&mut self) -> Result<Expr, Error> {
        if !self.eat_paren() {
            return Err(self.err_here("expected `(`"));
        }
        let e = self.expr()?;
        self.close_paren()?;
        Ok(e)
    }

    fn comma(&mut self) -> Result<bool, Error> {
        self.eat_op(",")
    }

    fn string_arg(&mut self) -> Result<String, Error> {
        match self.next()? {
            Tok::Str(s) => Ok(s),
            _ => Err(self.err_here("expected a string literal")),
        }
    }

    fn ident(&mut self, what: &str) -> Result<String, Error> {
        self.skip_ws();
        match self.next()? {
            Tok::Ident(s) if !matches!(s.as_str(), "true" | "false" | "null" | "in") => Ok(s),
            _ => Err(self.err_here(format!("expected {what}"))),
        }
    }

    /// `{ name: expr, … }`
    fn map(&mut self) -> Result<Vec<(String, Expr)>, Error> {
        if !self.eat_op("{")? {
            return Err(self.err_here("expected `{`"));
        }
        let mut out = Vec::new();
        loop {
            if self.eat_op("}")? {
                return Ok(out);
            }
            let key = match self.next()? {
                Tok::Ident(s) | Tok::Str(s) => s,
                _ => return Err(self.err_here("expected a key")),
            };
            if !self.eat_op(":")? {
                return Err(self.err_here("expected `:`"));
            }
            out.push((key, self.expr()?));
            if !self.eat_op(",")? {
                if self.eat_op("}")? {
                    return Ok(out);
                }
                return Err(self.err_here("expected `,` or `}`"));
            }
        }
    }

    // ---- expressions ----------------------------------------------------------------------------------------

    fn skip_ws(&mut self) {
        let bytes = self.src.as_bytes();
        while self.pos < self.limit && bytes.get(self.pos).is_some_and(u8::is_ascii_whitespace) {
            self.pos += 1;
        }
    }

    /// After optional spaces: `(`? Consumes through it.
    fn eat_paren(&mut self) -> bool {
        let rest = self.rest();
        let trimmed = rest.trim_start_matches([' ', '\t']);
        if trimmed.starts_with('(') {
            self.pos += rest.len() - trimmed.len() + 1;
            true
        } else {
            false
        }
    }

    /// The next token and the position after it, without consuming it.
    fn peek(&self) -> Result<(Tok, usize, usize), Error> {
        let bytes = self.src.as_bytes();
        let mut i = self.pos;
        while i < self.limit && bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if i >= self.limit {
            return Ok((Tok::Eof, i, i));
        }
        let start = i;
        let rest = self.src.get(i..self.limit).unwrap_or("");
        let c = rest.chars().next().unwrap_or(' ');
        if c.is_ascii_alphabetic() || c == '_' {
            let len = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let word = rest.get(..len).unwrap_or("");
            return Ok((Tok::Ident(word.to_owned()), start, i + len));
        }
        if c.is_ascii_digit() {
            let digits = |s: &str| s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
            let mut len = digits(rest);
            let mut float = false;
            if let Some(frac) = rest.get(len..).and_then(|r| r.strip_prefix('.'))
                && frac.starts_with(|c: char| c.is_ascii_digit())
            {
                float = true;
                len += 1 + digits(frac);
            }
            let text = rest.get(..len).unwrap_or("");
            let tok = if float {
                Tok::Float(
                    text.parse()
                        .map_err(|_| self.err(&self.span(start), "invalid number"))?,
                )
            } else {
                Tok::Int(
                    text.parse()
                        .map_err(|_| self.err(&self.span(start), "integer literal is too large"))?,
                )
            };
            return Ok((tok, start, i + len));
        }
        if c == '"' || c == '\'' {
            let mut out = String::new();
            let mut chars = rest.char_indices().skip(1);
            while let Some((j, ch)) = chars.next() {
                if ch == c {
                    return Ok((Tok::Str(out), start, i + j + 1));
                }
                if ch == '\\' {
                    match chars.next() {
                        Some((_, 'n')) => out.push('\n'),
                        Some((_, 't')) => out.push('\t'),
                        Some((_, 'r')) => out.push('\r'),
                        Some((_, '0')) => out.push('\0'),
                        Some((_, e @ ('\\' | '"' | '\''))) => out.push(e),
                        _ => return Err(self.err(&self.span(i + j), "invalid escape in string")),
                    }
                } else {
                    out.push(ch);
                }
            }
            return Err(self.err(&self.span(start), "unclosed string literal"));
        }
        for op in OPS {
            if rest.starts_with(op) {
                return Ok((Tok::Op(op), start, i + op.len()));
            }
        }
        Err(self.err(&self.span(start), format!("unexpected character `{c}`")))
    }

    fn next(&mut self) -> Result<Tok, Error> {
        let (tok, start, end) = self.peek()?;
        if tok == Tok::Eof {
            self.pos = start;
        } else {
            self.pos = end;
        }
        Ok(tok)
    }

    fn eat_op(&mut self, op: &str) -> Result<bool, Error> {
        let (tok, _, end) = self.peek()?;
        if matches!(tok, Tok::Op(o) if o == op) {
            self.pos = end;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(crate) fn expr(&mut self) -> Result<Expr, Error> {
        self.binary(0)
    }

    fn binary(&mut self, level: usize) -> Result<Expr, Error> {
        const LEVELS: &[&[(&str, BinOp)]] = &[
            &[("||", BinOp::Or)],
            &[("&&", BinOp::And)],
            &[
                ("==", BinOp::Eq),
                ("!=", BinOp::Ne),
                ("<=", BinOp::Le),
                (">=", BinOp::Ge),
                ("<", BinOp::Lt),
                (">", BinOp::Gt),
            ],
            &[("+", BinOp::Add), ("-", BinOp::Sub)],
            &[("*", BinOp::Mul), ("/", BinOp::Div), ("%", BinOp::Rem)],
        ];
        let Some(ops) = LEVELS.get(level) else {
            return self.unary();
        };
        let mut left = self.binary(level + 1)?;
        loop {
            let (tok, start, end) = self.peek()?;
            let Tok::Op(sym) = tok else { break };
            let Some((_, op)) = ops.iter().find(|(s, _)| *s == sym) else {
                break;
            };
            self.pos = end;
            let right = self.binary(level + 1)?;
            left = Expr {
                span: self.span(start),
                kind: ExprKind::Binary(*op, Box::new(left), Box::new(right)),
            };
            // Comparisons do not chain: `a < b < c` is an error.
            if op.is_comparison() {
                if let (Tok::Op(s), _, _) = self.peek()?
                    && ops.iter().any(|(o, _)| *o == s)
                {
                    return Err(self.err_here("comparisons cannot be chained; use `&&`"));
                }
                break;
            }
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, Error> {
        let (tok, start, end) = self.peek()?;
        let op = match tok {
            Tok::Op("!") => UnOp::Not,
            Tok::Op("-") => UnOp::Neg,
            _ => return self.postfix(),
        };
        self.pos = end;
        let inner = self.unary()?;
        Ok(Expr {
            span: self.span(start),
            kind: ExprKind::Unary(op, Box::new(inner)),
        })
    }

    fn postfix(&mut self) -> Result<Expr, Error> {
        let mut e = self.primary()?;
        loop {
            let (tok, start, end) = self.peek()?;
            match tok {
                Tok::Op(".") => {
                    self.pos = end;
                    let name = match self.next()? {
                        Tok::Ident(n) => n,
                        _ => return Err(self.err_here("expected a field name after `.`")),
                    };
                    e = Expr {
                        span: self.span(start),
                        kind: ExprKind::Field(Box::new(e), name),
                    };
                }
                Tok::Op("[") => {
                    self.pos = end;
                    let idx = self.expr()?;
                    if !self.eat_op("]")? {
                        return Err(self.err_here("expected `]`"));
                    }
                    e = Expr {
                        span: self.span(start),
                        kind: ExprKind::Index(Box::new(e), Box::new(idx)),
                    };
                }
                Tok::Op("|") => {
                    self.pos = end;
                    let fspan = self.span(self.pos);
                    let name = match self.next()? {
                        Tok::Ident(n) => n,
                        _ => return Err(self.err_here("expected a filter name after `|`")),
                    };
                    let (filter, arity) = Filter::from_name(&name).ok_or_else(|| {
                        self.err(
                            &fspan,
                            format!("unknown filter `{name}` (filters: upper, lower, trim, title, len, default, join, json, url)"),
                        )
                    })?;
                    let args = if self.eat_op("(")? {
                        self.args()?
                    } else {
                        Vec::new()
                    };
                    if args.len() != arity {
                        return Err(
                            self.err(&fspan, format!("filter `{name}` takes {arity} argument(s)"))
                        );
                    }
                    e = Expr {
                        span: fspan,
                        kind: ExprKind::Filter(filter, Box::new(e), args),
                    };
                }
                _ => return Ok(e),
            }
        }
    }

    /// Arguments after `(`, through `)`.
    fn args(&mut self) -> Result<Vec<Expr>, Error> {
        let mut args = Vec::new();
        if self.eat_op(")")? {
            return Ok(args);
        }
        loop {
            args.push(self.expr()?);
            if self.eat_op(")")? {
                return Ok(args);
            }
            if !self.eat_op(",")? {
                return Err(self.err_here("expected `,` or `)`"));
            }
        }
    }

    fn primary(&mut self) -> Result<Expr, Error> {
        let (tok, start, end) = self.peek()?;
        let span = self.span(start);
        let kind = match tok {
            Tok::Int(i) => ExprKind::Lit(Lit::Int(i)),
            Tok::Float(f) => ExprKind::Lit(Lit::Float(f)),
            Tok::Str(s) => ExprKind::Lit(Lit::Str(s)),
            Tok::Op("(") => {
                self.pos = end;
                let e = self.expr()?;
                if !self.eat_op(")")? {
                    return Err(self.err_here("expected `)`"));
                }
                return Ok(e);
            }
            Tok::Ident(name) => match name.as_str() {
                "true" => ExprKind::Lit(Lit::Bool(true)),
                "false" => ExprKind::Lit(Lit::Bool(false)),
                "null" => ExprKind::Lit(Lit::Null),
                _ => {
                    self.pos = end;
                    if self.eat_op("(")? {
                        return self.call(name, span);
                    }
                    return Ok(Expr {
                        kind: ExprKind::Var(name),
                        span,
                    });
                }
            },
            Tok::Eof => return Err(self.err(&span, "expected an expression")),
            Tok::Op(op) => {
                return Err(self.err(&span, format!("unexpected `{op}`, expected an expression")));
            }
        };
        self.pos = end;
        Ok(Expr { kind, span })
    }

    fn call(&mut self, name: String, span: Span) -> Result<Expr, Error> {
        let (func, args, params) = match name.as_str() {
            "old" => (Func::Old, self.args()?, Vec::new()),
            "csrf_token" => (Func::CsrfToken, self.args()?, Vec::new()),
            "session" => (Func::Session, self.args()?, Vec::new()),
            "route" => {
                let first = self.expr()?;
                let params = if self.eat_op(",")? {
                    self.map()?
                } else {
                    Vec::new()
                };
                self.close_paren()?;
                (Func::Route, vec![first], params)
            }
            _ => {
                return Err(self.err(
                    &span,
                    format!(
                        "unknown function `{name}` (functions: old, session, route, csrf_token)"
                    ),
                ));
            }
        };
        let want = match func {
            Func::Old => 1,
            Func::CsrfToken => 0,
            Func::Session => 1,
            Func::Route => 1,
        };
        if args.len() != want {
            return Err(self.err(&span, format!("`{name}` takes {want} argument(s)")));
        }
        Ok(Expr {
            kind: ExprKind::Call(func, args, params),
            span,
        })
    }
}

fn name_len(s: &str) -> usize {
    s.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .unwrap_or(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(src: &str) -> Result<Vec<PNode>, Error> {
        parse(src, &Arc::from("t.mold.html"))
    }

    fn text(s: &str) -> PNode {
        PNode::Text(s.to_owned())
    }

    #[test]
    fn text_comments_and_escapes() {
        assert_eq!(
            p("a {{-- x --}}b @{{ c @@ d a@b.com @media").unwrap(),
            vec![text("a b {{ c @ d a@b.com @media")]
        );
    }

    #[test]
    fn echo_and_raw() {
        let nodes = p("{{ a.b[0] | upper }}{!! x !!}").unwrap();
        assert!(
            matches!(&nodes[0], PNode::Echo(e, true) if matches!(e.kind, ExprKind::Filter(Filter::Upper, _, _)))
        );
        assert!(matches!(&nodes[1], PNode::Echo(_, false)));
    }

    #[test]
    fn precedence() {
        let nodes = p("{{ 1 + 2 * 3 == 7 && !x || y }}").unwrap();
        let PNode::Echo(e, _) = &nodes[0] else {
            panic!()
        };
        let ExprKind::Binary(BinOp::Or, l, _) = &e.kind else {
            panic!("{e:?}")
        };
        let ExprKind::Binary(BinOp::And, l, _) = &l.kind else {
            panic!()
        };
        let ExprKind::Binary(BinOp::Eq, l, _) = &l.kind else {
            panic!()
        };
        assert!(matches!(&l.kind, ExprKind::Binary(BinOp::Add, _, _)));
    }

    #[test]
    fn standalone_lines_are_dropped() {
        let nodes = p("<ul>\n  @if(x)\n  <li>\n  @endif\n</ul>").unwrap();
        assert_eq!(nodes[0], text("<ul>\n"));
        let PNode::If(b, _) = &nodes[1] else { panic!() };
        assert_eq!(b[0].1, vec![text("  <li>\n")]);
        assert_eq!(nodes[2], text("</ul>"));
        // Not alone on its line: kept.
        let nodes = p("a @if(x)b @endif c").unwrap();
        assert_eq!(nodes[0], text("a "));
        assert_eq!(nodes[2], text(" c"));
    }

    #[test]
    fn for_forms() {
        let nodes = p("@for x in items\n{{ x }}\n@endfor\n@for(k, v in m)@empty-@endfor").unwrap();
        assert!(matches!(&nodes[0], PNode::For { key: None, value, .. } if value == "x"));
        assert!(
            matches!(&nodes[1], PNode::For { key: Some(k), empty, .. } if k == "k" && empty.len() == 1)
        );
    }

    #[test]
    fn components() {
        let nodes = p("<x-forms.input type=\"text\" :count=\"n + 1\" data-id=\"7\" />").unwrap();
        let PNode::Component { name, props, .. } = &nodes[0] else {
            panic!()
        };
        assert_eq!(name, "forms/input");
        let names: Vec<&str> = props.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["type", "count", "data_id"]);
        let nodes = p("<x-a>x<x-a>y</x-a>@slot(\"t\")T @endslot</x-a>").unwrap();
        let PNode::Component { body, .. } = &nodes[0] else {
            panic!()
        };
        assert_eq!(body.len(), 3);
    }

    #[test]
    fn errors_have_positions() {
        let e = p("line\n  @if(x)\nnever closed").unwrap_err();
        assert_eq!(
            e.to_string(),
            "t.mold.html:2:3: unclosed `@if` (missing `@endif`)"
        );
        let e = p("{{ a +  }}").unwrap_err();
        assert_eq!(
            e.to_string(),
            "t.mold.html:1:9: unexpected `}`, expected an expression"
        );
        let e = p("@endif").unwrap_err();
        assert_eq!(e.to_string(), "t.mold.html:1:1: unexpected `@endif`");
        let e = p("{{ x | shout }}").unwrap_err();
        assert!(e.message.starts_with("unknown filter `shout`"), "{e}");
        let e = p("<x-a>\n</x-b>").unwrap_err();
        assert_eq!(
            e.to_string(),
            "t.mold.html:2:1: expected `</x-a>`, found `</x-b>`"
        );
        let e = p("{{ 1 < 2 < 3 }}").unwrap_err();
        assert!(e.message.contains("chained"));
        let e = p("{{ \"abc }}").unwrap_err();
        assert_eq!(e.message, "unclosed string literal");
    }

    #[test]
    fn directives_with_args() {
        let nodes = p("@include(\"p/nav\", { user: u, title: \"x\" })@yield(\"t\", \"D\")@spark(\"c\", {start: 5})")
            .unwrap();
        assert!(matches!(&nodes[0], PNode::Include(n, v, _) if n == "p/nav" && v.len() == 2));
        assert!(matches!(&nodes[1], PNode::Yield(n, Some(d)) if n == "t" && d == "D"));
        assert!(matches!(&nodes[2], PNode::Spark(n, v, _) if n == "c" && v.len() == 1));
        let nodes =
            p("{{ route(\"posts.show\", { post: p.id }) }}{{ old(\"f\") }}{{ csrf_token() }}")
                .unwrap();
        assert!(
            matches!(&nodes[0], PNode::Echo(Expr { kind: ExprKind::Call(Func::Route, a, p), .. }, _) if a.len() == 1 && p.len() == 1)
        );
    }

    #[test]
    fn alloy_and_vite_forms() {
        let nodes =
            p("@alloy|@alloy(\"root-2_x\")|@alloyHead|@vite|@vite(\"a.tsx\", 'b.css')").unwrap();
        assert!(
            matches!(&nodes[0], PNode::Alloy(id, s) if id == "app" && (s.line, s.col) == (1, 1))
        );
        assert_eq!(nodes[1], text("|"));
        assert!(matches!(&nodes[2], PNode::Alloy(id, s) if id == "root-2_x" && s.col == 8));
        assert_eq!(nodes[4], PNode::AlloyHead);
        assert!(matches!(&nodes[6], PNode::Vite(e, _) if e.is_empty()));
        assert!(matches!(&nodes[8], PNode::Vite(e, _) if e == &["a.tsx", "b.css"]));
        // Spaces or tabs may stand before the argument list, as for every directive; other text after a
        // bare directive stays text; longer words are not directives.
        let nodes =
            p("@vite (\"a.ts\") @alloy	(\"r\") @vite, then @alloy. @alloyHeads @vites").unwrap();
        assert!(matches!(&nodes[0], PNode::Vite(e, _) if e == &["a.ts"]));
        assert_eq!(nodes[1], text(" "));
        assert!(matches!(&nodes[2], PNode::Alloy(id, _) if id == "r"));
        assert!(matches!(&nodes[4], PNode::Vite(e, _) if e.is_empty()));
        assert_eq!(nodes[5], text(", then "));
        assert!(matches!(&nodes[6], PNode::Alloy(id, _) if id == "app"));
        assert_eq!(nodes[7], text(". @alloyHeads @vites"));
        // Not standalone-trimmed: the line break after the directive stays.
        let nodes = p("@vite\n@alloy\n").unwrap();
        assert_eq!(nodes[1], text("\n"));
        assert_eq!(nodes[3], text("\n"));
    }

    #[test]
    fn at_after_a_word_character_is_text() {
        // D-284: letters, digits and `_` before `@` keep it text.
        for src in [
            "me@auth.example",
            "a@if(x)",
            "x@endif",
            "9@csrf",
            "_@vite(1)",
            "first@endif@if(y)",
        ] {
            assert_eq!(p(src).unwrap(), vec![text(src)], "{src}");
        }
        // Any other character (or the start) before `@` still allows a directive.
        let nodes = p("(@csrf)-@csrf\n@csrf}}@csrf").unwrap();
        assert!(matches!(&nodes[1], PNode::Csrf(s) if s.col == 2));
        assert!(matches!(&nodes[3], PNode::Csrf(s) if s.col == 9));
        assert!(matches!(&nodes[5], PNode::Csrf(s) if (s.line, s.col) == (2, 1)));
        assert!(matches!(&nodes[7], PNode::Csrf(s) if (s.line, s.col) == (2, 8)));
        // A non-ASCII letter is not a word character here (an ASCII `\w`).
        assert!(matches!(&p("é@csrf").unwrap()[1], PNode::Csrf(_)));
        // `x@endif` inside a block is text; the error points at it and says why (the block is unclosed).
        let hint = |glued: &str, want: &str, opener: &str| {
            format!(
                "`{glued}` is text, not `@{want}`: an `@` right after an ASCII letter, digit or `_` never starts a \
                 directive, so the `{opener}` at line 1 is unclosed; put a space before the `@`"
            )
        };
        let e = p("@if(a)yes @else no@endif").unwrap_err();
        assert_eq!((e.line, e.col), (1, 19));
        assert_eq!(e.message, hint("o@endif", "endif", "@if"));
        // A mismatched terminator gets the same hint when a glued closer stands before it.
        let e = p("@for(x in y)
@auth 1_@endauth
@endfor")
        .unwrap_err();
        assert_eq!((e.line, e.col), (2, 9));
        assert_eq!(
            e.message,
            hint("_@endauth", "endauth", "@auth").replace("line 1", "line 2")
        );
        // `@endifx` is not a glued `@endif`: without one, the old message stays.
        let e = p("@if(a)x@endifx").unwrap_err();
        assert_eq!(e.message, "unclosed `@if` (missing `@endif`)");
        let e = p("x
@if(a) @endunless")
        .unwrap_err();
        assert!(e.message.starts_with("unexpected `@endunless`"), "{e}");
        // The escapes work after a word character too.
        assert_eq!(p("me@@auth x@{{ y").unwrap(), vec![text("me@auth x{{ y")]);
    }

    #[test]
    fn a_glued_branch_separator_is_an_error_not_merged_text() {
        // Sweep W6-03: `visitor@else` was text, so the guest saw the signed-in branch.
        let msg = |glued: &str, want: &str| {
            format!(
                "`{glued}` is text, not `@{want}`: an `@` right after an ASCII letter, digit or `_` never starts a \
                 directive, so the block would show this branch's text too; put a space before the `@` (or write \
                 `@@{want}` for the text)"
            )
        };
        let e = p("@guest\nWelcome, visitor@else <a>Admin</a>\n@endguest").unwrap_err();
        assert_eq!((e.line, e.col), (2, 17));
        assert_eq!(e.message, msg("r@else", "else"));
        let e = p("@if(a) yes@elseif(b) no @endif").unwrap_err();
        assert_eq!(e.message, msg("s@elseif", "elseif"));
        let e = p("@for(x in xs) x1@empty\nnone\n@endfor").unwrap_err();
        assert_eq!(e.message, msg("1@empty", "empty"));
        let e = p("@auth a@else b @endauth").unwrap_err();
        assert_eq!(e.message, msg("a@else", "else"));
        // Still text: e-mail addresses, a separator the block does not take, outside any block, escaped.
        for src in [
            "@if(a) mail bob@else.org or x@empty.io @endif",
            "@for(x in xs) a@else b @endfor",
            "@auth a@empty b @endauth",
            "@section(\"s\") a@else b @endsection",
            "a@else b",
            "@if(a) me@@else b @endif",
            "@if(a) x @else Hello there@else\n@endif",
            "@for(x in xs) {{ x }}@empty none @endfor",
        ] {
            assert!(p(src).is_ok(), "{src}: {:?}", p(src).err());
        }
        // The `if` inside a `for` takes `else`; the `for` around it takes `empty` again after `@endif`.
        assert!(p("@for(x in xs) @if(x) a @else b @endif c@empty d @endfor").is_err());
        assert!(p("@for(x in xs) @if(x) a @else b @endif @empty d @endfor").is_ok());
    }

    #[test]
    fn alloy_head_takes_no_arguments() {
        let e = p("<head>
@alloyHead (\"x\")")
        .unwrap_err();
        assert_eq!(
            e.to_string(),
            "t.mold.html:2:12: `@alloyHead` takes no arguments"
        );
        assert_eq!(p("@alloyHead, ok").unwrap()[1], text(", ok"));
    }

    #[test]
    fn alloy_and_vite_bad_arguments() {
        let msg = |src: &str| p(src).unwrap_err().to_string();
        let vite = "`@vite` takes one or more non-empty string literals (Vite entries), such as \
                    `@vite(\"resources/js/app.tsx\")`; a bare `@vite` renders the configured entries";
        assert_eq!(msg("x\n  @vite(1)"), format!("t.mold.html:2:9: {vite}"));
        assert_eq!(msg("@vite()"), format!("t.mold.html:1:7: {vite}"));
        assert_eq!(msg("@vite(\"\")"), format!("t.mold.html:1:7: {vite}"));
        assert_eq!(msg("@vite(\"a\", x)"), format!("t.mold.html:1:12: {vite}"));
        assert_eq!(msg("@vite(\"a\""), "t.mold.html:1:10: expected `)`");
        let alloy =
            "`@alloy` takes one string literal, the root element id, such as `@alloy(\"app\")`";
        assert_eq!(msg("@alloy(1, 2)"), format!("t.mold.html:1:8: {alloy}"));
        assert_eq!(
            msg("@alloy(\"a\", \"b\")"),
            format!("t.mold.html:1:11: {alloy}")
        );
        assert_eq!(msg("@alloy()"), format!("t.mold.html:1:8: {alloy}"));
        assert_eq!(
            msg("@alloy(\"a b\")"),
            "t.mold.html:1:8: invalid `@alloy` id `a b`: use letters, digits, `-` and `_`"
        );
        assert_eq!(
            msg("@alloy(\"\")"),
            "t.mold.html:1:8: invalid `@alloy` id ``: use letters, digits, `-` and `_`"
        );
        assert_eq!(
            msg("@alloy('\"><x')"),
            "t.mold.html:1:8: invalid `@alloy` id `\"><x`: use letters, digits, `-` and `_`"
        );
    }
}
