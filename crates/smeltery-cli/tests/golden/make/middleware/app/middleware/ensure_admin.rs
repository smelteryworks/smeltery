//! The `ensure_admin` middleware.

use smeltery::Response;
use smeltery::middleware::{Next, Request};

/// Runs around every request of the routes it is attached to.
pub async fn ensure_admin(req: Request, next: Next) -> Response {
    // Inspect or change `req` here; return early to stop the request.
    next.run(req).await
}
