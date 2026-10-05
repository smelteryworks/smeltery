//! The runtime back end: walks a [`Resolved`] tree against a [`Value`].

use crate::ast::{BinOp, Expr, ExprKind, Filter, Func, Lit, Node, Resolved, UnOp};
use crate::rt::{self, Loop, Scalar};
use crate::{Error, Host, Value};

/// Renders `resolved` with `data` (a map) as the root scope.
pub(crate) fn render(resolved: &Resolved, data: &Value, host: &dyn Host) -> Result<String, Error> {
    let mut it = Interp {
        resolved,
        root: data,
        host,
        frames: Vec::new(),
    };
    let mut out = String::new();
    it.nodes(&resolved.nodes, &mut out)?;
    Ok(out)
}

struct Frame {
    isolated: bool,
    vars: Vec<(String, Value)>,
}

struct Interp<'a> {
    resolved: &'a Resolved,
    root: &'a Value,
    host: &'a dyn Host,
    frames: Vec<Frame>,
}

impl Interp<'_> {
    fn err(&self, e: &Expr, msg: impl Into<String>) -> Error {
        self.resolved.error(&e.span, msg)
    }

    fn nodes(&mut self, nodes: &[Node], out: &mut String) -> Result<(), Error> {
        nodes.iter().try_for_each(|n| self.node(n, out))
    }

    fn with_frame<T>(
        &mut self,
        isolated: bool,
        vars: Vec<(String, Value)>,
        f: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        self.frames.push(Frame { isolated, vars });
        let r = f(self);
        self.frames.pop();
        r
    }

    fn node(&mut self, node: &Node, out: &mut String) -> Result<(), Error> {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Echo { expr, escape } => {
                let v = self.eval(expr)?;
                write_value(&v, out, *escape).map_err(|m| self.err(expr, m))?;
            }
            Node::If {
                branches,
                otherwise,
            } => {
                for (cond, body) in branches {
                    if truthy(&self.eval(cond)?) {
                        return self.nodes(body, out);
                    }
                }
                self.nodes(otherwise, out)?;
            }
            Node::For {
                key,
                value,
                expr,
                body,
                empty,
            } => {
                let items: Vec<(Value, Value)> = match self.eval(expr)? {
                    Value::Null => Vec::new(),
                    Value::List(items) => items
                        .into_iter()
                        .enumerate()
                        .map(|(i, v)| (Value::Int(i64::try_from(i).unwrap_or(i64::MAX)), v))
                        .collect(),
                    Value::Map(entries) => entries
                        .into_iter()
                        .map(|(k, v)| (Value::Str(k), v))
                        .collect(),
                    other => {
                        return Err(self.err(expr, format!("cannot loop over {}", other.kind())));
                    }
                };
                if items.is_empty() {
                    return self.nodes(empty, out);
                }
                let count = items.len();
                for (i, (k, v)) in items.into_iter().enumerate() {
                    let mut vars = vec![
                        (value.clone(), v),
                        ("loop".to_owned(), Loop::new(i, count).to_value()),
                    ];
                    if let Some(key) = key {
                        vars.push((key.clone(), k));
                    }
                    self.with_frame(false, vars, |it| it.nodes(body, out))?;
                }
            }
            Node::Scope {
                isolated,
                slots,
                vars,
                body,
            } => {
                let mut bound = Vec::with_capacity(slots.len() + vars.len());
                for (name, slot) in slots {
                    let mut s = String::new();
                    self.nodes(slot, &mut s)?;
                    bound.push((name.clone(), Value::Safe(s)));
                }
                for (name, e) in vars {
                    bound.push((name.clone(), self.eval(e)?));
                }
                self.with_frame(*isolated, bound, |it| it.nodes(body, out))?;
            }
            Node::Csrf(span) => {
                out.push_str(&rt::csrf_field(self.host).map_err(|m| self.resolved.error(span, m))?);
            }
            Node::Error { field, body } => {
                if let Some(first) = self.host.errors(field).first() {
                    let vars = vec![("message".to_owned(), Value::Str(first.clone()))];
                    self.with_frame(false, vars, |it| it.nodes(body, out))?;
                }
            }
            Node::Auth {
                guest,
                body,
                otherwise,
            } => {
                if self.host.authenticated() != *guest {
                    self.nodes(body, out)?;
                } else {
                    self.nodes(otherwise, out)?;
                }
            }
            Node::Spark { name, props, span } => {
                let mut entries = Vec::with_capacity(props.len());
                for (k, e) in props {
                    let v = match self.eval(e)? {
                        Value::Safe(s) => Value::Str(s),
                        v => v,
                    };
                    entries.push((k.clone(), v));
                }
                let html = self
                    .host
                    .spark(name, &Value::Map(entries))
                    .map_err(|m| self.resolved.error(span, m))?;
                out.push_str(&html);
            }
            Node::SparksScripts => out.push_str(&self.host.sparks_scripts()),
            Node::Alloy { id, span } => {
                let html = self
                    .host
                    .alloy_page(id)
                    .map_err(|m| self.resolved.error(span, m))?;
                out.push_str(&html);
            }
            Node::AlloyHead => out.push_str(&self.host.alloy_head()),
            Node::Vite { entries, span } => {
                let entries: Vec<&str> = entries.iter().map(String::as_str).collect();
                let html = self
                    .host
                    .vite(&entries)
                    .map_err(|m| self.resolved.error(span, m))?;
                out.push_str(&html);
            }
        }
        Ok(())
    }

    fn lookup(&self, name: &str, e: &Expr) -> Result<Value, Error> {
        for frame in self.frames.iter().rev() {
            if let Some((_, v)) = frame.vars.iter().rev().find(|(n, _)| n == name) {
                return Ok(v.clone());
            }
            if frame.isolated {
                return Err(self.err(e, format!("unknown variable `{name}`")));
            }
        }
        self.root
            .get(name)
            .cloned()
            .ok_or_else(|| self.err(e, format!("unknown variable `{name}`")))
    }

    fn scalar<'v>(&self, v: &'v Value, e: &Expr) -> Result<Scalar<'v>, Error> {
        as_scalar(v).map_err(|m| self.err(e, m))
    }

    fn eval(&mut self, e: &Expr) -> Result<Value, Error> {
        Ok(match &e.kind {
            ExprKind::Lit(lit) => match lit {
                Lit::Null => Value::Null,
                Lit::Bool(b) => Value::Bool(*b),
                Lit::Int(i) => Value::Int(*i),
                Lit::Float(f) => Value::Float(*f),
                Lit::Str(s) => Value::Str(s.clone()),
            },
            ExprKind::Var(name) => self.lookup(name, e)?,
            ExprKind::Field(inner, name) => match self.eval(inner)? {
                Value::Map(entries) => entries
                    .into_iter()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v)
                    .ok_or_else(|| self.err(e, format!("no field `{name}`")))?,
                other => {
                    return Err(
                        self.err(e, format!("cannot read field `{name}` of {}", other.kind()))
                    );
                }
            },
            ExprKind::Index(inner, idx) => {
                let base = self.eval(inner)?;
                let key = self.eval(idx)?;
                let key = self.scalar(&key, idx)?;
                match base {
                    Value::List(items) => {
                        let found = match &key {
                            Scalar::Int(i) => usize::try_from(*i).ok().and_then(|i| items.get(i)),
                            Scalar::UInt(u) => usize::try_from(*u).ok().and_then(|i| items.get(i)),
                            _ => {
                                return Err(self.err(e, "a list index must be an integer"));
                            }
                        };
                        found.cloned().ok_or_else(|| {
                            self.err(
                                e,
                                format!(
                                    "index {} is out of range for a list of {} items",
                                    rt::display(&key),
                                    items.len()
                                ),
                            )
                        })?
                    }
                    Value::Map(entries) => {
                        let k = rt::display(&key);
                        entries
                            .into_iter()
                            .find(|(n, _)| *n == k)
                            .map(|(_, v)| v)
                            .ok_or_else(|| self.err(e, format!("no key `{k}` in the map")))?
                    }
                    other => return Err(self.err(e, format!("cannot index into {}", other.kind()))),
                }
            }
            ExprKind::Unary(UnOp::Not, inner) => Value::Bool(!truthy(&self.eval(inner)?)),
            ExprKind::Unary(UnOp::Neg, inner) => {
                let v = self.eval(inner)?;
                let s = self.scalar(&v, inner)?;
                rt::neg(&s).map_err(|m| self.err(e, m))?.into()
            }
            ExprKind::Binary(BinOp::And, a, b) => {
                Value::Bool(truthy(&self.eval(a)?) && truthy(&self.eval(b)?))
            }
            ExprKind::Binary(BinOp::Or, a, b) => {
                Value::Bool(truthy(&self.eval(a)?) || truthy(&self.eval(b)?))
            }
            ExprKind::Binary(op, a, b) => {
                let (va, vb) = (self.eval(a)?, self.eval(b)?);
                let (sa, sb) = (self.scalar(&va, a)?, self.scalar(&vb, b)?);
                if op.is_comparison() {
                    Value::Bool(rt::compare(*op, &sa, &sb).map_err(|m| self.err(e, m))?)
                } else {
                    rt::arith(*op, &sa, &sb).map_err(|m| self.err(e, m))?.into()
                }
            }
            ExprKind::Filter(f, inner, args) => {
                let v = self.eval(inner)?;
                let text = |it: &Self| display_value(&v).map_err(|m| it.err(inner, m));
                match f {
                    Filter::Upper => Value::Str(rt::upper(&text(self)?)),
                    Filter::Lower => Value::Str(rt::lower(&text(self)?)),
                    Filter::Trim => Value::Str(rt::trim(&text(self)?)),
                    Filter::Title => Value::Str(rt::title(&text(self)?)),
                    Filter::Len => {
                        let n = match &v {
                            Value::Null => 0,
                            Value::Str(s) | Value::Safe(s) => s.chars().count(),
                            Value::List(items) => items.len(),
                            Value::Map(entries) => entries.len(),
                            other => {
                                return Err(self.err(
                                    e,
                                    format!(
                                        "`len` needs a string, list or map, not {}",
                                        other.kind()
                                    ),
                                ));
                            }
                        };
                        Value::Int(i64::try_from(n).unwrap_or(i64::MAX))
                    }
                    Filter::Default => {
                        let fallback = self.eval(arg(args, 0, e, self)?)?;
                        let a = self.scalar(&v, inner)?;
                        let b = self.scalar(&fallback, e)?;
                        rt::default(a, b).into()
                    }
                    Filter::Join => {
                        let sep = self.eval(arg(args, 0, e, self)?)?;
                        let sep = display_value(&sep).map_err(|m| self.err(e, m))?;
                        let items: Vec<Value> = match v {
                            Value::Null => Vec::new(),
                            Value::List(items) => items,
                            Value::Map(entries) => entries.into_iter().map(|(_, v)| v).collect(),
                            other => {
                                return Err(self
                                    .err(e, format!("`join` needs a list, not {}", other.kind())));
                            }
                        };
                        let mut parts = Vec::with_capacity(items.len());
                        for item in &items {
                            parts.push(display_value(item).map_err(|m| self.err(e, m))?);
                        }
                        Value::Str(parts.join(&sep))
                    }
                    Filter::Json => Value::Str(rt::json(&v).map_err(|m| self.err(e, m))?),
                    Filter::Url => Value::Str(rt::url(&text(self)?)),
                }
            }
            ExprKind::Call(func, args, params) => match func {
                Func::Old => {
                    let field = self.eval(arg(args, 0, e, self)?)?;
                    let field = display_value(&field).map_err(|m| self.err(e, m))?;
                    Value::Str(self.host.old(&field).unwrap_or("").to_owned())
                }
                Func::Session => {
                    let key = self.eval(arg(args, 0, e, self)?)?;
                    let key = display_value(&key).map_err(|m| self.err(e, m))?;
                    Value::Str(self.host.session(&key).unwrap_or_default())
                }
                Func::CsrfToken => Value::Str(
                    rt::csrf_token(self.host)
                        .map_err(|m| self.err(e, m))?
                        .to_owned(),
                ),
                Func::Route => {
                    let name = self.eval(arg(args, 0, e, self)?)?;
                    let name = display_value(&name).map_err(|m| self.err(e, m))?;
                    let mut ps = Vec::with_capacity(params.len());
                    for (k, pe) in params {
                        let v = self.eval(pe)?;
                        ps.push((k.clone(), display_value(&v).map_err(|m| self.err(pe, m))?));
                    }
                    Value::Str(self.host.route(&name, &ps).map_err(|m| self.err(e, m))?)
                }
            },
        })
    }
}

fn arg<'e>(args: &'e [Expr], i: usize, e: &Expr, it: &Interp<'_>) -> Result<&'e Expr, Error> {
    args.get(i).ok_or_else(|| it.err(e, "missing argument"))
}

/// Truthiness of a [`Value`] (same rules as [`rt::Truthy`]).
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Int(i) => *i != 0,
        Value::UInt(u) => *u != 0,
        Value::Float(f) => *f != 0.0,
        Value::Str(s) | Value::Safe(s) => !s.is_empty(),
        Value::List(items) => !items.is_empty(),
        Value::Map(entries) => !entries.is_empty(),
    }
}

fn as_scalar(v: &Value) -> Result<Scalar<'_>, String> {
    Ok(match v {
        Value::Null => Scalar::Null,
        Value::Bool(b) => Scalar::Bool(*b),
        Value::Int(i) => Scalar::Int(*i),
        Value::UInt(u) => Scalar::UInt(*u),
        Value::Float(f) => Scalar::Float(*f),
        Value::Str(s) | Value::Safe(s) => Scalar::Str(s.into()),
        other => return Err(format!("expected a single value, found {}", other.kind())),
    })
}

fn display_value(v: &Value) -> Result<String, String> {
    let mut out = String::new();
    write_value(v, &mut out, false)?;
    Ok(out)
}

fn write_value(v: &Value, out: &mut String, escape: bool) -> Result<(), String> {
    match v {
        Value::Safe(s) => out.push_str(s),
        Value::List(_) | Value::Map(_) => return Err(format!("cannot display {}", v.kind())),
        other => rt::Render::render(&as_scalar(other)?, out, escape),
    }
    Ok(())
}
