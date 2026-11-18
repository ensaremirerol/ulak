//! Self-update: check the configured GitHub repo for a newer `vX.Y.Z` release, download the
//! signed `ulakd-<target>.tar.gz`, verify it, swap the running binary and exit so the
//! supervisor (systemd `Restart=always`, docker `--restart`) starts the new one.
//!
//! Installing immediately is fine even with agents running: they live in tmux, not in this process.
pub mod config;
pub mod http;
pub mod hub;
pub mod install;
pub mod release;
pub mod trust;
pub mod verify;

use crate::errors::UpdateError;
use config::UpdateConfig;
use ed25519_dalek::VerifyingKey;
use http::{origin, Http};
use release::{Release, SemVer};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{watch, Mutex};
use ulak_proto::{UpdateState, VersionInfo};

/// Exit status after a successful install (EX_TEMPFAIL): "restart me".
pub const RESTART_EXIT: i32 = 75;
pub const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;
const MAX_SMALL: u64 = 1024 * 1024;
const OCTET: &str = "application/octet-stream";

/// This build's `VersionInfo` (version from Cargo, git sha from build.rs).
pub fn version_info() -> VersionInfo {
    VersionInfo::current(trust::build_version(), env!("ULAK_GIT_SHA"))
        .expect("crate version and git sha are short printable strings")
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// A panicking or cancelled blocking task is a failed update, never a dead daemon.
fn task_failed(what: &str, e: &tokio::task::JoinError) -> UpdateError {
    eprintln!("ulakd: update {what} task died: {e}");
    UpdateError::Http(format!("{what} task died: {e}"))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    /// Newer release exists, not installed (check only, or auto-update off, or skipped).
    Available(String),
    /// Binary replaced; a restart is needed.
    Installed(String),
}

pub struct Updater {
    cfg: UpdateConfig,
    auto: bool,
    token: Option<String>,
    info: VersionInfo,
    exe: PathBuf,
    dir: PathBuf,
    /// Release archive prefix: `ulakd` (default) or `ulak` (the app).
    product: &'static str,
    key: Option<VerifyingKey>,
    pub http: Arc<Http>,
    state: watch::Sender<Option<UpdateState>>,
    busy: Mutex<()>,
    /// Called with `RESTART_EXIT` after a successful install (default: `process::exit`).
    pub exit: Box<dyn Fn(i32) + Send + Sync>,
    /// Time to let the `Restarting` frame reach the hub before exiting.
    pub restart_delay: Duration,
    /// Delay before the first automatic check.
    pub initial_delay: Duration,
}

impl Updater {
    /// `key` is `trust::release_key()` in production; `Err` means every install fails closed.
    pub fn new(
        cfg: UpdateConfig,
        auto: bool,
        token: Option<String>,
        info: VersionInfo,
        exe: PathBuf,
        dir: PathBuf,
        key: Result<VerifyingKey, UpdateError>,
    ) -> Self {
        Self {
            cfg,
            auto,
            token,
            info,
            exe,
            dir,
            product: "ulakd",
            key: key.ok(),
            http: Arc::new(Http::new()),
            state: watch::channel(None).0,
            busy: Mutex::new(()),
            exit: Box::new(|c| std::process::exit(c)),
            restart_delay: Duration::from_secs(1),
            initial_delay: Duration::from_secs(10),
        }
    }

    /// Update another product from the same release (the `ulak` app uses `"ulak"`).
    pub fn with_product(mut self, product: &'static str) -> Self {
        self.product = product;
        self
    }

    pub fn info(&self) -> &VersionInfo {
        &self.info
    }

    pub fn subscribe(&self) -> watch::Receiver<Option<UpdateState>> {
        self.state.subscribe()
    }

    fn set(&self, s: UpdateState) {
        self.state.send_replace(Some(s));
    }

    async fn get(
        &self,
        url: &str,
        token: bool,
        accept: &'static str,
        max: u64,
    ) -> Result<Vec<u8>, UpdateError> {
        let (http, url) = (Arc::clone(&self.http), url.to_owned());
        let token = token.then(|| self.token.clone()).flatten();
        tokio::task::spawn_blocking(move || http.get(&url, token.as_deref(), accept, max))
            .await
            .unwrap_or_else(|e| Err(task_failed("http", &e)))
    }

    fn use_hub(&self) -> bool {
        self.cfg.source(&self.dir) == config::Source::Hub
    }

    /// Blocking hub request (identity files are re-read each time).
    async fn hub<T: Send + 'static>(
        &self,
        f: impl FnOnce(hub::HubClient) -> Result<T, UpdateError> + Send + 'static,
    ) -> Result<T, UpdateError> {
        let dir = self.dir.clone();
        tokio::task::spawn_blocking(move || f(hub::HubClient::load(&dir)?))
            .await
            .unwrap_or_else(|e| Err(task_failed("hub http", &e)))
    }

    async fn latest(&self) -> Result<Option<Release>, UpdateError> {
        if self.use_hub() {
            return self.hub(|c| c.latest()).await.map(Some);
        }
        let url = format!(
            "{}/repos/{}/releases/latest",
            self.cfg.api_base.trim_end_matches('/'),
            self.cfg.repo
        );
        let body = self
            .get(&url, true, "application/vnd.github+json", MAX_SMALL)
            .await?;
        release::parse_release(&String::from_utf8_lossy(&body))
    }

    /// One asset: the token goes only to the API host that listed it; redirects never get it.
    async fn asset(&self, a: &release::Asset, max: u64) -> Result<Vec<u8>, UpdateError> {
        if a.size > max {
            return Err(UpdateError::TooLarge(max));
        }
        if self.use_hub() {
            let path = a.url.clone();
            return self
                .hub(move |c| c.get(&path, max))
                .await
                .and_then(|b| match a.size {
                    0 => Ok(b),
                    n if b.len() as u64 == n => Ok(b),
                    n => Err(UpdateError::Truncated {
                        got: b.len() as u64,
                        want: n,
                    }),
                });
        }
        let same = origin(&a.url) == origin(&self.cfg.api_base);
        let bytes = self.get(&a.url, same, OCTET, max).await?;
        if a.size != 0 && bytes.len() as u64 != a.size {
            return Err(UpdateError::Truncated {
                got: bytes.len() as u64,
                want: a.size,
            });
        }
        Ok(bytes)
    }

    async fn download_verify_install(&self, rel: &Release) -> Result<(), UpdateError> {
        let key = self.key.as_ref().ok_or(UpdateError::NoKey)?;
        let target = release::target(&self.info)?;
        let picked = release::select(rel, self.product, &target)?;
        self.set(UpdateState::Downloading);
        let tarball = self.asset(picked.tarball, MAX_DOWNLOAD).await?;
        let tarball_sig = self.asset(picked.tarball_sig, MAX_SMALL).await?;
        let sums = self.asset(picked.sums, MAX_SMALL).await?;
        let sums_sig = self.asset(picked.sums_sig, MAX_SMALL).await?;
        self.set(UpdateState::Verifying);
        verify::verify_release(
            key,
            &picked.tarball_name,
            &tarball,
            &tarball_sig,
            &sums,
            &sums_sig,
        )?;
        let bin = verify::extract_binary(&tarball, &release::binary_name(self.product, &target))?;
        self.set(UpdateState::Installing);
        let (exe, dir) = (self.exe.clone(), self.dir.clone());
        let (from, to) = (
            self.info.version().to_owned(),
            rel.tag.trim_start_matches('v').to_owned(),
        );
        tokio::task::spawn_blocking(move || install::install(&exe, &bin, &dir, &from, &to))
            .await
            .unwrap_or_else(|e| Err(task_failed("install", &e)))
    }

    async fn run_inner(&self, install: bool, honor_skip: bool) -> Result<Outcome, UpdateError> {
        self.set(UpdateState::Checking);
        let cur: SemVer = release::running(&self.info)?;
        let Some(rel) = self
            .latest()
            .await
            .map(|r| r.filter(|r| release::is_newer(cur, r)))?
        else {
            self.set(UpdateState::UpToDate {
                checked_ms: now_ms(),
            });
            return Ok(Outcome::UpToDate);
        };
        let version = rel.tag.trim_start_matches('v').to_owned();
        self.set(UpdateState::Available {
            version: version.clone(),
        });
        if !install
            || (honor_skip && install::skipped(&self.dir, &self.exe).as_deref() == Some(&version))
        {
            return Ok(Outcome::Available(version));
        }
        self.download_verify_install(&rel).await?;
        Ok(Outcome::Installed(version))
    }

    /// Check (and install when `install`), serialised: returns `None` if another run is active.
    /// Failures are reported as `UpdateState::Failed` and returned. A successful install is
    /// returned as `Installed`; the caller decides about the restart (`restart`).
    pub async fn run(
        &self,
        install: bool,
        honor_skip: bool,
    ) -> Option<Result<Outcome, UpdateError>> {
        let _g = self.busy.try_lock().ok()?;
        let r = self.run_inner(install, honor_skip).await;
        if let Err(e) = &r {
            self.set(UpdateState::failed(&e.to_string()));
        }
        Some(r)
    }

    /// Report `Restarting`, give the frame a moment to leave, then exit with `RESTART_EXIT`.
    pub async fn restart(&self) {
        self.set(UpdateState::Restarting);
        tokio::time::sleep(self.restart_delay).await;
        (self.exit)(RESTART_EXIT);
    }

    /// Hub `CheckUpdate`.
    pub async fn on_check(&self) {
        self.run(false, false).await;
    }

    /// Hub `UpdateNow`: install regardless of `auto_update` (the user asked).
    pub async fn on_update_now(&self) {
        if let Some(Ok(Outcome::Installed(_))) = self.run(true, false).await {
            self.restart().await;
        }
    }

    /// Periodic check; installs when `auto_update` is on. Runs forever.
    pub async fn auto_loop(self: Arc<Self>) {
        tokio::time::sleep(self.initial_delay).await;
        loop {
            if let Some(Ok(Outcome::Installed(_))) = self.run(self.auto, true).await {
                self.restart().await;
            }
            tokio::time::sleep(Duration::from_secs(self.cfg.interval_hours * 3600)).await;
        }
    }
}
