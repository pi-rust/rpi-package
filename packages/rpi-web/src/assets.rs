//! Complete frontend embedded in both the extension and standalone program.
use axum::{
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};

include!(concat!(env!("OUT_DIR"), "/frontend.rs"));

fn response(name: &str) -> Response {
    let Some((_, bytes)) = FILES.iter().find(|(path, _)| *path == name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = match name.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    };
    ([(header::CONTENT_TYPE, mime)], *bytes).into_response()
}

pub async fn asset(uri: Uri) -> Response {
    response(&format!("assets/{}", uri.path().trim_start_matches('/')))
}

pub async fn favicon() -> Response {
    response("favicon.svg")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frontend_is_complete_and_paths_cannot_escape_the_inventory() {
        assert_eq!(response("index.html").status(), StatusCode::OK);
        assert_eq!(
            response("favicon.svg").headers()[header::CONTENT_TYPE],
            "image/svg+xml"
        );
        let html =
            std::str::from_utf8(FILES.iter().find(|(n, _)| *n == "index.html").unwrap().1).unwrap();
        for fragment in html.split('"').filter(|s| s.starts_with("/assets/")) {
            assert_eq!(
                response(fragment.trim_start_matches('/')).status(),
                StatusCode::OK
            );
        }
        for path in ["../Cargo.toml", "assets/../../.env", "assets/missing.js"] {
            assert_eq!(response(path).status(), StatusCode::NOT_FOUND);
        }
    }
}
