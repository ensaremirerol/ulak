//! Test helper for scripts/test-updater-docker.sh: a plain-http stand-in for the GitHub releases API.
//! Usage: fake_github <port> <public base url> <asset dir>
//! Serves `/repos/o/r/releases/latest` (assets point back at `<public base>/assets/<name>`) and
//! `/assets/<name>`, which, like GitHub's asset API, wants `Accept: application/octet-stream`.
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    routing::get,
    Json, Router,
};
use std::{path::PathBuf, sync::Arc};

struct Cfg {
    base: String,
    dir: PathBuf,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    if a.len() != 4 {
        return Err("usage: fake_github <port> <public base url> <asset dir>".into());
    }
    let cfg = Arc::new(Cfg {
        base: a[2].trim_end_matches('/').to_owned(),
        dir: PathBuf::from(&a[3]),
    });
    let app = Router::new()
        .route("/repos/o/r/releases/latest", get(latest))
        .route("/assets/{name}", get(asset))
        .with_state(cfg);
    let l = tokio::net::TcpListener::bind(("0.0.0.0", a[1].parse()?)).await?;
    println!("fake_github listening on {}", l.local_addr()?);
    axum::serve(l, app).await?;
    Ok(())
}

async fn latest(State(c): State<Arc<Cfg>>) -> Json<serde_json::Value> {
    let mut assets = vec![];
    for e in std::fs::read_dir(&c.dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let size = e.metadata().map_or(0, |m| m.len());
        assets.push(serde_json::json!({"name": name, "size": size, "url": format!("{}/assets/{name}", c.base)}));
    }
    Json(
        serde_json::json!({"tag_name": "v99.0.0", "draft": false, "prerelease": false, "assets": assets}),
    )
}

async fn asset(
    State(c): State<Arc<Cfg>>,
    Path(name): Path<String>,
    h: HeaderMap,
) -> Result<Vec<u8>, StatusCode> {
    let octet = h
        .get(header::ACCEPT)
        .is_some_and(|v| v == "application/octet-stream");
    if !octet || name.contains('/') || name.contains("..") {
        return Err(StatusCode::NOT_ACCEPTABLE);
    }
    std::fs::read(c.dir.join(name)).map_err(|_| StatusCode::NOT_FOUND)
}
