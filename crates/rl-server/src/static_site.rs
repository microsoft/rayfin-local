//! `--static`: the built SPA served from the backend origin, with the SPA fallback to its index
//! document.

use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

use crate::StaticSite;
use crate::routes::{is_backend_path, json_error, not_found};

/// What a path segment can't hold unescaped (`/` separates the index document's folders).
const PATH: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

pub(crate) struct StaticFiles {
    dir: ServeDir,
    index: PathBuf,
    index_document: String,
}

impl StaticFiles {
    pub(crate) fn new(site: StaticSite) -> Self {
        let index_document = site.index_document.trim_start_matches(['/', '\\']);
        let index_document = if is_plain_relative(index_document) {
            index_document.to_owned()
        } else {
            tracing::warn!(
                index_document = %site.index_document,
                "the index document must be a path inside the static folder; using index.html"
            );
            "index.html".to_owned()
        };
        Self {
            // `respond` asks for a folder's index document itself; there are no listings.
            dir: ServeDir::new(&site.root).append_index_html_on_directories(false),
            index: site.root.join(&index_document),
            index_document,
        }
    }

    /// Serves a request no route matched as BaaS's `StaticWebsiteWorkflow.ServeContentAsync` does:
    /// a path whose last segment has no `.` gets its folder's index document, any other path the
    /// file itself; when that isn't there, every GET gets the root index document (client-side
    /// routing), whatever it accepts.
    pub(crate) async fn respond(&self, req: Request) -> Response {
        let method = req.method().clone();
        let path = req.uri().path().to_owned();
        if is_backend_path(&path) || (method != Method::GET && method != Method::HEAD) {
            return not_found(&method, &path);
        }
        let mut index_headers = req.headers().clone();

        // `ServeDir` rejects `..`, drive prefixes and Windows device names, so a request can't
        // leave the root.
        let req = if has_file_extension(&path) {
            req
        } else {
            match self.folder_index(req) {
                Some(req) => req,
                None => return self.root_index(method, index_headers).await,
            }
        };
        let file = self
            .dir
            .clone()
            .oneshot(req)
            .await
            .unwrap_or_else(|e| match e {});
        if file.status() != StatusCode::NOT_FOUND {
            return no_cache(file.map(Body::new));
        }

        index_headers.remove(header::RANGE);
        index_headers.remove(header::IF_RANGE);
        self.root_index(method, index_headers).await
    }

    /// The request for `<path>/<index document>`.
    fn folder_index(&self, mut req: Request) -> Option<Request> {
        let folder = req.uri().path().trim_end_matches('/');
        let document =
            utf8_percent_encode(&self.index_document.replace('\\', "/"), PATH).to_string();
        let uri = format!("{folder}/{document}").parse().ok()?;
        *req.uri_mut() = uri;
        Some(req)
    }

    async fn root_index(&self, method: Method, headers: HeaderMap) -> Response {
        let mut index_req = Request::new(Body::empty());
        *index_req.method_mut() = method;
        *index_req.headers_mut() = headers;
        let index = ServeFile::new(&self.index)
            .oneshot(index_req)
            .await
            .unwrap_or_else(|e| match e {});
        if index.status() == StatusCode::NOT_FOUND {
            return json_error(
                StatusCode::NOT_FOUND,
                "not_found",
                &format!(
                    "The static site has no {}. Build the app (staticHosting.buildCommand) and \
                     restart `rayfin-local serve --static`.",
                    self.index_document
                ),
            );
        }
        no_cache(index.map(Body::new))
    }
}

fn is_plain_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(':')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// BaaS's `HasFileExtension`: the last segment has a `.`.
fn has_file_extension(path: &str) -> bool {
    path.rsplit('/').next().unwrap_or_default().contains('.')
}

fn no_cache(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dot_in_the_last_segment_makes_a_file() {
        assert!(has_file_extension("/assets/app.js"));
        assert!(has_file_extension("/user/jane.doe"));
        assert!(!has_file_extension("/"));
        assert!(!has_file_extension("/threads/42"));
        assert!(!has_file_extension("/v1.2/docs"));
        assert!(!has_file_extension("/docs/"));
    }

    #[test]
    fn index_document_must_stay_inside_the_root() {
        assert!(is_plain_relative("index.html"));
        assert!(is_plain_relative("app/index.html"));
        assert!(!is_plain_relative("../index.html"));
        assert!(!is_plain_relative("C:/index.html"));
        assert!(!is_plain_relative(""));
    }
}
