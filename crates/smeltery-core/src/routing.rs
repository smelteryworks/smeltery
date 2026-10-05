//! Routing: the [`Router`] that `routes/web.rs` and `routes/api.rs` fill.
//!
//! ```
//! use smeltery_core::routing::Router;
//!
//! async fn home() -> &'static str { "home" }
//! async fn dashboard() -> &'static str { "admin" }
//! async fn index() -> &'static str { "posts" }
//! async fn show() -> &'static str { "post" }
//!
//! pub fn routes(r: &mut Router) {
//!     r.get("/", home).name("home");
//!     r.group("/admin", |r| {
//!         r.get("/", dashboard).name("dashboard");
//!     })
//!     .name("admin.")
//!     .middleware("auth");
//!     r.resource("/posts").index(index).show(show);
//! }
//! ```

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;

use axum::handler::Handler;
use axum::routing::{MethodFilter, MethodRouter};
use http::Method;

use crate::app::{App, Kind};
use crate::error::{Error, Result};
use crate::middleware::{ErasedMiddleware, Family};

/// One route as `route:list` and the app see it.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RouteInfo {
    /// HTTP methods, e.g. `["GET", "HEAD"]` or `["PUT", "PATCH"]`.
    pub methods: Vec<String>,
    /// The path with `{param}` placeholders.
    pub path: String,
    /// The route name, if it has one.
    pub name: Option<String>,
    /// Middleware aliases, outermost first.
    pub middleware: Vec<String>,
    /// `true` for routes from `routes/api.rs`.
    pub api: bool,
}

/// A route being declared; returned by [`Router::get`] and friends to name it or add
/// middleware.
pub struct Route {
    methods: Vec<Method>,
    path: String,
    name: Option<String>,
    middleware: Vec<String>,
    kind: Kind,
    handler: MethodRouter<App>,
    /// Declared with `Router::any`: middleware wraps the whole method router.
    any: bool,
}

impl std::fmt::Debug for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Route")
            .field("methods", &self.methods)
            .field("path", &self.path)
            .field("name", &self.name)
            .field("middleware", &self.middleware)
            .finish_non_exhaustive()
    }
}

impl Route {
    /// Name the route, for URL generation (`app.url("posts.show", …)`).
    pub fn name(&mut self, name: impl Into<String>) -> &mut Self {
        self.name = Some(name.into());
        self
    }

    /// Add middleware by alias. Middleware added first runs first.
    pub fn middleware(&mut self, alias: impl Into<String>) -> &mut Self {
        self.middleware.push(alias.into());
        self
    }
}

/// Collects route declarations. Paths use `{param}` placeholders (`/posts/{post}`) and
/// `{*rest}` for a catch-all tail.
pub struct Router {
    kind: Kind,
    prefix: String,
    routes: Vec<Route>,
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Router")
            .field("prefix", &self.prefix)
            .field("routes", &self.routes)
            .finish()
    }
}

macro_rules! method_fn {
    ($(#[$doc:meta] $fn:ident => $filter:ident, [$($m:ident),+];)*) => {$(
        #[$doc]
        pub fn $fn<H, T>(&mut self, path: &str, handler: H) -> &mut Route
        where
            H: Handler<T, App>,
            T: 'static,
        {
            self.add(vec![$(Method::$m),+], MethodFilter::$filter, path, handler)
        }
    )*};
}

impl Router {
    pub(crate) fn new(kind: Kind, prefix: &str) -> Self {
        Self {
            kind,
            prefix: prefix.to_owned(),
            routes: Vec::new(),
        }
    }

    method_fn! {
        /// A `GET` (and `HEAD`) route.
        get => GET, [GET, HEAD];
        /// A `POST` route.
        post => POST, [POST];
        /// A `PUT` route.
        put => PUT, [PUT];
        /// A `PATCH` route.
        patch => PATCH, [PATCH];
        /// A `DELETE` route.
        delete => DELETE, [DELETE];
    }

    /// A route answering every method.
    pub fn any<H, T>(&mut self, path: &str, handler: H) -> &mut Route
    where
        H: Handler<T, App>,
        T: 'static,
    {
        let path = join(&self.prefix, path);
        self.routes.push(Route {
            methods: vec![
                Method::GET,
                Method::HEAD,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ],
            path,
            name: None,
            middleware: Vec::new(),
            kind: self.kind,
            handler: axum::routing::any(handler),
            any: true,
        });
        self.last()
    }

    fn add<H, T>(
        &mut self,
        methods: Vec<Method>,
        filter: MethodFilter,
        path: &str,
        handler: H,
    ) -> &mut Route
    where
        H: Handler<T, App>,
        T: 'static,
    {
        let path = join(&self.prefix, path);
        self.routes.push(Route {
            methods,
            path,
            name: None,
            middleware: Vec::new(),
            kind: self.kind,
            handler: axum::routing::on(filter, handler),
            any: false,
        });
        self.last()
    }

    fn last(&mut self) -> &mut Route {
        let index = self.routes.len() - 1;
        // `index` is the route just pushed.
        #[allow(clippy::indexing_slicing)]
        &mut self.routes[index]
    }

    /// Routes under a path prefix. Name prefixes and middleware for the whole group are set
    /// on the returned [`Group`].
    pub fn group(&mut self, prefix: &str, routes: impl FnOnce(&mut Router)) -> Group<'_> {
        let mut inner = Router::new(self.kind, &join(&self.prefix, prefix));
        routes(&mut inner);
        let start = self.routes.len();
        self.routes.extend(inner.routes);
        let end = self.routes.len();
        Group {
            routes: self.routes.get_mut(start..end).unwrap_or_default(),
        }
    }

    /// Resource routes for `path` (e.g. `/posts`): pick the actions you have with
    /// [`Resource`]'s methods.
    ///
    /// | Action | Method | Path | Name |
    /// |---|---|---|---|
    /// | `index` | GET | `/posts` | `posts.index` |
    /// | `create` | GET | `/posts/create` | `posts.create` |
    /// | `store` | POST | `/posts` | `posts.store` |
    /// | `show` | GET | `/posts/{post}` | `posts.show` |
    /// | `edit` | GET | `/posts/{post}/edit` | `posts.edit` |
    /// | `update` | PUT, PATCH | `/posts/{post}` | `posts.update` |
    /// | `destroy` | DELETE | `/posts/{post}` | `posts.destroy` |
    ///
    /// The placeholder is the last path segment made singular (`posts` → `post`,
    /// `categories` → `category`); the name is the path with `/` turned into `.`.
    pub fn resource(&mut self, path: &str) -> Resource<'_> {
        let trimmed = path.trim_matches('/');
        let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
        Resource {
            base: format!("/{trimmed}"),
            name: trimmed.replace('/', "."),
            param: singular(last).replace('-', "_"),
            start: self.routes.len(),
            router: self,
        }
    }

    pub(crate) fn into_routes(self) -> Vec<Route> {
        self.routes
    }
}

/// A group of routes from [`Router::group`].
#[derive(Debug)]
pub struct Group<'a> {
    routes: &'a mut [Route],
}

impl Group<'_> {
    /// Add middleware to every route of the group. It runs before the routes' own.
    pub fn middleware(self, alias: impl Into<String>) -> Self {
        let alias = alias.into();
        for route in self.routes.iter_mut() {
            route.middleware.insert(0, alias.clone());
        }
        self
    }

    /// Prefix every route name in the group (`"admin."` turns `dashboard` into
    /// `admin.dashboard`).
    pub fn name(self, prefix: &str) -> Self {
        for route in self.routes.iter_mut() {
            if let Some(name) = &mut route.name {
                name.insert_str(0, prefix);
            }
        }
        self
    }
}

/// Resource routes from [`Router::resource`].
#[derive(Debug)]
pub struct Resource<'a> {
    router: &'a mut Router,
    base: String,
    name: String,
    param: String,
    start: usize,
}

macro_rules! resource_fn {
    ($(#[$doc:meta] $fn:ident => $method:ident, $suffix:expr, $member:expr;)*) => {$(
        #[$doc]
        pub fn $fn<H, T>(self, handler: H) -> Self
        where
            H: Handler<T, App>,
            T: 'static,
        {
            let path = self.path($member, $suffix);
            let name = format!("{}.{}", self.name, stringify!($fn));
            self.router.$method(&path, handler).name(name);
            self
        }
    )*};
}

impl Resource<'_> {
    fn path(&self, member: bool, suffix: &str) -> String {
        let mut path = self.base.clone();
        if member {
            path.push_str(&format!("/{{{}}}", self.param));
        }
        path.push_str(suffix);
        path
    }

    resource_fn! {
        /// `GET /posts` → `posts.index`.
        index => get, "", false;
        /// `GET /posts/create` → `posts.create`.
        create => get, "/create", false;
        /// `POST /posts` → `posts.store`.
        store => post, "", false;
        /// `GET /posts/{post}` → `posts.show`.
        show => get, "", true;
        /// `GET /posts/{post}/edit` → `posts.edit`.
        edit => get, "/edit", true;
        /// `DELETE /posts/{post}` → `posts.destroy`.
        destroy => delete, "", true;
    }

    /// `PUT` and `PATCH /posts/{post}` → `posts.update`.
    pub fn update<H, T>(self, handler: H) -> Self
    where
        H: Handler<T, App>,
        T: 'static,
    {
        let path = self.path(true, "");
        let name = format!("{}.update", self.name);
        self.router
            .add(
                vec![Method::PUT, Method::PATCH],
                MethodFilter::PUT.or(MethodFilter::PATCH),
                &path,
                handler,
            )
            .name(name);
        self
    }

    /// Add middleware to every route of the resource declared so far.
    pub fn middleware(self, alias: impl Into<String>) -> Self {
        let alias = alias.into();
        for route in self.router.routes.iter_mut().skip(self.start) {
            route.middleware.insert(0, alias.clone());
        }
        self
    }
}

/// The checked route table, ready to become an Axum router.
pub(crate) struct RouteTable {
    routes: Vec<(Route, Vec<ErasedMiddleware>)>,
}

impl RouteTable {
    pub(crate) fn new(
        routes: Vec<Route>,
        aliases: &HashMap<String, ErasedMiddleware>,
        families: &HashMap<String, Family>,
    ) -> Result<Self> {
        let mut seen = HashSet::new();
        let mut names = HashSet::new();
        let mut out = Vec::with_capacity(routes.len());
        for route in routes {
            validate_path(&route.path)?;
            for method in &route.methods {
                if *method != Method::HEAD && !seen.insert((method.clone(), route.path.clone())) {
                    return Err(Error::internal(format!(
                        "route {method} {} is declared twice",
                        route.path
                    )));
                }
            }
            if let Some(name) = &route.name
                && !names.insert(name.clone())
            {
                return Err(Error::internal(format!(
                    "route name `{name}` is used twice"
                )));
            }
            let route_key = format!(
                "{} {}",
                route
                    .methods
                    .iter()
                    .map(Method::as_str)
                    .collect::<Vec<_>>()
                    .join("|"),
                route.path
            );
            let middleware = route
                .middleware
                .iter()
                .map(|alias| {
                    if let Some(m) = aliases.get(alias) {
                        return Ok(m.clone());
                    }
                    // A family alias with arguments (`throttle:5,1`, `auth:web`): its factory checks them.
                    if let Some((prefix, args)) = alias.split_once(':')
                        && let Some(make) = families.get(prefix)
                    {
                        return make(args, &route_key);
                    }
                    Err(Error::internal(format!(
                        "route {} uses unknown middleware `{alias}`",
                        route.path
                    )))
                })
                .collect::<Result<Vec<_>>>()?;
            out.push((route, middleware));
        }
        Ok(Self { routes: out })
    }

    pub(crate) fn infos(&self) -> Vec<RouteInfo> {
        self.routes
            .iter()
            .map(|(r, _)| RouteInfo {
                methods: r.methods.iter().map(ToString::to_string).collect(),
                path: r.path.clone(),
                name: r.name.clone(),
                middleware: r.middleware.clone(),
                api: r.kind == Kind::Api,
            })
            .collect()
    }

    pub(crate) fn names(&self) -> HashMap<String, String> {
        self.routes
            .iter()
            .filter_map(|(r, _)| r.name.clone().map(|n| (n, r.path.clone())))
            .collect()
    }

    /// Whether any route comes from `routes/web.rs` (and so needs sessions).
    pub(crate) fn has_web(&self) -> bool {
        self.routes.iter().any(|(r, _)| r.kind == Kind::Web)
    }

    /// The Axum router with every route and its middleware; `web` (sessions, CSRF) wraps
    /// every web route outside its own middleware, and `web_middleware` runs between the two
    /// (the first one outermost).
    pub(crate) fn into_axum(
        self,
        web: Option<&ErasedMiddleware>,
        web_middleware: &[ErasedMiddleware],
    ) -> Result<axum::Router<App>> {
        let mut router = axum::Router::new();
        for (route, middleware) in self.routes {
            let mut handler = route.handler;
            let wrap = |m: &ErasedMiddleware, h: MethodRouter<App>| {
                if route.any {
                    m.wrap_any_route(h)
                } else {
                    m.wrap_route(h)
                }
            };
            // The last alias wraps first, so the first alias is outermost and runs first.
            for m in middleware.iter().rev() {
                handler = wrap(m, handler);
            }
            if route.kind == Kind::Web {
                for m in web_middleware.iter().rev() {
                    handler = wrap(m, handler);
                }
                if let Some(web) = web {
                    handler = wrap(web, handler);
                }
            }
            let path = route.path;
            let current = router;
            // Paths are validated above; this only guards against an Axum rule we missed.
            router =
                std::panic::catch_unwind(AssertUnwindSafe(move || current.route(&path, handler)))
                    .map_err(|_| Error::internal("a route was rejected by the router"))?;
        }
        Ok(router)
    }
}

/// Add the framework's health route, `GET /up`, unless the app declares `GET /up` itself.
/// It is an API-kind route, so it runs without sessions and CSRF.
pub(crate) fn add_health_route(routes: &mut Vec<Route>) {
    if routes
        .iter()
        .any(|r| r.path == "/up" && r.methods.contains(&Method::GET))
    {
        return;
    }
    let mut router = Router::new(Kind::Api, "");
    router.get("/up", health);
    routes.extend(router.into_routes());
}

/// `200 OK`: the process is up and serving requests. Never cached, so a load balancer or
/// uptime monitor always reaches the app.
async fn health() -> impl axum::response::IntoResponse {
    ([(http::header::CACHE_CONTROL, "no-store")], "OK")
}

fn validate_path(path: &str) -> Result<()> {
    if !path.starts_with('/') {
        return Err(Error::internal(format!(
            "route path `{path}` must start with `/`"
        )));
    }
    for segment in path.split('/') {
        if segment.starts_with(':') || segment.starts_with('*') {
            return Err(Error::internal(format!(
                "route path `{path}`: write parameters as `{{name}}`, not `{segment}`"
            )));
        }
        let opens = segment.matches('{').count();
        let closes = segment.matches('}').count();
        if opens != closes || opens > 1 {
            return Err(Error::internal(format!(
                "route path `{path}`: invalid parameter in `{segment}`"
            )));
        }
    }
    Ok(())
}

/// Join a prefix and a path into one normalised path.
fn join(prefix: &str, path: &str) -> String {
    let joined = format!(
        "/{}/{}",
        prefix.trim_matches('/'),
        path.trim_start_matches('/')
    );
    let mut out = String::with_capacity(joined.len());
    for c in joined.chars() {
        if !(c == '/' && out.ends_with('/')) {
            out.push(c);
        }
    }
    if out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// A simple English singular, enough for resource placeholders.
pub(crate) fn singular(word: &str) -> String {
    if let Some(stem) = word.strip_suffix("ies") {
        format!("{stem}y")
    } else if word.ends_with("sses")
        || word.ends_with("xes")
        || word.ends_with("ches")
        || word.ends_with("shes")
    {
        word.get(..word.len() - 2).unwrap_or(word).to_owned()
    } else if let Some(stem) = word.strip_suffix('s')
        && !word.ends_with("ss")
    {
        stem.to_owned()
    } else {
        word.to_owned()
    }
}

/// Fill `{param}` placeholders; leftover params become the query string.
pub(crate) fn fill_path(path: &str, params: &[(&str, &str)]) -> Result<String> {
    let mut used = HashSet::new();
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        let (before, after) = rest.split_at(open);
        out.push_str(before);
        let close = after
            .find('}')
            .ok_or_else(|| Error::internal(format!("invalid route path `{path}`")))?;
        let key = after.get(1..close).unwrap_or("");
        let key = key.trim_start_matches('*');
        let value = params
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
            .ok_or_else(|| {
                Error::internal(format!("route `{path}` needs the parameter `{key}`"))
            })?;
        used.insert(key);
        out.push_str(&encode_segment(value));
        rest = after.get(close + 1..).unwrap_or("");
    }
    out.push_str(rest);
    let query: Vec<(&str, &str)> = params
        .iter()
        .filter(|(k, _)| !used.contains(k))
        .copied()
        .collect();
    if !query.is_empty() {
        out.push('?');
        out.push_str(&serde_urlencoded::to_string(query).map_err(Error::other)?);
    }
    Ok(out)
}

fn encode_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn h() {}

    fn web() -> Router {
        Router::new(Kind::Web, "")
    }

    #[test]
    fn joins_paths() {
        assert_eq!(join("", "/"), "/");
        assert_eq!(join("", ""), "/");
        assert_eq!(join("/api", "/"), "/api");
        assert_eq!(join("/admin/", "/users/"), "/admin/users");
        assert_eq!(join("admin", "users/{id}"), "/admin/users/{id}");
    }

    #[test]
    fn singulars() {
        assert_eq!(singular("posts"), "post");
        assert_eq!(singular("categories"), "category");
        assert_eq!(singular("boxes"), "box");
        assert_eq!(singular("classes"), "class");
        assert_eq!(singular("news"), "new");
        assert_eq!(singular("glass"), "glass");
        assert_eq!(singular("sheep"), "sheep");
    }

    #[test]
    fn groups_prefix_paths_names_and_middleware() {
        let mut r = web();
        r.group("/admin", |r| {
            r.get("/", h).name("dashboard").middleware("log");
            r.get("/users", h);
        })
        .name("admin.")
        .middleware("auth");
        let routes = r.into_routes();
        assert_eq!(routes[0].path, "/admin");
        assert_eq!(routes[0].name.as_deref(), Some("admin.dashboard"));
        assert_eq!(routes[0].middleware, ["auth", "log"]);
        assert_eq!(routes[1].path, "/admin/users");
        assert_eq!(routes[1].name, None);
    }

    #[test]
    fn resource_routes() {
        let mut r = web();
        r.resource("/admin/blog-posts")
            .index(h)
            .create(h)
            .store(h)
            .show(h)
            .edit(h)
            .update(h)
            .destroy(h)
            .middleware("auth");
        let table = RouteTable::new(
            r.into_routes(),
            &{
                let mut m = HashMap::new();
                m.insert(
                "auth".to_owned(),
                ErasedMiddleware::new(
                    |req: crate::middleware::Request, next: crate::middleware::Next| async move {
                        next.run(req).await
                    },
                ),
            );
                m
            },
            &HashMap::new(),
        )
        .unwrap();
        let infos = table.infos();
        let rows: Vec<(String, String, String)> = infos
            .iter()
            .map(|i| (i.methods.join("|"), i.path.clone(), i.name.clone().unwrap()))
            .collect();
        let expect = [
            ("GET|HEAD", "/admin/blog-posts", "admin.blog-posts.index"),
            (
                "GET|HEAD",
                "/admin/blog-posts/create",
                "admin.blog-posts.create",
            ),
            ("POST", "/admin/blog-posts", "admin.blog-posts.store"),
            (
                "GET|HEAD",
                "/admin/blog-posts/{blog_post}",
                "admin.blog-posts.show",
            ),
            (
                "GET|HEAD",
                "/admin/blog-posts/{blog_post}/edit",
                "admin.blog-posts.edit",
            ),
            (
                "PUT|PATCH",
                "/admin/blog-posts/{blog_post}",
                "admin.blog-posts.update",
            ),
            (
                "DELETE",
                "/admin/blog-posts/{blog_post}",
                "admin.blog-posts.destroy",
            ),
        ];
        for (row, (m, p, n)) in rows.iter().zip(expect) {
            assert_eq!(row, &(m.to_owned(), p.to_owned(), n.to_owned()));
        }
        assert!(infos.iter().all(|i| i.middleware == ["auth"]));
        let _router = table.into_axum(None, &[]).unwrap();
    }

    #[test]
    fn any_routes_take_middleware() {
        // axum's `route_layer` panics on a method router without per-method routes.
        let mut r = web();
        r.any("/hook", h).middleware("auth");
        let pass = ErasedMiddleware::new(
            |req: crate::middleware::Request, next: crate::middleware::Next| async move {
                next.run(req).await
            },
        );
        let aliases = HashMap::from([("auth".to_owned(), pass.clone())]);
        let table = RouteTable::new(r.into_routes(), &aliases, &HashMap::new()).unwrap();
        assert!(
            table
                .into_axum(Some(&pass), std::slice::from_ref(&pass))
                .is_ok()
        );
    }

    #[test]
    fn rejects_bad_tables() {
        let none = HashMap::new();
        let mut r = web();
        r.get("/a", h);
        r.get("/a", h);
        assert!(RouteTable::new(r.into_routes(), &none, &HashMap::new()).is_err());

        let mut r = web();
        r.get("/a", h).name("x");
        r.get("/b", h).name("x");
        assert!(RouteTable::new(r.into_routes(), &none, &HashMap::new()).is_err());

        let mut r = web();
        r.get("/a", h).middleware("nope");
        assert!(RouteTable::new(r.into_routes(), &none, &HashMap::new()).is_err());

        let mut r = web();
        r.get("/a/:id", h);
        assert!(RouteTable::new(r.into_routes(), &none, &HashMap::new()).is_err());

        let mut r = web();
        r.get("/a", h);
        r.post("/a", h);
        assert!(RouteTable::new(r.into_routes(), &none, &HashMap::new()).is_ok());
    }

    #[test]
    fn fills_paths() {
        assert_eq!(
            fill_path("/p/{id}", &[("id", "a/b c")]).unwrap(),
            "/p/a%2Fb%20c"
        );
        assert_eq!(
            fill_path("/p/{id}/e", &[("id", "1"), ("q", "x&y")]).unwrap(),
            "/p/1/e?q=x%26y"
        );
        assert!(fill_path("/p/{id}", &[]).is_err());
        assert_eq!(fill_path("/f/{*rest}", &[("rest", "a")]).unwrap(), "/f/a");
    }
}
