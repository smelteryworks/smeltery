//! The framework error type and how errors turn into responses.

use std::sync::Arc;

use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};

use crate::html::escape;

/// `Result` with [`Error`] as the error type: what handlers usually return.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// An error a handler (or the framework) returns.
///
/// It becomes an HTTP response: an HTML error page, or `{"error": "…"}` JSON when the
/// request asked for JSON. With `APP_DEBUG=true` the page shows the error message; in
/// production it shows only the status text. Internal errors are logged.
///
/// ```
/// use smeltery_core::{Error, Result};
///
/// async fn show(id: u64) -> Result<String> {
///     if id == 0 {
///         return Err(Error::not_found());
///     }
///     Ok(format!("post {id}"))
/// }
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A response with this status and a message safe to show to the user.
    #[error("{message}")]
    Http {
        /// The status code.
        status: StatusCode,
        /// The message shown in the page or the JSON body.
        message: String,
    },
    /// A failure inside the app: 500, details only in the log and in debug pages.
    #[error("{0}")]
    Internal(String),
    /// A failure from another library: 500, like [`Error::Internal`].
    #[error(transparent)]
    Other(Box<dyn std::error::Error + Send + Sync + 'static>),
    /// Input failed validation (or a login was throttled): JSON clients get 422 (429)
    /// `{"message": …, "errors": {…}}`; on web routes other requests are redirected back with
    /// the messages and the old input flashed to the session.
    #[error("{}", .0.message)]
    Validation(Box<crate::validation::Invalid>),
}

impl Error {
    /// An error with any status and a message shown to the user.
    pub fn http(status: StatusCode, message: impl Into<String>) -> Self {
        Self::Http {
            status,
            message: message.into(),
        }
    }

    /// 404 Not Found.
    pub fn not_found() -> Self {
        Self::http(StatusCode::NOT_FOUND, "Not Found")
    }

    /// 403 Forbidden.
    pub fn forbidden() -> Self {
        Self::http(StatusCode::FORBIDDEN, "Forbidden")
    }

    /// 401 Unauthorized.
    pub fn unauthorized() -> Self {
        Self::http(StatusCode::UNAUTHORIZED, "Unauthorized")
    }

    /// 400 Bad Request with a message.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::http(StatusCode::BAD_REQUEST, message)
    }

    /// 500 with a message for the log.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal(message.into())
    }

    /// A validation failure with one message on `field`, e.g. after a failed login:
    /// `Error::validation("email", "These credentials do not match our records.")`.
    pub fn validation(field: impl Into<String>, message: impl Into<String>) -> Self {
        let mut errors = crate::validation::ValidationErrors::new();
        errors.add(field, message);
        Self::Validation(Box::new(crate::validation::Invalid::new(
            errors,
            crate::validation::Input::new(),
        )))
    }

    /// Wrap any error as a 500.
    pub fn other(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Other(Box::new(error))
    }

    /// The HTTP status this error responds with.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Http { status, .. } => *status,
            Self::Internal(_) | Self::Other(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Validation(invalid) => invalid.status,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::other(error)
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::other(error)
    }
}

/// What the error-page middleware needs: kept in the response extensions.
#[derive(Clone, Debug)]
pub(crate) struct ErrorReport {
    pub(crate) status: StatusCode,
    /// The message for the user (always safe to show).
    pub(crate) public: String,
    /// The full message, shown only in debug mode.
    pub(crate) detail: Arc<str>,
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        if let Self::Validation(invalid) = self {
            return invalid.into_response();
        }
        let status = self.status();
        let detail: Arc<str> = Arc::from(self.to_string());
        let public = match &self {
            Self::Http { message, .. } => message.clone(),
            Self::Validation(_) => String::new(),
            Self::Internal(_) | Self::Other(_) => {
                tracing::error!(error = %detail, "internal error");
                status
                    .canonical_reason()
                    .unwrap_or("Server Error")
                    .to_owned()
            }
        };
        let report = ErrorReport {
            status,
            public: public.clone(),
            detail,
        };
        // A plain body for anyone calling the handler without the middleware stack.
        let mut response = (status, public).into_response();
        response.extensions_mut().insert(report);
        response
    }
}

/// Whether the client asks for JSON: its `Accept` header names `application/json` or a `+json` type. Error answers,
/// `auth` and `verified` follow it.
pub fn wants_json(headers: &http::HeaderMap) -> bool {
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    accept.contains("application/json") || accept.contains("+json")
}

/// Render the error page or JSON body for `status`.
pub(crate) fn render_error(
    status: StatusCode,
    public: &str,
    detail: Option<&str>,
    json: bool,
) -> Response {
    if json {
        let body = match detail {
            Some(detail) => serde_json::json!({ "error": public, "detail": detail }),
            None => serde_json::json!({ "error": public }),
        };
        return (status, axum::Json(body)).into_response();
    }
    let title = format!("{} {}", status.as_u16(), reason(status));
    let detail_html = match detail {
        Some(detail) if detail != public => {
            format!("<pre class=\"detail\">{}</pre>", escape(detail))
        }
        _ => String::new(),
    };
    let html = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title}</title>\n<style>{ERROR_CSS}</style>\n</head>\n<body>\n<main>\n\
         <h1>{title}</h1>\n<p>{message}</p>\n{detail_html}\n</main>\n</body>\n</html>\n",
        title = escape(&title),
        message = escape(public),
    );
    let mut response = (status, html).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response
}

/// The reason phrase, with 419 "Page Expired" for CSRF failures.
pub(crate) fn reason(status: StatusCode) -> &'static str {
    if status.as_u16() == 419 {
        return "Page Expired";
    }
    status.canonical_reason().unwrap_or("Error")
}

const ERROR_CSS: &str = "body{font-family:system-ui,sans-serif;background:#1c1917;color:#e7e5e4;\
margin:0;display:flex;min-height:100vh;align-items:center;justify-content:center}\
main{max-width:48rem;padding:2rem}h1{color:#f97316;margin:0 0 .5rem}\
pre.detail{background:#292524;padding:1rem;border-radius:.5rem;white-space:pre-wrap;\
overflow-x:auto}";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses() {
        assert_eq!(Error::not_found().status(), StatusCode::NOT_FOUND);
        assert_eq!(
            Error::internal("x").status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        let io = std::io::Error::other("disk");
        assert_eq!(Error::from(io).status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn internal_errors_hide_details_publicly() {
        let response = Error::internal("db password wrong").into_response();
        let report = response.extensions().get::<ErrorReport>().unwrap();
        assert_eq!(report.public, "Internal Server Error");
        assert_eq!(&*report.detail, "db password wrong");
    }

    #[test]
    fn json_detection() {
        let mut headers = http::HeaderMap::new();
        assert!(!wants_json(&headers));
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        assert!(wants_json(&headers));
    }
}
