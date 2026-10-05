//! Middleware: functions that wrap request handling.
//!
//! A middleware is an `async fn(Request, Next) -> impl IntoResponse`. Register it under an
//! alias with [`AppBuilder::middleware`](crate::AppBuilder::middleware) and attach it to
//! routes or groups by alias, or run it on every request with
//! [`AppBuilder::global_middleware`](crate::AppBuilder::global_middleware).

use std::future::Future;
use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use http::{Method, header};

use crate::app::App;

pub use axum::extract::Request;
pub use axum::middleware::Next;

/// Something that can wrap request handling. Implemented for every
/// `Fn(Request, Next) -> impl Future<Output = impl IntoResponse>` that is `Clone + Send +
/// Sync`.
pub trait Middleware: Clone + Send + Sync + 'static {
    /// Handle the request, usually calling `next.run(req).await` somewhere.
    fn handle(&self, req: Request, next: Next) -> impl Future<Output = Response> + Send;
}

impl<F, Fut, R> Middleware for F
where
    F: Fn(Request, Next) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
    R: IntoResponse,
{
    fn handle(&self, req: Request, next: Next) -> impl Future<Output = Response> + Send {
        let fut = self(req, next);
        async move { fut.await.into_response() }
    }
}

type RouteLayer = Arc<dyn Fn(MethodRouter<App>) -> MethodRouter<App> + Send + Sync>;
type RouterLayer = Arc<dyn Fn(axum::Router) -> axum::Router + Send + Sync>;

/// A middleware with its type erased: what the factory of a middleware family returns
/// ([`AppBuilder::middleware_family`](crate::AppBuilder::middleware_family)).
#[derive(Clone)]
pub struct BoxedMiddleware(pub(crate) ErasedMiddleware);

impl BoxedMiddleware {
    /// Erase `middleware`'s type.
    pub fn new(middleware: impl Middleware) -> Self {
        Self(ErasedMiddleware::new(middleware))
    }
}

impl std::fmt::Debug for BoxedMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoxedMiddleware").finish_non_exhaustive()
    }
}

/// A middleware family's factory: the alias's arguments and the route ("METHODS /pattern") in, the middleware out.
pub(crate) type Family =
    Arc<dyn Fn(&str, &str) -> crate::Result<ErasedMiddleware> + Send + Sync + 'static>;

/// A middleware with its type erased, ready to wrap a route or the whole router.
#[derive(Clone)]
pub(crate) struct ErasedMiddleware {
    route: RouteLayer,
    any_route: RouteLayer,
    router: RouterLayer,
}

impl ErasedMiddleware {
    pub(crate) fn new<M: Middleware>(middleware: M) -> Self {
        let for_route = middleware.clone();
        let route: RouteLayer = Arc::new(move |method_router: MethodRouter<App>| {
            let m = for_route.clone();
            method_router.route_layer(axum::middleware::from_fn(
                move |req: Request, next: Next| {
                    let m = m.clone();
                    async move { m.handle(req, next).await }
                },
            ))
        });
        // `Router::any` routes have no per-method endpoints (axum panics on `route_layer`
        // there): the middleware wraps the whole method router, which answers every method.
        let for_any = middleware.clone();
        let any_route: RouteLayer = Arc::new(move |method_router: MethodRouter<App>| {
            let m = for_any.clone();
            method_router.layer(axum::middleware::from_fn(
                move |req: Request, next: Next| {
                    let m = m.clone();
                    async move { m.handle(req, next).await }
                },
            ))
        });
        let router: RouterLayer = Arc::new(move |router: axum::Router| {
            let m = middleware.clone();
            router.layer(axum::middleware::from_fn(
                move |req: Request, next: Next| {
                    let m = m.clone();
                    async move { m.handle(req, next).await }
                },
            ))
        });
        Self {
            route,
            any_route,
            router,
        }
    }

    pub(crate) fn wrap_route(&self, method_router: MethodRouter<App>) -> MethodRouter<App> {
        (self.route)(method_router)
    }

    /// [`wrap_route`](Self::wrap_route) for a `Router::any` route.
    pub(crate) fn wrap_any_route(&self, method_router: MethodRouter<App>) -> MethodRouter<App> {
        (self.any_route)(method_router)
    }

    pub(crate) fn wrap_router(&self, router: axum::Router) -> axum::Router {
        (self.router)(router)
    }
}

/// Rewrite a `POST` into `PUT`, `PATCH` or `DELETE` before routing, from the
/// `X-HTTP-Method-Override` header or a `_method` field in a URL-encoded or multipart form
/// body (what Mold's `@method('PUT')` writes).
pub(crate) async fn method_override(req: Request, limit: usize) -> Request {
    if req.method() != Method::POST {
        return req;
    }
    if let Some(method) = req
        .headers()
        .get("x-http-method-override")
        .and_then(|v| v.to_str().ok())
        .and_then(spoofable)
    {
        let (mut parts, body) = req.into_parts();
        parts.method = method;
        return Request::from_parts(parts, body);
    }
    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if let Some(boundary) = crate::multipart::boundary(content_type) {
        // Only the start of the body is read (at most `limit` bytes); the rest streams on.
        let (mut parts, body) = req.into_parts();
        let (body, found) =
            crate::multipart::peek_fields(body, &boundary, &["_method"], limit).await;
        if let Some(method) = found.first().and_then(|(_, v)| spoofable(v)) {
            parts.method = method;
        }
        return Request::from_parts(parts, body);
    }
    let is_form = content_type.starts_with("application/x-www-form-urlencoded");
    if !is_form {
        return req;
    }
    let (mut parts, body) = req.into_parts();
    // Reading the body here is bounded by the body limit; an oversized body is passed on
    // empty and the handler's own extractor reports the problem.
    let bytes = match axum::body::to_bytes(body, limit).await {
        Ok(bytes) => bytes,
        Err(_) => return Request::from_parts(parts, axum::body::Body::empty()),
    };
    if let Some(method) = serde_urlencoded::from_bytes::<Vec<(String, String)>>(&bytes)
        .ok()
        .and_then(|fields| {
            fields
                .into_iter()
                .find(|(k, _)| k == "_method")
                .and_then(|(_, v)| spoofable(&v))
        })
    {
        parts.method = method;
    }
    Request::from_parts(parts, axum::body::Body::from(bytes))
}

fn spoofable(method: &str) -> Option<Method> {
    match method.to_ascii_uppercase().as_str() {
        "PUT" => Some(Method::PUT),
        "PATCH" => Some(Method::PATCH),
        "DELETE" => Some(Method::DELETE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn form(body: &'static str) -> Request {
        Request::builder()
            .method(Method::POST)
            .uri("/")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn form_field_spoofs_method_and_keeps_body() {
        let req = method_override(form("_method=PUT&title=x"), 1024).await;
        assert_eq!(req.method(), Method::PUT);
        let body = axum::body::to_bytes(req.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"_method=PUT&title=x");
    }

    #[tokio::test]
    async fn only_put_patch_delete_are_spoofable() {
        let req = method_override(form("_method=GET"), 1024).await;
        assert_eq!(req.method(), Method::POST);
    }

    #[tokio::test]
    async fn header_spoofs_method() {
        let req = Request::builder()
            .method(Method::POST)
            .header("x-http-method-override", "delete")
            .body(Body::empty())
            .unwrap();
        assert_eq!(method_override(req, 10).await.method(), Method::DELETE);
    }
}
