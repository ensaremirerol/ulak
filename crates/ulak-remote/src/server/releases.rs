//! Hub release cache: the hub is the only party with a GitHub token (or ships the release in its
//! image, see `seed`). It fetches the latest release, verifies it with the release key (the same code ulakd runs), keeps the last two
//! versions under `<dir>/<version>/` and serves them to nodes over mTLS. Nodes verify again: the
//! hub is not trusted for integrity.
use super::hub::{now_ms, Hub, Peer};
use axum::{
    body::Body,
    extract::{Path as AxPath, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Extension, Json,
};
use ed25519_dalek::VerifyingKey;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use ulak_proto::{GitToken, HubToUi, NodeId};
use ulakd::update::{
    release::{parse_release, SemVer},
    verify,
};

pub const KEEP: usize = 2;
const MAX_ASSET: u64 = 200 * 1024 * 1024;
const MAX_SMALL: u64 = 1024 * 1024;
/// Minimum gap between fetches triggered by a UI `CheckUpdate`.
const MIN_GAP_MS: u64 = 60_000;
/// Requests per device per minute on `/api/releases/*`.
const PER_MIN: usize = 30;
const META: &str = "meta.json";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AssetInfo {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Cached {
    pub version: String,
    pub published_at: String,
    pub assets: Vec<AssetInfo>,
}

/// In-memory view of the cache; only verified versions are in `versions` (newest first).
#[derive(Default)]
pub(crate) struct CacheState {
    pub versions: Vec<Cached>,
    pub checked_ms: u64,
    pub attempt_ms: u64,
    pub error: Option<String>,
}

/// Body of `GET /api/releases/latest` and of the admin `releases` report.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Report {
    pub latest: Option<Cached>,
    pub versions: Vec<String>,
    pub checked_at: u64,
    pub error: Option<String>,
}

fn safe_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 128
        && !n.starts_with('.')
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// Archives served to devices: `ulakd-*` (nodes) and `ulak-*` (the app); never the hub's own.
fn is_tarball(n: &str) -> bool {
    (n.starts_with("ulakd-") || (n.starts_with("ulak-") && !n.starts_with("ulak-remote-")))
        && n.ends_with(".tar.gz")
}

/// Verifies every `ulakd-*.tar.gz` of an in-memory release; `files` are (name, bytes).
fn verify_all(key: &VerifyingKey, files: &[(String, Vec<u8>)]) -> Result<(), String> {
    let get = |n: &str| {
        files
            .iter()
            .find(|(f, _)| f == n)
            .map(|(_, b)| b.as_slice())
            .ok_or_else(|| format!("release has no {n}"))
    };
    let (sums, sums_sig) = (get("SHA256SUMS")?, get("SHA256SUMS.sig")?);
    let mut any = false;
    for (name, bytes) in files.iter().filter(|(n, _)| is_tarball(n)) {
        let sig = get(&format!("{name}.sig"))?;
        verify::verify_release(key, name, bytes, sig, sums, sums_sig).map_err(|e| e.to_string())?;
        any = true;
    }
    any.then_some(())
        .ok_or_else(|| "release has no ulakd or ulak archive".into())
}

/// Reads and verifies `<dir>/<version>`; `None` if anything is off.
fn load_version(dir: &Path, version: &str, key: &VerifyingKey) -> Option<Cached> {
    let vdir = dir.join(version);
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(vdir.join(META)).ok()?).ok()?;
    let mut files = Vec::new();
    for e in std::fs::read_dir(&vdir).ok()? {
        let e = e.ok()?;
        let name = e.file_name().into_string().ok()?;
        if name == META {
            continue;
        }
        if !safe_name(&name) || !e.file_type().ok()?.is_file() {
            return None;
        }
        files.push((name, std::fs::read(e.path()).ok()?));
    }
    verify_all(key, &files).ok()?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Some(Cached {
        version: version.to_owned(),
        published_at: meta["published_at"].as_str().unwrap_or_default().to_owned(),
        assets: files
            .iter()
            .map(|(name, b)| AssetInfo {
                name: name.clone(),
                size: b.len() as u64,
                sha256: verify::sha256_hex(b),
            })
            .collect(),
    })
}

/// Verified versions on disk, newest first; everything else beyond [`KEEP`] (and stale temp
/// dirs) is deleted.
pub(crate) fn scan_and_prune(dir: &Path, key: &VerifyingKey) -> Vec<Cached> {
    let mut names: Vec<(SemVer, String)> = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        match SemVer::parse(&name).filter(|_| !name.starts_with('v')) {
            Some(v) => names.push((v, name)),
            None if name.starts_with(".tmp-") => {
                let _ = std::fs::remove_dir_all(e.path());
            }
            None => {}
        }
    }
    names.sort_by_key(|n| std::cmp::Reverse(n.0));
    let mut kept = Vec::new();
    for (_, name) in names {
        let v = (kept.len() < KEEP)
            .then(|| load_version(dir, &name, key))
            .flatten();
        match v {
            Some(c) => kept.push(c),
            None => {
                let _ = std::fs::remove_dir_all(dir.join(&name));
            }
        }
    }
    kept
}

/// Copies bundled versions newer than everything cached into `dir`; `scan_and_prune` verifies
/// them afterwards like any other cache entry.
fn seed(bundle: &Path, dir: &Path) {
    let ver = |n: &str| SemVer::parse(n).filter(|_| !n.starts_with('v'));
    let newest = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| ver(&e.file_name().to_string_lossy()))
        .max();
    for e in std::fs::read_dir(bundle).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if ver(&name).is_none_or(|v| newest.is_some_and(|n| v <= n)) {
            continue;
        }
        let tmp = dir.join(format!(".tmp-{name}-{}", std::process::id()));
        let copy = || -> std::io::Result<()> {
            std::fs::create_dir_all(&tmp)?;
            for f in std::fs::read_dir(e.path())?.flatten() {
                if f.file_type()?.is_file() {
                    std::fs::copy(f.path(), tmp.join(f.file_name()))?;
                }
            }
            std::fs::rename(&tmp, dir.join(&name))
        };
        if let Err(err) = copy() {
            let _ = std::fs::remove_dir_all(&tmp);
            eprintln!("ulak-remote: cannot copy bundled release {name}: {err}");
        }
    }
}

/// Fetches the latest release and stores it if it is new and verifies. `Ok(false)` = already cached.
fn fetch(h: &Hub, dir: &Path, key: &VerifyingKey, have: &[String]) -> Result<bool, String> {
    let token = h.release_token().map_err(|e| e.to_string())?;
    // No token but an image bundle: the bundle is the source; a new image brings the next release.
    if token.is_none() && h.cfg.bundled_releases.is_some() {
        return Ok(false);
    }
    let (gh, repo) = (&h.github, &h.cfg.release_repo);
    let json = gh
        .latest_release(repo, token.as_ref())
        .map_err(|e| e.to_string())?;
    let rel = parse_release(&json)
        .map_err(|e| e.to_string())?
        .ok_or("the latest release is a draft, a prerelease or has no vX.Y.Z tag")?;
    let version = rel.tag.trim_start_matches('v').to_owned();
    if have.contains(&version) {
        return Ok(false);
    }
    let raw: serde_json::Value = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    let wanted = |n: &str| {
        safe_name(n)
            && (is_tarball(n)
                || n == "SHA256SUMS"
                || n == "SHA256SUMS.sig"
                || n.strip_suffix(".sig").is_some_and(is_tarball))
    };
    let mut files = Vec::new();
    for a in rel.assets.iter().filter(|a| wanted(&a.name)) {
        let max = if is_tarball(&a.name) {
            MAX_ASSET
        } else {
            MAX_SMALL
        };
        if a.size > max {
            return Err(format!("asset {} is larger than {max} bytes", a.name));
        }
        let b = gh
            .asset(&a.url, token.as_ref(), max)
            .map_err(|e| e.to_string())?;
        if a.size != 0 && a.size != b.len() as u64 {
            return Err(format!("asset {} is truncated", a.name));
        }
        files.push((a.name.clone(), b));
    }
    verify_all(key, &files)?;
    let tmp = dir.join(format!(".tmp-{version}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(&tmp)?;
        for (n, b) in &files {
            std::fs::write(tmp.join(n), b)?;
        }
        let meta = serde_json::json!({ "published_at": raw["published_at"] });
        std::fs::write(tmp.join(META), meta.to_string())?;
        let _ = std::fs::remove_dir_all(dir.join(&version));
        std::fs::rename(&tmp, dir.join(&version))
    };
    write().map_err(|e| {
        let _ = std::fs::remove_dir_all(&tmp);
        format!("cannot store the release: {e}")
    })?;
    Ok(true)
}

fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(300).collect()
}

impl Hub {
    fn release_state(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.releases
            .lock()
            .expect("release cache lock is never held across a panic")
    }

    /// Startup: load what is on disk (verified), or record why nothing can be served.
    pub(crate) fn init_releases(&self) {
        let Some(dir) = &self.cfg.release_dir else {
            return;
        };
        let _ = std::fs::create_dir_all(dir);
        if let Some(b) = &self.cfg.bundled_releases {
            seed(b, dir);
        }
        let mut st = self.release_state();
        match &self.cfg.release_key {
            Some(k) => st.versions = scan_and_prune(dir, k),
            None => {
                st.error =
                    Some("no release signing key configured; the hub cannot cache releases".into());
            }
        }
    }

    /// Stored credential (preferred) or the in-memory env/file token; `None` = anonymous.
    fn release_token(&self) -> Result<Option<GitToken>, crate::ServerError> {
        match &self.cfg.release_credential {
            Some(id) => self.git_token(id).map(Some),
            None => Ok(self.cfg.release_token.clone()),
        }
    }

    pub fn release_report(&self) -> Report {
        let st = self.release_state();
        Report {
            latest: st.versions.first().cloned(),
            versions: st.versions.iter().map(|c| c.version.clone()).collect(),
            checked_at: st.checked_ms,
            error: st.error.clone(),
        }
    }

    pub(crate) fn release_frame(&self) -> HubToUi {
        let r = self.release_report();
        HubToUi::ReleaseCache {
            latest: r.latest.map(|l| l.version),
            checked_ms: r.checked_at,
            error: r.error,
        }
    }

    /// Fetches the latest release into the cache. `force` skips the 60 s gap; a refresh that is
    /// already running is not doubled. No-op when the cache is not configured.
    pub async fn refresh_releases(self: &Arc<Self>, force: bool) -> Report {
        let (Some(dir), Some(key)) = (
            self.cfg.release_dir.clone(),
            self.cfg.release_key.as_ref().cloned(),
        ) else {
            return self.release_report();
        };
        let Ok(_busy) = self.release_busy.try_lock() else {
            return self.release_report();
        };
        let now = now_ms();
        let have = {
            let mut st = self.release_state();
            if !force && now.saturating_sub(st.attempt_ms) < MIN_GAP_MS {
                drop(st);
                return self.release_report();
            }
            st.attempt_ms = now;
            st.versions
                .iter()
                .map(|c| c.version.clone())
                .collect::<Vec<_>>()
        };
        let h = self.clone();
        let r = tokio::task::spawn_blocking(move || {
            let r = fetch(&h, &dir, &key, &have);
            (r, scan_and_prune(&dir, &key))
        })
        .await;
        {
            let mut st = self.release_state();
            st.checked_ms = now_ms();
            match r {
                Ok((r, versions)) => {
                    st.versions = versions;
                    st.error = r.err().map(|e| clean(&e));
                }
                Err(_) => st.error = Some("release refresh task died".into()),
            }
        }
        if let Some(e) = self.release_state().error.as_ref() {
            eprintln!("ulak-remote: release refresh failed: {e}");
        }
        self.ui_send(self.release_frame());
        self.release_report()
    }

    fn release_rate_ok(&self, id: &NodeId, now: u64) -> bool {
        let mut m = self
            .release_hits
            .lock()
            .expect("rate map lock is never held across a panic");
        let hits = m.entry(id.clone()).or_default();
        hits.retain(|t| now.saturating_sub(*t) < 60_000);
        hits.push(now);
        hits.len() <= PER_MIN
    }

    /// Certificate-only auth (browser sessions and agent tokens have no cert) plus the rate limit.
    fn release_caller(&self, p: &Peer) -> Result<(), StatusCode> {
        let id = self.authorize(p)?;
        if self.release_rate_ok(&id, now_ms()) {
            Ok(())
        } else {
            Err(StatusCode::TOO_MANY_REQUESTS)
        }
    }
}

pub(crate) async fn latest(
    State(h): State<Arc<Hub>>,
    Extension(p): Extension<Peer>,
) -> Result<Response, StatusCode> {
    h.release_caller(&p)?;
    let r = h.release_report();
    match r.latest {
        Some(l) => Ok(Json(serde_json::json!({
            "version": l.version,
            "published_at": l.published_at,
            "assets": l.assets,
            "checked_at": r.checked_at,
        }))
        .into_response()),
        None => Ok((StatusCode::NOT_FOUND, "no release cached on the hub yet").into_response()),
    }
}

fn content_type(name: &str) -> &'static str {
    if name.ends_with(".tar.gz") {
        "application/gzip"
    } else if name == "SHA256SUMS" {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

pub(crate) async fn asset(
    State(h): State<Arc<Hub>>,
    Extension(p): Extension<Peer>,
    AxPath((version, name)): AxPath<(String, String)>,
) -> Result<Response, StatusCode> {
    h.release_caller(&p)?;
    // Only names the verified cache lists: no path is ever built from the request alone.
    let dir: PathBuf = h.cfg.release_dir.clone().ok_or(StatusCode::NOT_FOUND)?;
    let size = h
        .release_state()
        .versions
        .iter()
        .find(|c| c.version == version)
        .and_then(|c| c.assets.iter().find(|a| a.name == name))
        .map(|a| a.size)
        .ok_or(StatusCode::NOT_FOUND)?;
    let f = tokio::fs::File::open(dir.join(&version).join(&name))
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(f));
    Ok((
        [
            (header::CONTENT_TYPE, content_type(&name).to_owned()),
            (header::CONTENT_LENGTH, size.to_string()),
        ],
        body,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    #[test]
    fn serves_node_and_app_archives_only() {
        for n in [
            "ulakd-aarch64-unknown-linux-gnu.tar.gz",
            "ulak-x86_64-pc-windows-msvc.tar.gz",
            "ulak-aarch64-apple-darwin.tar.gz",
        ] {
            assert!(super::is_tarball(n), "{n}");
        }
        for n in [
            "ulak-remote-x86_64-unknown-linux-gnu.tar.gz",
            "ulak-x86_64-pc-windows-msvc.zip",
            "SHA256SUMS",
        ] {
            assert!(!super::is_tarball(n), "{n}");
        }
    }
}
