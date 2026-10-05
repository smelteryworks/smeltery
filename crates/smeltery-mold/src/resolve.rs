//! The resolver: inlines `@extends`/`@section`/`@yield`, `@include` and components into one flat tree, checks
//! variables inside components and detects cycles.

use crate::Error;
use crate::ast::{Expr, ExprKind, Node, Resolved, SourceFile, Span};
use crate::parse::{PNode, parse};
use crate::rt;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Parses and resolves template `name` from `views_dir`, showing files in errors under `display_dir`
/// (for example `resources/views`).
pub fn resolve_dir(views_dir: &Path, display_dir: &Path, name: &str) -> Result<Resolved, Error> {
    let mut r = Resolver {
        views_dir,
        display_dir,
        files: Vec::new(),
        stack: Vec::new(),
        scopes: Vec::new(),
    };
    let nodes = r.template(name, &HashMap::new(), None)?;
    Ok(Resolved {
        nodes: merge_text(nodes),
        files: r.files,
    })
}

/// The file of template `name` under `dir`.
pub(crate) fn template_path(dir: &Path, name: &str) -> PathBuf {
    let mut p = dir.to_path_buf();
    for part in name.split('/') {
        p.push(part);
    }
    let file = format!(
        "{}.mold.html",
        p.file_name().and_then(|f| f.to_str()).unwrap_or("")
    );
    p.set_file_name(file);
    p
}

/// The file of template `name` under `dir` as shown in errors: always with `/` between components, so messages
/// read the same on every OS and in both modes (runtime and compiled).
pub(crate) fn display_file(dir: &Path, name: &str) -> String {
    let shown = template_path(dir, name).display().to_string();
    // Only on platforms whose separator is not `/` (Windows): on Unix a `\` is an ordinary file-name character.
    if std::path::MAIN_SEPARATOR == '/' {
        shown
    } else {
        shown.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && name
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && Path::new(name)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

type Sections = HashMap<String, Vec<Node>>;

struct Frame {
    isolated: bool,
    names: Vec<String>,
    /// The component whose body this frame belongs to, for error messages.
    owner: Option<String>,
}

struct Resolver<'a> {
    views_dir: &'a Path,
    display_dir: &'a Path,
    files: Vec<SourceFile>,
    /// Templates being resolved, outermost first, for cycle detection.
    stack: Vec<String>,
    scopes: Vec<Frame>,
}

impl Resolver<'_> {
    fn error(&self, span: &Span, msg: impl Into<String>) -> Error {
        let src = self
            .files
            .iter()
            .find(|f| f.display == span.file)
            .map_or("", |f| &f.text);
        Error::at(&span.file, src, span.line, span.col, msg)
    }

    fn load(&mut self, name: &str, via: Option<&Span>) -> Result<(Arc<str>, Arc<str>), Error> {
        let fail = |this: &Self, msg: String| match via {
            Some(span) => this.error(span, msg),
            None => Error::in_file(&display_file(this.display_dir, name), msg),
        };
        if !valid_name(name) {
            return Err(fail(self, format!("invalid template name `{name}`")));
        }
        if let Some(f) = self.files.iter().find(|f| f.name == name) {
            return Ok((f.display.clone(), f.text.clone()));
        }
        let path = template_path(self.views_dir, name);
        let display: Arc<str> = Arc::from(display_file(self.display_dir, name));
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let text: Arc<str> = match std::fs::read_to_string(&path) {
            Ok(t) => Arc::from(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let msg = match via {
                    Some(_) => format!("template `{name}` not found (looked for {display})"),
                    None => format!("template `{name}` not found"),
                };
                return Err(fail(self, msg));
            }
            Err(e) => return Err(fail(self, format!("cannot read {display}: {e}"))),
        };
        self.files.push(SourceFile {
            name: name.to_owned(),
            display: display.clone(),
            path,
            text: text.clone(),
            mtime,
        });
        Ok((display, text))
    }

    /// Resolves template `name` with the sections given by the templates extending it.
    fn template(
        &mut self,
        name: &str,
        sections: &Sections,
        via: Option<&Span>,
    ) -> Result<Vec<Node>, Error> {
        if self.stack.iter().any(|n| n == name) {
            let mut chain = self.stack.clone();
            chain.push(name.to_owned());
            let msg = format!("template cycle: {}", chain.join(" -> "));
            return Err(match via {
                Some(span) => self.error(span, msg),
                None => Error::msg(msg),
            });
        }
        let (display, text) = self.load(name, via)?;
        let nodes = parse(&text, &display)?;
        self.stack.push(name.to_owned());
        let result = self.template_nodes(nodes, sections);
        self.stack.pop();
        result
    }

    fn template_nodes(
        &mut self,
        nodes: Vec<PNode>,
        sections: &Sections,
    ) -> Result<Vec<Node>, Error> {
        let first = nodes
            .iter()
            .position(|n| !matches!(n, PNode::Text(t) if t.trim().is_empty()));
        let Some(PNode::Extends(layout, span)) = first.and_then(|i| nodes.get(i)) else {
            return self.nodes(nodes, sections);
        };
        let (layout, span) = (layout.clone(), span.clone());
        let mut merged = sections.clone();
        for (i, node) in nodes.into_iter().enumerate() {
            match node {
                PNode::Section(name, body, _) => {
                    let body = self.nodes(body, sections)?;
                    merged.entry(name).or_insert(body);
                }
                PNode::SectionInline(name, expr) => {
                    self.check_expr(&expr)?;
                    merged
                        .entry(name)
                        .or_insert(vec![Node::Echo { expr, escape: true }]);
                }
                PNode::Extends(_, s) if Some(i) != first => {
                    return Err(self.error(&s, "`@extends` must come first in the template"));
                }
                // Content outside sections is not rendered in a template that extends a layout.
                _ => {}
            }
        }
        self.template(&layout, &merged, Some(&span))
    }

    fn nodes(&mut self, nodes: Vec<PNode>, sections: &Sections) -> Result<Vec<Node>, Error> {
        let mut out = Vec::with_capacity(nodes.len());
        for node in nodes {
            self.node(node, sections, &mut out)?;
        }
        Ok(out)
    }

    fn node(&mut self, node: PNode, sections: &Sections, out: &mut Vec<Node>) -> Result<(), Error> {
        match node {
            PNode::Text(t) => out.push(Node::Text(t)),
            PNode::Echo(expr, escape) => {
                self.check_expr(&expr)?;
                out.push(Node::Echo { expr, escape });
            }
            PNode::If(branches, otherwise) => {
                let mut rb = Vec::with_capacity(branches.len());
                for (cond, body) in branches {
                    self.check_expr(&cond)?;
                    rb.push((cond, self.nodes(body, sections)?));
                }
                let otherwise = self.nodes(otherwise, sections)?;
                out.push(Node::If {
                    branches: rb,
                    otherwise,
                });
            }
            PNode::For {
                key,
                value,
                expr,
                body,
                empty,
            } => {
                self.check_expr(&expr)?;
                let mut names = vec![value.clone(), "loop".to_owned()];
                names.extend(key.clone());
                let body = self.scoped(false, names, None, |r| r.nodes(body, sections))?;
                let empty = self.nodes(empty, sections)?;
                out.push(Node::For {
                    key,
                    value,
                    expr,
                    body,
                    empty,
                });
            }
            PNode::Extends(_, span) => {
                return Err(self.error(&span, "`@extends` must come first in the template"));
            }
            PNode::Section(_, _, span) => {
                return Err(self.error(
                    &span,
                    "`@section` is only allowed in a template that uses `@extends`",
                ));
            }
            PNode::SectionInline(_, expr) => {
                return Err(self.error(
                    &expr.span,
                    "`@section` is only allowed in a template that uses `@extends`",
                ));
            }
            PNode::Yield(name, default) => match sections.get(&name) {
                Some(body) => out.extend(body.iter().cloned()),
                None => {
                    if let Some(d) = default {
                        out.push(Node::Text(rt::escape(&d)));
                    }
                }
            },
            PNode::Include(name, vars, span) => {
                for (_, e) in &vars {
                    self.check_expr(e)?;
                }
                let names = vars.iter().map(|(n, _)| n.clone()).collect();
                let body = self.scoped(false, names, None, |r| {
                    r.template(&name, sections, Some(&span))
                })?;
                out.push(Node::Scope {
                    isolated: false,
                    slots: Vec::new(),
                    vars,
                    body: merge_text(body),
                });
            }
            PNode::Slot(_, _, span) => {
                return Err(
                    self.error(&span, "`@slot` is only allowed directly inside a component")
                );
            }
            PNode::Component {
                name,
                props,
                body,
                span,
            } => {
                for (_, e) in &props {
                    self.check_expr(e)?;
                }
                let mut slots = Vec::new();
                let mut default = Vec::new();
                for n in body {
                    match n {
                        PNode::Slot(slot, body, _) => {
                            let body = self.nodes(body, sections)?;
                            slots.push((slot, merge_text(body)));
                        }
                        other => default.push(other),
                    }
                }
                let default = self.nodes(default, sections)?;
                slots.insert(0, ("slot".to_owned(), merge_text(default)));
                let mut names: Vec<String> = props.iter().map(|(n, _)| n.clone()).collect();
                names.extend(slots.iter().map(|(n, _)| n.clone()));
                let template = format!("components/{name}");
                let owner = Some(template.clone());
                let body = self.scoped(true, names, owner, |r| {
                    r.template(&template, &HashMap::new(), Some(&span))
                })?;
                out.push(Node::Scope {
                    isolated: true,
                    slots,
                    vars: props,
                    body: merge_text(body),
                });
            }
            PNode::Csrf(span) => out.push(Node::Csrf(span)),
            PNode::Method(m) => out.push(Node::Text(rt::method_field(&m))),
            PNode::ErrorBlock(field, body) => {
                let body = self.scoped(false, vec!["message".to_owned()], None, |r| {
                    r.nodes(body, sections)
                })?;
                out.push(Node::Error { field, body });
            }
            PNode::Auth {
                guest,
                body,
                otherwise,
            } => {
                let body = self.nodes(body, sections)?;
                let otherwise = self.nodes(otherwise, sections)?;
                out.push(Node::Auth {
                    guest,
                    body,
                    otherwise,
                });
            }
            PNode::Spark(name, props, span) => {
                for (_, e) in &props {
                    self.check_expr(e)?;
                }
                out.push(Node::Spark { name, props, span });
            }
            PNode::SparksScripts => out.push(Node::SparksScripts),
            PNode::Alloy(id, span) => out.push(Node::Alloy { id, span }),
            PNode::AlloyHead => out.push(Node::AlloyHead),
            PNode::Vite(entries, span) => out.push(Node::Vite { entries, span }),
        }
        Ok(())
    }

    fn scoped<T>(
        &mut self,
        isolated: bool,
        names: Vec<String>,
        owner: Option<String>,
        f: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.scopes.push(Frame {
            isolated,
            names,
            owner,
        });
        let r = f(self);
        self.scopes.pop();
        r
    }

    /// Inside a component only its props, slots and the variables bound within it exist.
    fn check_expr(&self, e: &Expr) -> Result<(), Error> {
        match &e.kind {
            ExprKind::Lit(_) => Ok(()),
            ExprKind::Var(name) => {
                for frame in self.scopes.iter().rev() {
                    if frame.names.iter().any(|n| n == name) {
                        return Ok(());
                    }
                    if frame.isolated {
                        let owner = frame.owner.as_deref().unwrap_or("component");
                        return Err(self.error(
                            &e.span,
                            format!("unknown variable `{name}` in component `{owner}` (pass it as a prop)"),
                        ));
                    }
                }
                Ok(())
            }
            ExprKind::Field(inner, _) | ExprKind::Unary(_, inner) => self.check_expr(inner),
            ExprKind::Index(a, b) | ExprKind::Binary(_, a, b) => {
                self.check_expr(a)?;
                self.check_expr(b)
            }
            ExprKind::Filter(_, inner, args) => {
                self.check_expr(inner)?;
                args.iter().try_for_each(|a| self.check_expr(a))
            }
            ExprKind::Call(_, args, params) => {
                args.iter().try_for_each(|a| self.check_expr(a))?;
                params.iter().try_for_each(|(_, a)| self.check_expr(a))
            }
        }
    }
}

/// Joins adjacent text nodes.
fn merge_text(nodes: Vec<Node>) -> Vec<Node> {
    let mut out: Vec<Node> = Vec::with_capacity(nodes.len());
    for n in nodes {
        match (out.last_mut(), n) {
            (Some(Node::Text(prev)), Node::Text(t)) => prev.push_str(&t),
            (_, Node::Text(t)) if t.is_empty() => {}
            (_, n) => out.push(n),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Lit;
    use std::fs;

    fn lit_str(e: &Expr) -> Option<&str> {
        match &e.kind {
            ExprKind::Lit(Lit::Str(s)) => Some(s),
            _ => None,
        }
    }

    fn views(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            let path = template_path(dir.path(), name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        dir
    }

    fn resolve(dir: &tempfile::TempDir, name: &str) -> Result<Resolved, Error> {
        resolve_dir(dir.path(), Path::new("views"), name)
    }

    #[test]
    fn layouts_are_inlined() {
        let d = views(&[
            (
                "base",
                "<title>@yield(\"title\", \"Def & co\")</title>@yield(\"body\")",
            ),
            (
                "app",
                "@extends(\"base\")\n@section(\"body\")<main>@yield(\"content\")</main>@endsection\n",
            ),
            (
                "page",
                "@extends(\"app\")\n@section(\"content\")Hi @endsection\nignored",
            ),
        ]);
        let r = resolve(&d, "page").unwrap();
        assert_eq!(
            r.nodes,
            vec![Node::Text(
                "<title>Def &amp; co</title><main>Hi </main>".into()
            )]
        );
        assert_eq!(r.files.len(), 3);
    }

    #[test]
    fn cycles_are_reported_with_the_chain() {
        let d = views(&[("a", "x\n@include(\"b\")"), ("b", "@include(\"a\")")]);
        let e = resolve(&d, "a").unwrap_err();
        assert_eq!(
            e.to_string(),
            "views/b.mold.html:1:1: template cycle: a -> b -> a"
        );
        let d = views(&[("components/x", "<x-x />")]);
        let e = resolve(&d, "components/x").unwrap_err();
        assert!(e.message.contains("components/x -> components/x"), "{e}");
    }

    #[test]
    fn unknown_variable_in_component() {
        let d = views(&[
            (
                "components/alert",
                "<div class=\"{{ type }}\">{{ slot }}{{ titel }}</div>",
            ),
            ("p", "{{ anything }}<x-alert type=\"error\">Body</x-alert>"),
        ]);
        let e = resolve(&d, "p").unwrap_err();
        assert_eq!(
            e.to_string(),
            "views/components/alert.mold.html:1:38: unknown variable `titel` in component `components/alert` (pass it as a prop)"
        );
    }

    #[test]
    fn components_slots_and_includes() {
        let d = views(&[
            ("components/card", "[{{ title }}|{{ slot }}|{{ n }}]"),
            ("partials/x", "{{ who }}"),
            (
                "p",
                "<x-card :n=\"1\">@slot(\"title\")T @endslot body</x-card>@include(\"partials/x\", { who: \"me\" })",
            ),
        ]);
        let r = resolve(&d, "p").unwrap();
        let Node::Scope {
            isolated: true,
            slots,
            vars,
            ..
        } = &r.nodes[0]
        else {
            panic!("{:?}", r.nodes)
        };
        assert_eq!(
            slots[0],
            ("slot".to_owned(), vec![Node::Text(" body".into())])
        );
        assert_eq!(
            slots[1],
            ("title".to_owned(), vec![Node::Text("T ".into())])
        );
        assert_eq!(vars[0].0, "n");
        let Node::Scope {
            isolated: false,
            vars,
            ..
        } = &r.nodes[1]
        else {
            panic!()
        };
        assert_eq!(lit_str(&vars[0].1), Some("me"));
    }

    #[test]
    fn missing_and_invalid_templates() {
        let d = views(&[("p", "\n@include(\"nope\")")]);
        let e = resolve(&d, "p").unwrap_err();
        assert_eq!(
            e.to_string(),
            "views/p.mold.html:2:1: template `nope` not found (looked for views/nope.mold.html)"
        );
        let e = resolve(&d, "../etc/passwd").unwrap_err();
        assert!(e.message.contains("invalid template name"));
        let e = resolve(&d, "missing").unwrap_err();
        assert_eq!(e.file, "views/missing.mold.html");
    }

    #[test]
    fn displayed_paths_use_forward_slashes() {
        // A display dir joined with the OS separator (`\` on Windows) still shows `/` everywhere.
        let dir = Path::new("resources").join("views");
        assert_eq!(
            display_file(&dir, "posts/index"),
            "resources/views/posts/index.mold.html"
        );
        assert_eq!(
            display_file(Path::new("tests/views"), "errors"),
            "tests/views/errors.mold.html"
        );
        let d = views(&[("posts/p", "\n@include(\"posts/nope\")")]);
        let e = resolve_dir(d.path(), &dir, "posts/p").unwrap_err();
        assert_eq!(
            e.to_string(),
            "resources/views/posts/p.mold.html:2:1: template `posts/nope` not found (looked for resources/views/posts/nope.mold.html)"
        );
    }

    #[test]
    fn misplaced_directives() {
        let d = views(&[
            ("a", "x @extends(\"b\")"),
            ("b", "@section(\"s\")x @endsection"),
            ("c", "@slot(\"x\")y @endslot"),
        ]);
        assert!(
            resolve(&d, "a")
                .unwrap_err()
                .message
                .contains("must come first")
        );
        assert!(resolve(&d, "b").unwrap_err().message.contains("@extends"));
        assert!(
            resolve(&d, "c")
                .unwrap_err()
                .message
                .contains("inside a component")
        );
    }
}
