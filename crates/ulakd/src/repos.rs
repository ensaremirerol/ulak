//! Repo index, clone reuse and cleanup. Everything here shells out to `git` with argv vectors
//! (no shell), with timeouts, and is blocking: callers run it in `spawn_blocking`.
//!
//! Safety rules for deletion live in [`judge`] and [`remove_clone`]: only clones directly under
//! the daemon's own work root (`<work>/<agent>/<repo>`), never anything else.
use crate::{
    errors::{AgentError, ConfigError},
    git::EnvChanges,
};
use serde::Deserialize;
use std::{
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use ulak_proto::{BranchName, CleanItem, DirListing, NodeSettings, RepoEntry, RepoRef};

/// At most this many entries are reported to the hub.
pub const INDEX_CAP: usize = 200;
const GIT_TIMEOUT: Duration = Duration::from_secs(20);
/// Candidate directories examined per scan (a runaway `repo_roots` must not stall the node).
const SCAN_CAP: usize = 600;
/// Marker (in `<work>/<agent>/`) holding the unix ms at which the clone was first seen unused.
const IDLE_MARKER: &str = ".ulak-idle";

fn d_interval() -> u64 {
    10
}
fn d_hours() -> u64 {
    24
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CleanCfg {
    /// Delete eligible clones automatically (default off).
    #[serde(default)]
    pub auto: bool,
    /// Only clones whose agent ended at least this long ago.
    #[serde(default = "d_hours")]
    pub after_exit_hours: u64,
}
impl Default for CleanCfg {
    fn default() -> Self {
        Self {
            auto: false,
            after_exit_hours: d_hours(),
        }
    }
}

/// `ulakd.json` in the config dir (absent = defaults).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Extra folders scanned for git repositories (one or two levels deep). Never cleaned.
    #[serde(default)]
    pub repo_roots: Vec<PathBuf>,
    /// Minutes between rescans (and automatic cleanups).
    #[serde(default = "d_interval")]
    pub index_interval_mins: u64,
    #[serde(default)]
    pub clean: CleanCfg,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            repo_roots: Vec::new(),
            index_interval_mins: d_interval(),
            clean: CleanCfg::default(),
        }
    }
}
impl Settings {
    pub fn load(dir: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(dir.join("ulakd.json")) {
            Ok(s) => {
                let mut me: Self = serde_json::from_str(&s)?;
                me.repo_roots = me.repo_roots.iter().map(|p| expand_tilde(p)).collect();
                me.index_interval_mins = me.index_interval_mins.max(1);
                Ok(me)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// These (local) settings with the hub's overrides; an out-of-range value is ignored
    /// (logged) and the local one kept.
    pub fn overlay(&self, hub: &NodeSettings) -> Self {
        let mut me = self.clone();
        let ignore = |what: &str| eprintln!("ulakd: ignoring hub setting {what}: out of range");
        match &hub.repo_roots {
            Some(r) if r.len() > 16 || r.iter().any(|p| p.is_empty()) => ignore("repo_roots"),
            Some(r) => me.repo_roots = r.iter().map(|p| expand_tilde(Path::new(p))).collect(),
            None => {}
        }
        match hub.index_interval_mins {
            Some(m) if (1..=1440).contains(&m) => me.index_interval_mins = m,
            Some(_) => ignore("index_interval_mins"),
            None => {}
        }
        if let Some(a) = hub.clean_auto {
            me.clean.auto = a;
        }
        match hub.clean_after_exit_hours {
            Some(h) if (1..=8760).contains(&h) => me.clean.after_exit_hours = h,
            Some(_) => ignore("clean_after_exit_hours"),
            None => {}
        }
        me
    }
}

/// `~` and `~/x` become `$HOME` paths (on the node); anything else is unchanged.
/// Sub-folders of `requested` (`~` expanded; empty = home) for the New agent folder browser.
/// Only folders under one of `allowed` (home, repo roots) can be listed; hidden ones are skipped.
/// Blocking: run it in `spawn_blocking`.
pub fn list_dirs(requested: &str, allowed: &[PathBuf]) -> DirListing {
    let fail = |e: String| DirListing {
        requested: requested.to_owned(),
        path: String::new(),
        dirs: Vec::new(),
        error: Some(e),
    };
    let asked = if requested.trim().is_empty() {
        "~"
    } else {
        requested.trim()
    };
    let Ok(abs) = std::fs::canonicalize(expand_tilde(Path::new(asked))) else {
        return fail(format!("{asked} does not exist"));
    };
    let inside = allowed
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .any(|r| abs.starts_with(&r));
    if !inside {
        return fail("only folders under your home or a repo root can be browsed".into());
    }
    let rd = match std::fs::read_dir(&abs) {
        Ok(rd) => rd,
        Err(e) => return fail(format!("cannot read {}: {e}", abs.display())),
    };
    let mut dirs: Vec<String> = rd
        .flatten()
        .take(20 * DirListing::MAX) // a huge folder must not stall the node
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            (!name.starts_with('.')
                && name.len() <= 255
                && std::fs::metadata(e.path()).ok()?.is_dir())
            .then_some(name)
        })
        .collect();
    dirs.sort();
    dirs.truncate(DirListing::MAX);
    DirListing {
        requested: requested.to_owned(),
        path: abs.display().to_string(),
        dirs,
        error: None,
    }
}

pub fn expand_tilde(p: &Path) -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    if p == Path::new("~") {
        return home().unwrap_or_else(|| p.to_owned());
    }
    match (p.strip_prefix("~"), home()) {
        (Ok(rest), Some(h)) => h.join(rest),
        _ => p.to_owned(),
    }
}

/// What scanning and cleaning need to know.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub program: String,
    /// Clone base URL (`https://github.com`; a `file://` base in tests).
    pub host: String,
    pub work_root: PathBuf,
    pub roots: Vec<PathBuf>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

// ---------------------------------------------------------------- git runner

#[derive(Debug)]
pub struct Out {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// `git -C <cwd> <args>` with a timeout (the whole process group is killed on expiry).
pub fn git(
    program: &str,
    cwd: &Path,
    args: &[&str],
    env: &EnvChanges,
    timeout: Duration,
) -> Result<Out, String> {
    let mut cmd = Command::new(program);
    cmd.arg("-C")
        .arg(cwd)
        // no hooks: a clone an agent controlled must not run code next to a token
        .args(["-c", "core.fsmonitor=false", "-c", "credential.helper="])
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for k in &env.1 {
        cmd.env_remove(k);
    }
    cmd.envs(env.0.iter().map(|(k, v)| (k, v)));
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => "git is not installed on this node".to_owned(),
        _ => format!("cannot run git: {e}"),
    })?;
    let reader = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = r.read_to_end(&mut b);
            String::from_utf8_lossy(&b).into_owned()
        })
    };
    let so = reader(Box::new(child.stdout.take().expect("piped stdout")));
    let se = reader(Box::new(child.stderr.take().expect("piped stderr")));
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                #[cfg(unix)]
                let _ = Command::new("kill")
                    .args(["-KILL", "--", &format!("-{}", child.id())])
                    .stderr(Stdio::null())
                    .status();
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("git timed out after {}s", timeout.as_secs()));
            }
            Err(e) => return Err(format!("git failed: {e}")),
        }
    };
    Ok(Out {
        ok: status.success(),
        stdout: so.join().unwrap_or_default(),
        stderr: se.join().unwrap_or_default(),
    })
}

fn no_env() -> EnvChanges {
    (Vec::new(), Vec::new())
}

fn git_ok(ctx: &Ctx, dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = git(&ctx.program, dir, args, &no_env(), GIT_TIMEOUT)?;
    if o.ok {
        Ok(o.stdout)
    } else {
        Err(format!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            o.stderr
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("no output")
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect::<String>()
        ))
    }
}

// ---------------------------------------------------------------- parsing

/// `owner/name` from a github.com origin URL (https, with optional userinfo; `git@` and
/// `ssh://git@` forms) or from the configured clone base. Anything else is `None`.
pub fn parse_origin(url: &str, host: &str) -> Option<RepoRef> {
    let u = url.trim();
    let rest = if let Some(r) = u.strip_prefix(&format!("{host}/")) {
        r
    } else if let Some(r) = u.strip_prefix("git@github.com:") {
        r
    } else if let Some(r) = u.strip_prefix("ssh://git@github.com/") {
        r
    } else if let Some(r) = u.strip_prefix("https://") {
        let (auth, path) = r.split_once('/')?;
        let h = auth.rsplit('@').next()?;
        if !h.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    } else {
        return None;
    };
    let rest = rest.trim_end_matches('/');
    RepoRef::new(rest.strip_suffix(".git").unwrap_or(rest)).ok()
}

/// (branch, ahead, behind, dirty) from `git status --porcelain=v1 -b` output.
pub fn parse_status(out: &str) -> (Option<String>, Option<u32>, Option<u32>, bool) {
    let mut lines = out.lines();
    let head = lines.next().unwrap_or("");
    let dirty = lines.any(|l| !l.trim().is_empty());
    let h = head.strip_prefix("## ").unwrap_or(head);
    let (names, counts) = match h.split_once(" [") {
        Some((n, c)) => (n, Some(c.trim_end_matches(']'))),
        None => (h, None),
    };
    let (local, upstream) = match names.split_once("...") {
        Some((l, u)) => (l, Some(u)),
        None => (names, None),
    };
    let branch = if local.starts_with("HEAD (no branch)") {
        None
    } else {
        let l = local.strip_prefix("No commits yet on ").unwrap_or(local);
        let l = l.strip_prefix("Initial commit on ").unwrap_or(l);
        Some(l.to_owned())
    };
    let (mut ahead, mut behind) = (None, None);
    if upstream.is_some() {
        let (mut a, mut b) = (0, 0);
        for part in counts.unwrap_or("").split(',') {
            let part = part.trim();
            if let Some(n) = part.strip_prefix("ahead ") {
                a = n.parse().unwrap_or(0);
            } else if let Some(n) = part.strip_prefix("behind ") {
                b = n.parse().unwrap_or(0);
            }
        }
        // "gone": the upstream no longer exists, so there is nothing to compare with
        if counts != Some("gone") {
            (ahead, behind) = (Some(a), Some(b));
        }
    }
    (branch, ahead, behind, dirty)
}

// ---------------------------------------------------------------- scan

#[derive(Debug, Clone)]
pub struct Found {
    pub entry: RepoEntry,
    /// Managed clones only: when the clone was first seen without a running agent.
    pub idle_since_ms: Option<u64>,
}

fn is_repo(p: &Path) -> bool {
    p.join(".git").exists()
}

fn subdirs(p: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir())) // symlinks are not followed
        .map(|e| e.path())
        .filter(|p| {
            !p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'))
        })
        .collect();
    v.sort();
    v
}

/// (path, managed) candidates: `<work>/<agent>/<repo>` plus the roots, two levels deep.
fn candidates(ctx: &Ctx, work: &Path) -> Vec<(PathBuf, bool)> {
    let mut out = Vec::new();
    for agent in subdirs(work) {
        for repo in subdirs(&agent) {
            if is_repo(&repo) {
                out.push((repo, true));
            }
        }
    }
    for root in &ctx.roots {
        let Ok(root) = root.canonicalize() else {
            continue;
        };
        if is_repo(&root) {
            out.push((root, false));
            continue;
        }
        for d1 in subdirs(&root) {
            if is_repo(&d1) {
                out.push((d1, false));
                continue;
            }
            out.extend(
                subdirs(&d1)
                    .into_iter()
                    .filter(|d| is_repo(d))
                    .map(|d| (d, false)),
            );
        }
    }
    out
}

fn mtime_ms(p: &Path) -> Option<u64> {
    let d = p
        .metadata()
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?;
    u64::try_from(d.as_millis()).ok()
}

fn last_fetch_ms(repo: &Path) -> Option<u64> {
    let g = repo.join(".git");
    // ponytail: a fresh clone has no FETCH_HEAD; its HEAD file's mtime stands in for "clone time"
    mtime_ms(&g.join("FETCH_HEAD")).or_else(|| mtime_ms(&g.join("HEAD")))
}

fn in_use(repo: &Path, busy: &HashSet<PathBuf>) -> bool {
    busy.iter().any(|b| b.starts_with(repo))
}

/// Indexes every repository found. No network. A repo whose state cannot be read, whose origin
/// is not github.com (or the configured base) or has no origin is left out. Also keeps the idle
/// markers of managed clones up to date (`now`).
pub fn scan(ctx: &Ctx, busy: &HashSet<PathBuf>, now: u64) -> Vec<Found> {
    let work = ctx
        .work_root
        .canonicalize()
        .unwrap_or_else(|_| ctx.work_root.clone());
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for (path, managed) in candidates(ctx, &work).into_iter().take(SCAN_CAP) {
        if !seen.insert(path.clone()) || (!managed && path.starts_with(&work)) {
            continue;
        }
        if let Some(f) = inspect(ctx, &path, managed, busy, now) {
            found.push(f);
        }
    }
    found.sort_by(|a, b| a.entry.path.cmp(&b.entry.path));
    found
}

fn inspect(
    ctx: &Ctx,
    path: &Path,
    managed: bool,
    busy: &HashSet<PathBuf>,
    now: u64,
) -> Option<Found> {
    let url = git_ok(ctx, path, &["config", "--get", "remote.origin.url"]).ok()?;
    let full_name = parse_origin(&url, &ctx.host)?;
    let st = git_ok(ctx, path, &["status", "--porcelain=v1", "-b"]).ok()?;
    let (branch, ahead, behind, dirty) = parse_status(&st);
    let used = in_use(path, busy);
    let idle_since_ms = managed.then(|| idle_marker(path, used, now)).flatten();
    Some(Found {
        entry: RepoEntry {
            full_name,
            path: path.to_string_lossy().into_owned(),
            branch,
            clean: !dirty,
            ahead,
            behind,
            last_fetch_ms: last_fetch_ms(path),
            in_use: used,
            managed,
        },
        idle_since_ms,
    })
}

/// Reads, creates (first idle sighting) or removes (in use again) the marker.
fn idle_marker(repo: &Path, used: bool, now: u64) -> Option<u64> {
    let m = repo.parent()?.join(IDLE_MARKER);
    if used {
        let _ = std::fs::remove_file(&m);
        return None;
    }
    match std::fs::read_to_string(&m)
        .ok()
        .and_then(|s| s.trim().parse().ok())
    {
        Some(t) => Some(t),
        None => {
            let _ = std::fs::write(&m, now.to_string());
            Some(now)
        }
    }
}

/// The wire form: sorted by path, at most [`INDEX_CAP`].
pub fn entries(found: &[Found]) -> Vec<RepoEntry> {
    found
        .iter()
        .take(INDEX_CAP)
        .map(|f| f.entry.clone())
        .collect()
}

// ---------------------------------------------------------------- cleanup

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Remove(String),
    Keep(String),
}

/// `path` must be exactly `<work>/<agent>/<repo>`, spelled canonically (so no symlink anywhere
/// on the way, and none as the last component). Returns the canonical path.
pub fn inside_work_root(work_root: &Path, path: &Path) -> Result<PathBuf, String> {
    let work = work_root
        .canonicalize()
        .map_err(|e| format!("cannot resolve the work root: {e}"))?;
    let canon = path
        .canonicalize()
        .map_err(|e| format!("cannot resolve the path: {e}"))?;
    if canon != path {
        return Err("path is not canonical (symlink or relative component)".into());
    }
    let rel = canon
        .strip_prefix(&work)
        .map_err(|_| "not under the work root (never deleted)".to_owned())?;
    if rel.components().count() != 2 {
        return Err("not a work-root clone (<work>/<agent>/<repo>)".into());
    }
    Ok(canon)
}

fn ago(ms: Option<u64>, now: u64) -> String {
    match ms {
        Some(t) => format!("{}h ago", now.saturating_sub(t) / 3_600_000),
        None => "unknown time".into(),
    }
}

/// Decides whether the clone at `path` may be deleted, and says why. Never deletes. Any
/// failure to determine the state is a `Keep`. `token_fetch`: `Some(Ok)` a fresh fetch just
/// succeeded, `Some(Err(reason))` it failed, `None` no token (the last fetch is used).
pub fn judge(
    ctx: &Ctx,
    path: &Path,
    busy: &HashSet<PathBuf>,
    token_fetch: Option<Result<(), String>>,
    now: u64,
) -> Verdict {
    use Verdict::{Keep, Remove};
    let path = match inside_work_root(&ctx.work_root, path) {
        Ok(p) => p,
        Err(e) => return Keep(e),
    };
    if in_use(&path, busy) {
        return Keep("an agent is using it".into());
    }
    let basis = match token_fetch {
        Some(Ok(())) => "after a fresh fetch".to_owned(),
        Some(Err(e)) => {
            return Keep(format!(
                "cannot verify against the remote: fetch failed: {e}"
            ))
        }
        None => format!("from the last fetch ({})", ago(last_fetch_ms(&path), now)),
    };
    let run = |args: &[&str]| git_ok(ctx, &path, args);
    let st = match run(&["status", "--porcelain=v1", "-b", "--untracked-files=all"]) {
        Ok(s) => s,
        Err(e) => return Keep(format!("cannot determine state: {e}")),
    };
    if parse_status(&st).3 {
        return Keep("uncommitted or untracked files".into());
    }
    match run(&["stash", "list"]) {
        Ok(s) if s.trim().is_empty() => {}
        Ok(_) => return Keep("has a stash".into()),
        Err(e) => return Keep(format!("cannot determine state: {e}")),
    }
    match run(&["remote"]) {
        Ok(s) if !s.trim().is_empty() => {}
        Ok(_) => return Keep("no remote configured".into()),
        Err(e) => return Keep(format!("cannot determine state: {e}")),
    }
    // every local branch commit must be reachable from some remote branch
    match run(&["rev-list", "-n", "1", "--branches", "--not", "--remotes"]) {
        Ok(s) if s.trim().is_empty() => {}
        Ok(_) => return Keep("has local commits that are not on the remote".into()),
        Err(e) => return Keep(format!("cannot determine state: {e}")),
    }
    // a detached HEAD is not a branch: check it too
    if git(
        &ctx.program,
        &path,
        &["symbolic-ref", "-q", "HEAD"],
        &no_env(),
        GIT_TIMEOUT,
    )
    .is_ok_and(|o| !o.ok && o.stdout.trim().is_empty())
    {
        match run(&["rev-list", "-n", "1", "HEAD", "--not", "--remotes"]) {
            Ok(s) if s.trim().is_empty() => {}
            Ok(_) => return Keep("detached HEAD has commits that are not on the remote".into()),
            Err(e) => return Keep(format!("cannot determine state: {e}")),
        }
    }
    Remove(format!("up to date with the remote, {basis}"))
}

/// Deletes the clone directory, then its `<work>/<agent>` parent when that is left empty.
/// Re-checks containment right before deleting.
pub fn remove_clone(work_root: &Path, path: &Path) -> Result<(), String> {
    let canon = inside_work_root(work_root, path)?;
    std::fs::remove_dir_all(&canon).map_err(|e| format!("delete failed: {e}"))?;
    if let Some(parent) = canon.parent() {
        let _ = std::fs::remove_file(parent.join(IDLE_MARKER));
        let _ = std::fs::remove_dir(parent); // only succeeds when empty
    }
    Ok(())
}

/// Result of one clean pass.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cleaned {
    pub removed: Vec<CleanItem>,
    pub kept: Vec<CleanItem>,
}

fn item(f: &Found, reason: String) -> CleanItem {
    CleanItem {
        path: f.entry.path.clone(),
        full_name: Some(f.entry.full_name.clone()),
        reason,
    }
}

/// Judges (and, unless `dry_run`, deletes) the given indexed clones. `fetch` runs before the
/// judgement and says whether the remote data is fresh (see [`judge`]).
pub fn clean(
    ctx: &Ctx,
    targets: &[Found],
    busy: &HashSet<PathBuf>,
    dry_run: bool,
    fetch: &dyn Fn(&Path) -> Option<Result<(), String>>,
    now: u64,
) -> Cleaned {
    let mut c = Cleaned::default();
    for f in targets {
        let path = Path::new(&f.entry.path);
        if !f.entry.managed {
            c.kept
                .push(item(f, "not under the work root (never deleted)".into()));
            continue;
        }
        match judge(ctx, path, busy, fetch(path), now) {
            Verdict::Keep(r) => c.kept.push(item(f, r)),
            Verdict::Remove(r) if dry_run => c.removed.push(item(f, r)),
            Verdict::Remove(r) => match remove_clone(&ctx.work_root, path) {
                Ok(()) => c.removed.push(item(f, r)),
                Err(e) => c.kept.push(item(f, e)),
            },
        }
    }
    c
}

/// Clones that the automatic policy may delete: managed, unused, idle for `hours`.
pub fn auto_due(found: &[Found], now: u64, hours: u64) -> Vec<Found> {
    let limit = hours.saturating_mul(3_600_000);
    found
        .iter()
        .filter(|f| {
            f.entry.managed
                && !f.entry.in_use
                && f.idle_since_ms
                    .is_some_and(|t| now.saturating_sub(t) >= limit)
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------- reuse

fn fail(msg: impl Into<String>) -> AgentError {
    AgentError::Git(msg.into())
}

/// Fetches `branch` (or the default branch) from the clone base with the same askpass mechanism
/// as a clone, checks it out (creating a tracking branch when it only exists on the remote) and
/// fast-forwards when behind. Never discards local work. `env` carries the askpass + token.
pub fn reuse(
    ctx: &Ctx,
    repo: &RepoRef,
    branch: Option<&BranchName>,
    path: &Path,
    env: &EnvChanges,
    timeout: Duration,
) -> Result<(), AgentError> {
    // the token env (askpass) goes only to the commands that talk to the remote
    let net = |args: &[&str]| -> Result<Out, AgentError> {
        git(&ctx.program, path, args, env, timeout).map_err(fail)
    };
    let run = |args: &[&str]| -> Result<Out, AgentError> {
        git(&ctx.program, path, args, &no_env(), timeout).map_err(fail)
    };
    let token = env
        .0
        .iter()
        .find(|(k, _)| k == crate::git::TOKEN_ENV)
        .map_or("", |(_, v)| v.as_str());
    let check = |o: Out| -> Result<String, AgentError> {
        if o.ok {
            Ok(o.stdout)
        } else {
            Err(fail(
                crate::git::map_failure(&o.stderr, token).replace("git clone failed", "git failed"),
            ))
        }
    };
    let must = |args: &[&str]| check(run(args)?);
    let url = format!("{}/{}.git", ctx.host, repo.as_str());
    let st = must(&["status", "--porcelain=v1", "-b", "--untracked-files=normal"])?;
    if parse_status(&st).3 {
        return Err(fail(format!(
            "the existing clone {} has uncommitted changes; refusing to switch it (commit or stash them, or spawn with fresh=true)",
            path.display()
        )));
    }
    let b = match branch {
        Some(b) => b.as_str().to_owned(),
        None => {
            // origin/HEAD from the clone (absent in a single-branch clone: ask the remote);
            // else whatever is checked out
            let h = run(&["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"])?;
            let local =
                h.ok.then(|| h.stdout.trim().strip_prefix("origin/").map(str::to_owned))
                    .flatten();
            let remote = || {
                let o = check(net(&["ls-remote", "--symref", "--", &url, "HEAD"])?)?;
                Ok::<_, AgentError>(o.lines().find_map(|l| {
                    let r = l.strip_prefix("ref: refs/heads/")?;
                    Some(r.split('\t').next()?.to_owned())
                }))
            };
            match local.map_or_else(remote, |b| Ok(Some(b)))? {
                Some(b) => b,
                None => parse_status(&st).0.ok_or_else(|| {
                    fail("the existing clone has a detached HEAD and no branch was requested")
                })?,
            }
        }
    };
    let b = BranchName::new(&b).map_err(|_| fail("invalid branch name"))?;
    let b = b.as_str();
    if b.starts_with('-') {
        return Err(fail("invalid branch name"));
    }
    let remote_ref = format!("refs/remotes/origin/{b}");
    check(net(&[
        "fetch",
        "--no-tags",
        "--",
        &url,
        &format!("+refs/heads/{b}:{remote_ref}"),
    ])?)?;
    let local = format!("refs/heads/{b}");
    let has_local = run(&["rev-parse", "--verify", "-q", &local])?.ok;
    if has_local {
        must(&["checkout", "-q", b, "--"])?;
    } else {
        must(&["checkout", "-q", "-b", b, &remote_ref])?;
        must(&["config", &format!("branch.{b}.remote"), "origin"])?;
        must(&["config", &format!("branch.{b}.merge"), &local])?;
    }
    // fast-forward only; ahead or diverged work stays as it is
    let counts = must(&[
        "rev-list",
        "--left-right",
        "--count",
        &format!("HEAD...{remote_ref}"),
    ])?;
    let mut it = counts
        .split_whitespace()
        .map(|n| n.parse::<u32>().unwrap_or(0));
    let (ahead, behind) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
    if behind > 0 && ahead == 0 {
        must(&["merge", "--ff-only", "-q", &remote_ref])?;
    } else if behind > 0 {
        return Err(fail(format!(
            "branch {b} has local commits and the remote moved: not fast-forwardable, left untouched"
        )));
    }
    Ok(())
}

/// Picks the clone to reuse: a clean unused one, else (all dirty) the first unused one so the
/// caller can refuse with the reason. `None`: nothing to reuse.
pub fn pick(found: &[Found], repo: &RepoRef, busy: &HashSet<PathBuf>) -> Option<Found> {
    let mine: Vec<&Found> = found
        .iter()
        .filter(|f| {
            &f.entry.full_name == repo && !f.entry.in_use && !in_use(Path::new(&f.entry.path), busy)
        })
        .collect();
    mine.iter()
        .find(|f| f.entry.clean)
        .or_else(|| mine.first())
        .map(|f| (*f).clone())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Command;

    fn g(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).into_owned()
    }

    fn commit(dir: &Path, file: &str) {
        std::fs::write(dir.join(file), file).unwrap();
        g(dir, &["add", "."]);
        g(dir, &["commit", "-q", "-m", file]);
    }

    /// `<t>/srv/octo/a.git` with one commit on main; `ctx` with work `<t>/work`, roots `<t>/roots`.
    fn setup() -> (tempfile::TempDir, Ctx) {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        g(&src, &["init", "-q", "-b", "main"]);
        commit(&src, "README");
        let bare = root.join("srv/octo/a.git");
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        g(
            &root,
            &[
                "clone",
                "-q",
                "--bare",
                src.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
        );
        std::fs::create_dir_all(root.join("work")).unwrap();
        std::fs::create_dir_all(root.join("roots")).unwrap();
        let ctx = Ctx {
            program: "git".into(),
            host: format!("file://{}", root.join("srv").display()),
            work_root: root.join("work"),
            roots: vec![root.join("roots")],
        };
        (t, ctx)
    }

    /// Clones `octo/a` to `<dir>` (relative to the temp root) and returns its path.
    fn clone(ctx: &Ctx, rel: &str) -> PathBuf {
        let dst = ctx.work_root.parent().unwrap().join(rel);
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        let url = format!("{}/octo/a.git", ctx.host);
        g(
            dst.parent().unwrap(),
            &["clone", "-q", &url, dst.to_str().unwrap()],
        );
        dst
    }

    fn push_upstream(ctx: &Ctx, file: &str) {
        let up = clone(ctx, &format!("up-{file}/a"));
        commit(&up, file);
        g(&up, &["push", "-q", "origin", "main"]);
    }

    #[test]
    fn origin_and_status_parsing() {
        let h = "https://github.com";
        let r = |s: &str| parse_origin(s, h).map(|r| r.as_str().to_owned());
        assert_eq!(r("https://github.com/o/n.git").as_deref(), Some("o/n"));
        assert_eq!(
            r("https://x-access-token@github.com/o/n").as_deref(),
            Some("o/n")
        );
        assert_eq!(r("git@github.com:o/n.git").as_deref(), Some("o/n"));
        assert_eq!(r("ssh://git@github.com/o/n.git\n").as_deref(), Some("o/n"));
        assert_eq!(r("https://gitlab.com/o/n.git"), None);
        assert_eq!(r("/local/path"), None);
        assert_eq!(
            parse_status("## main...origin/main [ahead 2, behind 1]\n?? x\n"),
            (Some("main".into()), Some(2), Some(1), true)
        );
        assert_eq!(
            parse_status("## main...origin/main\n"),
            (Some("main".into()), Some(0), Some(0), false)
        );
        assert_eq!(
            parse_status("## main\n"),
            (Some("main".into()), None, None, false)
        );
        assert_eq!(
            parse_status("## HEAD (no branch)\n"),
            (None, None, None, false)
        );
        assert_eq!(parse_status("## dev...origin/dev [gone]\n").1, None);
        assert_eq!(
            parse_status("## No commits yet on main\n").0.as_deref(),
            Some("main")
        );
    }

    #[test]
    fn list_dirs_stays_inside_the_allowed_roots() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("home");
        for d in ["b", "a", ".hidden", "a/inner"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("file.txt"), "x").unwrap();
        std::os::unix::fs::symlink(t.path(), root.join("escape")).unwrap();
        let allowed = [root.clone()];
        let l = list_dirs(root.to_str().unwrap(), &allowed);
        assert_eq!(l.error, None);
        assert_eq!(l.dirs, ["a", "b", "escape"]); // sorted, no hidden, no files
        assert_eq!(
            l.path,
            std::fs::canonicalize(&root).unwrap().display().to_string()
        );
        assert_eq!(
            list_dirs(root.join("a").to_str().unwrap(), &allowed).dirs,
            ["inner"]
        );
        // A symlink out of the root, `..` and absolute paths elsewhere are refused.
        for bad in [
            root.join("escape"),
            root.join("a/../.."),
            PathBuf::from("/etc"),
        ] {
            let l = list_dirs(bad.to_str().unwrap(), &allowed);
            assert!(l.error.unwrap().contains("only folders"), "{bad:?}");
            assert!(l.dirs.is_empty() && l.path.is_empty());
        }
        assert!(list_dirs("/no/such/dir", &allowed)
            .error
            .unwrap()
            .contains("does not exist"));
    }

    #[test]
    fn settings_load_defaults_and_tilde() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(t.path()).unwrap(), Settings::default());
        std::fs::write(
            t.path().join("ulakd.json"),
            r#"{"repo_roots":["~/code"],"index_interval_mins":0,"clean":{"auto":true}}"#,
        )
        .unwrap();
        let s = Settings::load(t.path()).unwrap();
        assert!(s.clean.auto && s.clean.after_exit_hours == 24 && s.index_interval_mins == 1);
        assert!(!s.repo_roots[0].starts_with("~") && s.repo_roots[0].ends_with("code"));
        std::fs::write(t.path().join("ulakd.json"), r#"{"bogus":1}"#).unwrap();
        assert!(Settings::load(t.path()).is_err());
    }

    #[test]
    fn hub_settings_overlay_validate_and_restore() {
        let local = Settings::default();
        let hub = NodeSettings {
            repo_roots: Some(vec!["~/src".into()]),
            index_interval_mins: Some(30),
            clean_auto: Some(true),
            clean_after_exit_hours: Some(2),
            ..NodeSettings::default()
        };
        let s = local.overlay(&hub);
        assert!(s.repo_roots.len() == 1 && !s.repo_roots[0].starts_with("~"));
        assert!(s.repo_roots[0].ends_with("src"));
        assert_eq!((s.index_interval_mins, s.clean.auto), (30, true));
        assert_eq!(s.clean.after_exit_hours, 2);
        // out of range: the local value stays
        let bad = NodeSettings {
            repo_roots: Some(vec!["/x".into(); 17]),
            index_interval_mins: Some(0),
            clean_after_exit_hours: Some(8761),
            ..NodeSettings::default()
        };
        assert_eq!(local.overlay(&bad), local);
        // all None: the local settings
        assert_eq!(local.overlay(&NodeSettings::default()), local);
        // an empty list is a valid override (no extra roots)
        let mine = Settings {
            repo_roots: vec!["/mine".into()],
            ..Settings::default()
        };
        let none = NodeSettings {
            repo_roots: Some(vec![]),
            ..NodeSettings::default()
        };
        assert!(mine.overlay(&none).repo_roots.is_empty());
    }

    #[test]
    fn scan_reports_state_and_ignores_foreign_repos() {
        let (_t, ctx) = setup();
        let clean = clone(&ctx, "work/a1/a");
        let dirty = clone(&ctx, "work/a2/a");
        std::fs::write(dirty.join("new"), "x").unwrap();
        let ahead = clone(&ctx, "work/a3/a");
        commit(&ahead, "mine");
        let behind = clone(&ctx, "work/a4/a");
        push_upstream(&ctx, "up1");
        g(&behind, &["fetch", "-q"]);
        let none = ctx.work_root.join("a5/x");
        std::fs::create_dir_all(&none).unwrap();
        g(&none, &["init", "-q"]);
        let gitlab = clone(&ctx, "work/a6/a");
        g(
            &gitlab,
            &["remote", "set-url", "origin", "https://gitlab.com/o/a.git"],
        );
        let used = clone(&ctx, "work/a7/a");
        let mine = clone(&ctx, "roots/proj");
        let busy: HashSet<PathBuf> = [used.join("sub")].into();
        let found = scan(&ctx, &busy, 1000);
        let get = |p: &Path| {
            found
                .iter()
                .find(|f| f.entry.path == p.to_str().unwrap())
                .map(|f| &f.entry)
        };
        let e = get(&clean).unwrap();
        assert!(e.clean && e.managed && !e.in_use && e.full_name.as_str() == "octo/a");
        assert_eq!(
            (e.branch.as_deref(), e.ahead, e.behind),
            (Some("main"), Some(0), Some(0))
        );
        assert!(e.last_fetch_ms.is_some());
        assert!(!get(&dirty).unwrap().clean);
        assert_eq!(get(&ahead).unwrap().ahead, Some(1));
        assert_eq!(get(&behind).unwrap().behind, Some(1));
        assert!(get(&none).is_none() && get(&gitlab).is_none());
        assert!(get(&used).unwrap().in_use);
        let m = get(&mine).unwrap();
        assert!(!m.managed && m.clean);
        // the up-* helper clones live outside work and roots
        assert_eq!(found.len(), 6, "{found:?}");
        // idle markers: unused managed clones only
        assert_eq!(
            found
                .iter()
                .find(|f| f.entry.path == clean.to_str().unwrap())
                .unwrap()
                .idle_since_ms,
            Some(1000)
        );
        assert!(found
            .iter()
            .find(|f| f.entry.path == used.to_str().unwrap())
            .unwrap()
            .idle_since_ms
            .is_none());
        assert!(found
            .iter()
            .find(|f| f.entry.path == mine.to_str().unwrap())
            .unwrap()
            .idle_since_ms
            .is_none());
        assert_eq!(entries(&found).len(), 6);
    }

    #[test]
    fn cleanup_matrix() {
        let (t, ctx) = setup();
        let none = HashSet::new();
        let j = |p: &Path, busy: &HashSet<PathBuf>| judge(&ctx, p, busy, None, now_ms());
        let keep = |p: &Path, why: &str| match j(p, &none) {
            Verdict::Keep(r) => assert!(r.contains(why), "{p:?}: {r}"),
            v => panic!("{p:?}: {v:?}"),
        };
        let dirty = clone(&ctx, "work/d/a");
        std::fs::write(dirty.join("README"), "changed").unwrap();
        keep(&dirty, "uncommitted");
        let untracked = clone(&ctx, "work/u/a");
        std::fs::create_dir_all(untracked.join("deep/er")).unwrap();
        std::fs::write(untracked.join("deep/er/x"), "x").unwrap();
        keep(&untracked, "untracked");
        let ahead = clone(&ctx, "work/h/a");
        commit(&ahead, "mine");
        keep(&ahead, "not on the remote");
        let side = clone(&ctx, "work/b/a");
        g(&side, &["checkout", "-q", "-b", "side"]);
        commit(&side, "side");
        g(&side, &["checkout", "-q", "main"]);
        keep(&side, "not on the remote");
        let stash = clone(&ctx, "work/s/a");
        std::fs::write(stash.join("README"), "wip").unwrap();
        g(&stash, &["stash", "-q"]);
        keep(&stash, "stash");
        let used = clone(&ctx, "work/x/a");
        assert!(
            matches!(j(&used, &[used.clone()].into()), Verdict::Keep(r) if r.contains("using"))
        );
        let mine = clone(&ctx, "roots/proj");
        keep(&mine, "not under the work root");
        // a symlinked agent dir pointing outside the work root
        let outside = clone(&ctx, "elsewhere/a");
        std::os::unix::fs::symlink(outside.parent().unwrap(), ctx.work_root.join("l")).unwrap();
        keep(&ctx.work_root.join("l/a"), "not canonical");
        keep(&ctx.work_root.join("d"), "not a work-root clone");
        let det = clone(&ctx, "work/t/a");
        g(&det, &["checkout", "-q", "--detach"]);
        commit(&det, "loose");
        keep(&det, "detached HEAD");
        let broken = ctx.work_root.join("k/a");
        std::fs::create_dir_all(broken.join(".git")).unwrap();
        keep(&broken, "cannot determine state");
        assert!(matches!(
            judge(&ctx, &dirty, &none, Some(Err("offline".into())), 0),
            Verdict::Keep(r) if r.contains("fetch failed")
        ));
        let ok = clone(&ctx, "work/ok/a");
        assert!(matches!(j(&ok, &none), Verdict::Remove(r) if r.contains("last fetch")));
        // dry run judges everything and removes nothing; a real run removes only `ok`
        let found = scan(&ctx, &none, 0);
        let managed: Vec<Found> = found.iter().filter(|f| f.entry.managed).cloned().collect();
        let busy: HashSet<PathBuf> = [used.clone()].into();
        let dry = clean(&ctx, &found, &busy, true, &|_| None, 0);
        assert_eq!(dry.removed.len(), 1, "{dry:?}");
        assert!(ok.exists());
        assert!(dry
            .kept
            .iter()
            .any(|k| k.path == mine.to_str().unwrap() && k.reason.contains("never deleted")));
        let real = clean(&ctx, &managed, &busy, false, &|_| None, 0);
        assert_eq!(
            real.removed
                .iter()
                .map(|i| i.path.clone())
                .collect::<Vec<_>>(),
            [ok.to_string_lossy()]
        );
        assert!(
            !ok.exists() && !ctx.work_root.join("ok").exists(),
            "agent dir removed too"
        );
        for p in [
            &dirty, &untracked, &ahead, &side, &stash, &used, &mine, &outside, &det,
        ] {
            assert!(p.exists(), "{p:?} kept");
        }
        assert!(remove_clone(&ctx.work_root, &mine).is_err());
        assert!(remove_clone(&ctx.work_root, &ctx.work_root.join("l/a")).is_err());
        drop(t);
    }

    #[test]
    fn auto_policy_waits_for_idle_hours() {
        let (_t, ctx) = setup();
        let a = clone(&ctx, "work/a/a");
        let b = clone(&ctx, "work/b/a");
        let h = 3_600_000;
        let busy: HashSet<PathBuf> = [b.clone()].into();
        // first sighting at t0; still idle later keeps the first time
        let f = scan(&ctx, &busy, 10 * h);
        assert!(auto_due(&f, 33 * h, 24).is_empty());
        let f = scan(&ctx, &busy, 20 * h);
        let due = auto_due(&f, 34 * h, 24);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].entry.path, a.to_str().unwrap());
        // in use resets the clock
        let f = scan(&ctx, &[a.clone()].into(), 35 * h);
        assert!(auto_due(&f, 35 * h, 24)
            .iter()
            .all(|d| d.entry.path != a.to_str().unwrap()));
        let f = scan(&ctx, &HashSet::new(), 36 * h);
        let due = auto_due(&f, 59 * h, 24); // b idle since 35h, a again since 36h
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].entry.path, b.to_str().unwrap());
        assert_eq!(auto_due(&f, 60 * h, 24).len(), 2);
    }

    #[test]
    fn pick_prefers_clean_unused_clones() {
        let (_t, ctx) = setup();
        let a = clone(&ctx, "work/a/a");
        let b = clone(&ctx, "work/b/a");
        std::fs::write(a.join("x"), "x").unwrap();
        let none = HashSet::new();
        let f = scan(&ctx, &none, 0);
        let r = RepoRef::new("octo/a").unwrap();
        assert_eq!(pick(&f, &r, &none).unwrap().entry.path, b.to_str().unwrap());
        let busy: HashSet<PathBuf> = [b.clone()].into();
        assert_eq!(
            pick(&f, &r, &busy).unwrap().entry.path,
            a.to_str().unwrap(),
            "dirty, for the refusal"
        );
        assert!(pick(&f, &r, &[a, b].into()).is_none());
        assert!(pick(&f, &RepoRef::new("octo/other").unwrap(), &none).is_none());
    }
}
