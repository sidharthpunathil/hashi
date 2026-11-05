//! Static pages for people you share with: the invite claim page and "My access".
//!
//! The files are compiled into the binary. They load nothing from other
//! origins, and the CSP forbids inline script, so a page can only run
//! `hashi.js` and only talk back to this server.

use axum::Router;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::api::AppState;

const CLAIM_HTML: &str = include_str!("web/claim.html");
const ME_HTML: &str = include_str!("web/me.html");
const CSS: &str = include_str!("web/hashi.css");
const JS: &str = include_str!("web/hashi.js");

/// Content Security Policy for every page and asset.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; \
     connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; \
     frame-ancestors 'none'";

fn asset(body: &'static str, content_type: &'static str) -> Response {
    let mut res = body.into_response();
    let h = res.headers_mut();
    // The CSP itself is added to every response by the API's header layer.
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    res
}

/// Routes for the pages and their assets.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/claim/{code}",
            get(|| async { asset(CLAIM_HTML, "text/html; charset=utf-8") }),
        )
        .route(
            "/me",
            get(|| async { asset(ME_HTML, "text/html; charset=utf-8") }),
        )
        .route(
            "/assets/hashi.css",
            get(|| async { asset(CSS, "text/css; charset=utf-8") }),
        )
        .route(
            "/assets/hashi.js",
            get(|| async { asset(JS, "text/javascript; charset=utf-8") }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_never_use_inner_html() {
        assert!(!JS.contains("innerHTML"));
        assert!(!JS.contains("insertAdjacentHTML"));
    }

    #[test]
    fn pages_have_no_inline_script() {
        for page in [CLAIM_HTML, ME_HTML] {
            assert_eq!(page.matches("<script").count(), 1);
            assert!(page.contains("<script src=\"/assets/hashi.js\"></script>"));
        }
    }
}
