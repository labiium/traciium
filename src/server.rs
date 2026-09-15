//! HTTP server: embedded single-page UI + two JSON endpoints.

use crate::format::FormatSpec;
use crate::trace::TraceFile;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

pub struct AppState {
    pub spec: FormatSpec,
    pub files: RwLock<Vec<TraceFile>>,
    pub format_path: String,
    pub source_path: PathBuf,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        .route("/api/meta", get(meta))
        .route("/api/reload", post(reload))
        .route("/api/trace/{file}", get(trace))
        .fallback(not_found)
        .with_state(state)
}

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE_CSS: &str = include_str!("../web/style.css");

async fn index() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        INDEX_HTML,
    )
        .into_response()
}

async fn app_js() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        APP_JS,
    )
        .into_response()
}

async fn style_css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        STYLE_CSS,
    )
        .into_response()
}

async fn meta(State(st): State<Arc<AppState>>) -> Response {
    let files_guard = st.files.read().unwrap();
    let files: Vec<Value> = files_guard
        .iter()
        .map(|f| {
            json!({
                "name": f.name,
                "path": f.path,
                "dir": f.dir,
                "bytes": f.bytes,
                "runs": f.runs.iter().map(|r| json!({
                    "id": r.id,
                    "label": r.label,
                    "stats": r.stats,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Json(json!({
        "format": st.spec,
        "format_path": st.format_path,
        "files": files,
    }))
    .into_response()
}

async fn reload(State(st): State<Arc<AppState>>) -> Response {
    let paths = match crate::collect_jsonl(&st.source_path) {
        Ok(paths) => paths,
        Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
    };
    let mut files = Vec::new();
    let mut skipped = 0usize;
    for path in paths {
        match crate::trace::load_file(&path.to_string_lossy(), files.len(), &st.spec) {
            Ok(mut file) => {
                file.runs.retain(|run| run.stats.events > 0);
                if file.runs.is_empty() {
                    skipped += 1;
                } else {
                    files.push(file);
                }
            }
            Err(_) => skipped += 1,
        }
    }
    if files.is_empty() {
        return (StatusCode::BAD_REQUEST, "no parseable .jsonl events found").into_response();
    }
    let count = files.len();
    *st.files.write().unwrap() = files;
    Json(json!({ "files": count, "skipped": skipped })).into_response()
}

async fn trace(State(st): State<Arc<AppState>>, Path(file): Path<usize>) -> Response {
    let files = st.files.read().unwrap();
    match files.get(file) {
        Some(f) => Json(f).into_response(),
        None => (StatusCode::NOT_FOUND, "no such file").into_response(),
    }
}

async fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}
