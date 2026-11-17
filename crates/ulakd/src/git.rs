//! Repo spawns: the in-memory token store, the `git clone` argv/env builders, the askpass
//! helper and the clone runner. No shell is involved anywhere and the token never reaches an
//! argv, a URL, a file other than the 0700 helper's environment lookup, or a log line.
use crate::errors::AgentError;
use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::process::Command;
use ulak_proto::{AgentId, BranchName, GitToken, RepoRef};

/// Env var that carries the token to the askpass helper (child environment only).
pub const TOKEN_ENV: &str = "ULAK_GIT_TOKEN";
const DEFAULT_HOST: &str = "https://github.com";
/// How long a `GitAccess` waits for its `Spawn`.
pub const TOKEN_TTL: Duration = Duration::from_secs(120);

/// Answers git's `Username for ...` with `x-access-token` and `Password for ...` with the token.
const ASKPASS: &str = "#!/bin/sh\ncase \"$1\" in\n  Username*) printf '%s\\n' x-access-token ;;\n  Password*) printf '%s\\n' \"$ULAK_GIT_TOKEN\" ;;\n  *) exit 1 ;;\nesac\n";

/// The agent's helper: same answers, but the token comes from the 0600 `token` file next to the
/// script, so it is never in an argv or an environment.
const ASKPASS_FILE: &str = "#!/bin/sh\ncase \"$1\" in\n  Username*) printf '%s\\n' x-access-token ;;\n  Password*) printf '%s\\n' \"$(cat \"${0%/*}/token\")\" ;;\n  *) exit 1 ;;\nesac\n";

/// Tokens delivered by `GitAccess`, in memory only. Single use, expire after `ttl`.
/// (Debug prints only the count.)
pub struct GitStore {
    ttl: Duration,
    m: Mutex<HashMap<AgentId, (GitToken, Instant)>>,
}

impl std::fmt::Debug for GitStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.m.lock().map_or(0, |m| m.len());
        write!(f, "GitStore({n} tokens)")
    }
}

impl Default for GitStore {
    fn default() -> Self {
        Self::new(TOKEN_TTL)
    }
}

impl GitStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            m: Mutex::default(),
        }
    }
    pub fn put(&self, agent: AgentId, token: GitToken) {
        let mut m = self.m.lock().expect("store lock");
        m.retain(|_, (_, at)| at.elapsed() < self.ttl); // expired tokens are dropped
        m.insert(agent, (token, Instant::now()));
    }
    /// Removes and returns the token if it has not expired.
    pub fn take(&self, agent: &AgentId) -> Option<GitToken> {
        let mut m = self.m.lock().expect("store lock");
        m.retain(|_, (_, at)| at.elapsed() < self.ttl);
        m.remove(agent).map(|(t, _)| t)
    }
}

#[derive(Debug, Clone)]
pub struct GitCfg {
    /// `<config>/work`: clones live in `<work_root>/<agent-id>/<repo-name>`.
    pub work_root: PathBuf,
    /// `<config>/run`: per-agent helpers live in `git-<agent-id>/`.
    pub run_dir: PathBuf,
    pub timeout: Duration,
    host: String,
    program: String,
}

impl GitCfg {
    pub fn new(work_root: PathBuf, run_dir: PathBuf) -> Self {
        Self {
            work_root,
            run_dir,
            timeout: Duration::from_secs(300),
            host: DEFAULT_HOST.into(),
            program: "git".into(),
        }
    }
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
    /// Tests only (never a runtime knob): another base URL, e.g. `file:///tmp/x`.
    #[cfg(feature = "test-key")]
    pub fn with_test_host(mut self, base: &str) -> Self {
        self.host = base.into();
        self
    }
    /// Tests only: a stub in place of `git`.
    #[cfg(feature = "test-key")]
    pub fn with_test_program(mut self, p: &str) -> Self {
        self.program = p.into();
        self
    }
}

fn bad(msg: impl Into<String>) -> AgentError {
    AgentError::Git(msg.into())
}

/// Argv after `git`. The repo and branch are re-validated here even though the proto did it.
pub fn clone_argv(
    host: &str,
    repo: &RepoRef,
    branch: Option<&BranchName>,
    dir: &Path,
) -> Result<Vec<String>, AgentError> {
    let repo = RepoRef::new(repo.as_str()).map_err(|_| bad("invalid repository name"))?;
    let mut a: Vec<String> = ["-c", "credential.helper=", "clone", "--depth", "1"]
        .map(String::from)
        .into();
    if let Some(b) = branch {
        let b = BranchName::new(b.as_str()).map_err(|_| bad("invalid branch name"))?;
        if b.as_str().starts_with('-') {
            return Err(bad("invalid branch name"));
        }
        a.extend(["--branch".into(), b.as_str().to_owned()]);
    }
    a.extend([
        "--".into(),
        format!("{host}/{}.git", repo.as_str()),
        dir.to_string_lossy().into_owned(),
    ]);
    Ok(a)
}

/// Env changes for the clone child: (set, removed).
pub type EnvChanges = (Vec<(String, String)>, Vec<&'static str>);

pub fn clone_env(host: &str, askpass: &Path, token: &GitToken) -> EnvChanges {
    let scheme = host.split(':').next().unwrap_or("https");
    (
        vec![
            ("GIT_ASKPASS".into(), askpass.to_string_lossy().into_owned()),
            (TOKEN_ENV.into(), token.expose().to_owned()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GCM_INTERACTIVE".into(), "never".into()),
            ("GIT_ALLOW_PROTOCOL".into(), scheme.into()),
        ],
        vec!["SSH_ASKPASS"],
    )
}

/// `create_dir_all` with mode 0700 on new directories (Unix only; elsewhere default mode).
fn private_dirs(p: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(p)
}

/// chmod 0700 (the DirBuilder mode is subject to the umask only downwards, this is explicit).
fn restrict(_p: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    std::fs::set_permissions(_p, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    Ok(())
}

/// A 0700 directory holding the askpass script (and, for agents, the 0600 token file); removed
/// on drop.
pub struct Helper {
    dir: PathBuf,
    pub script: PathBuf,
}

impl Helper {
    /// Clone helper: reads the token from `ULAK_GIT_TOKEN` of the clone child.
    pub fn create(dir: PathBuf) -> std::io::Result<Self> {
        Self::make(dir, ASKPASS, None)
    }

    /// Agent helper (`allow_push`): the token lives in `<dir>/token`, mode 0600.
    pub fn create_with_token(dir: PathBuf, token: &GitToken) -> std::io::Result<Self> {
        Self::make(dir, ASKPASS_FILE, Some(token))
    }

    fn make(dir: PathBuf, script_text: &str, token: Option<&GitToken>) -> std::io::Result<Self> {
        let _ = std::fs::remove_dir_all(&dir); // stale leftover of a crashed run
        if let Some(p) = dir.parent() {
            private_dirs(p)?;
        }
        std::fs::DirBuilder::new().create(&dir)?;
        restrict(&dir)?;
        let script = dir.join("askpass");
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o700);
        #[cfg(not(unix))]
        return Err(std::io::Error::other("git helpers need a Unix platform"));
        #[cfg_attr(not(unix), allow(unreachable_code))]
        let mut f = o.open(&script)?;
        f.write_all(script_text.as_bytes())?;
        f.sync_all()?;
        drop(f); // no long-lived fd to the script
        let this = Self { dir, script }; // dropping it on an error below removes the dir
        if let Some(t) = token {
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
            o.open(this.dir.join("token"))?
                .write_all(t.expose().as_bytes())?;
        }
        settle_exec(&this.script);
        Ok(this)
    }
}

/// Git execs the askpass program directly (no shell, verified with git 2.53), and the kernel
/// refuses to exec a file that any process holds open for writing (ETXTBSY). A concurrent fork in
/// another thread can briefly inherit our write fd even after we closed it. Probe-exec the script
/// (an unknown prompt just exits 1) until that stops; once no writer is left none can appear.
#[cfg(unix)]
pub(crate) fn settle_exec(script: &Path) {
    for _ in 0..200 {
        let r = std::process::Command::new(script)
            .arg("probe")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match r {
            Err(e) if e.raw_os_error() == Some(26) => std::thread::sleep(Duration::from_millis(5)),
            _ => return, // exec worked (or fails for another reason git will report itself)
        }
    }
}
#[cfg(not(unix))]
pub(crate) fn settle_exec(_: &Path) {}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn helper_dir(run_dir: &Path, id: &AgentId) -> PathBuf {
    run_dir.join(format!("git-{}", id.as_str()))
}

/// Deletes `git-*` helper dirs of agents not in `alive` (daemon start).
pub fn cleanup(run_dir: &Path, alive: &[AgentId]) {
    let Ok(rd) = std::fs::read_dir(run_dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(id) = name.strip_prefix("git-") {
            if !alive.iter().any(|a| a.as_str() == id) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

/// A readable reason from git's stderr. Never echoes the token or raw credentials.
pub fn map_failure(stderr: &str, token: &str) -> String {
    let l = stderr.to_lowercase();
    let has = |s: &str| l.contains(s);
    if has("remote branch") && has("not found") {
        "branch not found in the repository".into()
    } else if has("authentication failed")
        || has("could not read username")
        || has("could not read password")
        || has("terminal prompts disabled")
        || has("invalid username or password")
        || has("error: 403")
    {
        "authentication failed: the token was rejected or has no access to this repository".into()
    } else if has("repository not found")
        || has("not found") && has("error: 404")
        || has("does not appear to be a git repository")
    {
        "repository not found (or the token cannot see it)".into()
    } else if has("could not resolve host")
        || has("failed to connect")
        || has("connection refused")
        || has("connection timed out")
        || has("unable to access")
    {
        "network error: cannot reach the git host".into()
    } else {
        let last = stderr
            .lines()
            .find(|x| x.starts_with("fatal:"))
            .or_else(|| stderr.lines().rev().find(|x| !x.trim().is_empty()))
            .unwrap_or("no output");
        let mut t: String = last.chars().filter(|c| !c.is_control()).take(200).collect();
        if !token.is_empty() {
            t = t.replace(token, "***");
        }
        format!("git clone failed: {t}")
    }
}

async fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{pid}")])
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

async fn git_run(
    cfg: &GitCfg,
    args: &[String],
    env: &EnvChanges,
    timeout: Duration,
    token: &str,
) -> Result<(), AgentError> {
    let mut cmd = Command::new(&cfg.program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    for k in &env.1 {
        cmd.env_remove(k);
    }
    cmd.envs(env.0.iter().map(|(k, v)| (k, v)));
    let child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => bad("git is not installed on this node"),
        _ => bad(format!("cannot run git: {e}")),
    })?;
    let pid = child.id();
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_) => {
            kill_group(pid).await;
            Err(bad(format!("git timed out after {}s", timeout.as_secs())))
        }
        Ok(Err(e)) => Err(bad(format!("git failed: {e}"))),
        Ok(Ok(o)) if !o.status.success() => {
            Err(bad(map_failure(&String::from_utf8_lossy(&o.stderr), token)))
        }
        Ok(Ok(_)) => Ok(()),
    }
}

/// Clones `repo` into `<work_root>/<agent>/<repo-name>` and returns that path. On failure
/// nothing is left behind (the agent dir is removed).
pub async fn clone_repo(
    cfg: &GitCfg,
    agent: &AgentId,
    repo: &RepoRef,
    branch: Option<&BranchName>,
    token: &GitToken,
    allow_push: bool,
) -> Result<PathBuf, AgentError> {
    let agent_dir = cfg.work_root.join(agent.as_str());
    let dir = agent_dir.join(repo.name());
    let nonempty = std::fs::read_dir(&dir).is_ok_and(|mut r| r.next().is_some());
    if nonempty || dir.is_file() {
        return Err(bad(format!(
            "working directory {} already exists and is not empty",
            dir.display()
        )));
    }
    let argv = clone_argv(&cfg.host, repo, branch, &dir)?;
    private_dirs(&agent_dir)?;
    let r = async {
        let helper = Helper::create(cfg.work_root.join(format!(".askpass-{}", agent.as_str())))?;
        let env = clone_env(&cfg.host, &helper.script, token);
        git_run(cfg, &argv, &env, cfg.timeout, token.expose()).await?;
        if !allow_push {
            // guard rail, not a security boundary: the token's own scope is the boundary
            let a = ["-C", &dir.to_string_lossy()]
                .into_iter()
                .chain(["config", "--local", "remote.origin.pushurl", "no-push-here"])
                .map(String::from)
                .collect::<Vec<_>>();
            git_run(cfg, &a, &(vec![], vec![]), Duration::from_secs(30), "").await?;
        }
        Ok(())
    }
    .await;
    if r.is_err() {
        let _ = std::fs::remove_dir_all(&agent_dir);
    }
    r.map(|()| dir)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tok() -> GitToken {
        GitToken::new("ghs_SECRETSECRET1234").unwrap()
    }
    fn id(s: &str) -> AgentId {
        AgentId::new(s).unwrap()
    }

    #[test]
    fn argv_has_no_token_and_handles_branch() {
        let repo = RepoRef::new("octo/hello.rs").unwrap();
        let a = clone_argv(
            "https://github.com",
            &repo,
            None,
            Path::new("/w/a/hello.rs"),
        )
        .unwrap();
        assert_eq!(
            a,
            [
                "-c",
                "credential.helper=",
                "clone",
                "--depth",
                "1",
                "--",
                "https://github.com/octo/hello.rs.git",
                "/w/a/hello.rs"
            ]
        );
        let b = BranchName::new("feat/x").unwrap();
        let a = clone_argv("https://github.com", &repo, Some(&b), Path::new("/d")).unwrap();
        let i = a.iter().position(|x| x == "--branch").unwrap();
        assert_eq!(a[i + 1], "feat/x");
        assert!(i < a.iter().position(|x| x == "--").unwrap());
        assert!(!a.join(" ").contains("SECRET"));
    }

    #[test]
    fn option_injection_is_rejected() {
        // the proto refuses these; re-checking is defence in depth (serde skips validation? no,
        // but a future constructor might), so go through the public validators.
        assert!(BranchName::new("--upload-pack=x").is_err());
        assert!(BranchName::new("-x").is_err());
        assert!(RepoRef::new("--evil/name").is_err());
        assert!(RepoRef::new("a/b c").is_err());
        assert!(RepoRef::new("a/../b").is_err());
        assert!(RepoRef::new("a/b;rm").is_err());
    }

    #[test]
    fn env_carries_token_only_in_env() {
        let (set, rm) = clone_env("https://github.com", Path::new("/h/askpass"), &tok());
        let get = |k: &str| set.iter().find(|(n, _)| n == k).map(|x| x.1.clone());
        assert_eq!(get(TOKEN_ENV).unwrap(), "ghs_SECRETSECRET1234");
        assert_eq!(get("GIT_ASKPASS").unwrap(), "/h/askpass");
        assert_eq!(get("GIT_TERMINAL_PROMPT").unwrap(), "0");
        assert_eq!(get("GIT_CONFIG_NOSYSTEM").unwrap(), "1");
        assert_eq!(get("GCM_INTERACTIVE").unwrap(), "never");
        assert_eq!(get("GIT_ALLOW_PROTOCOL").unwrap(), "https");
        assert!(rm.contains(&"SSH_ASKPASS"));
    }

    /// N threads fork+exec continuously while one writes and execs a script 200 times.
    #[cfg(unix)]
    #[test]
    fn written_scripts_never_hit_text_file_busy() {
        use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let forkers: Vec<_> = (0..8)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(Relaxed) {
                        let _ = std::process::Command::new("true").status();
                    }
                })
            })
            .collect();
        let t = tempfile::tempdir().unwrap();
        let mut busy = 0;
        for i in 0..200 {
            let h = Helper::create(t.path().join(format!("h{i}"))).unwrap();
            let r = std::process::Command::new(&h.script)
                .arg("Username")
                .output();
            if matches!(&r, Err(e) if e.raw_os_error() == Some(26)) {
                busy += 1;
            }
        }
        stop.store(true, Relaxed);
        for f in forkers {
            f.join().unwrap();
        }
        assert_eq!(busy, 0, "ETXTBSY after Helper::create");
    }

    #[test]
    fn askpass_answers_both_prompts_and_cleans_up() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("run/h");
        let script;
        {
            let h = Helper::create(dir.clone()).unwrap();
            script = h.script.clone();
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&script), 0o700);
            let ask = |p: &str| {
                let o = std::process::Command::new(&script)
                    .arg(p)
                    .env(TOKEN_ENV, "tok123")
                    .output()
                    .unwrap();
                (o.status.success(), String::from_utf8(o.stdout).unwrap())
            };
            assert_eq!(
                ask("Username for 'https://github.com': "),
                (true, "x-access-token\n".into())
            );
            assert_eq!(
                ask("Password for 'https://x-access-token@github.com': "),
                (true, "tok123\n".into())
            );
            assert!(!ask("Something else").0);
        }
        assert!(!dir.exists(), "helper dir removed on drop");
    }

    #[test]
    fn agent_helper_reads_the_0600_token_file_and_cleans_up() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("run/git-a1");
        {
            let h = Helper::create_with_token(dir.clone(), &tok()).unwrap();
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&dir.join("token")), 0o600);
            let o = std::process::Command::new(&h.script)
                .arg("Password for 'https://github.com': ")
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .output()
                .unwrap();
            assert_eq!(
                String::from_utf8(o.stdout).unwrap(),
                "ghs_SECRETSECRET1234\n"
            );
        }
        assert!(!dir.exists());
    }

    #[test]
    fn failure_reasons_never_carry_a_token_or_control_chars() {
        let raw = "fatal: \u{1b}[31mbad ghs_SECRETSECRET1234 \u{7}here";
        let r = ulak_proto::sanitize_reason(&map_failure(raw, "ghs_SECRETSECRET1234"));
        assert!(
            !r.contains("SECRET") && !r.chars().any(char::is_control),
            "{r}"
        );
        assert!(ulak_proto::sanitize_reason(&"x".repeat(1000)).len() <= 300);
    }

    #[test]
    fn store_is_single_use_and_expires() {
        let s = GitStore::new(Duration::from_millis(80));
        s.put(id("a1"), tok());
        assert!(s.take(&id("zz")).is_none());
        assert!(s.take(&id("a1")).is_some());
        assert!(s.take(&id("a1")).is_none(), "single use");
        s.put(id("a2"), tok());
        std::thread::sleep(Duration::from_millis(120));
        assert!(s.take(&id("a2")).is_none(), "expired");
        s.put(id("a3"), tok());
        let dbg = format!("{s:?} {:?}", tok());
        assert!(!dbg.contains("SECRET"));
    }

    #[test]
    fn failures_are_readable_and_redacted() {
        let m = |s: &str| map_failure(s, "ghs_SECRETSECRET1234");
        assert!(m("remote: Invalid username or password.\nfatal: Authentication failed for 'https://github.com/o/r.git/'").contains("authentication failed"));
        assert!(m(
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled"
        )
        .contains("authentication"));
        assert!(
            m("remote: Repository not found.\nfatal: repository 'x' not found")
                .contains("repository not found")
        );
        assert!(m("warning: Could not find remote branch zz to clone.\nfatal: Remote branch zz not found in upstream origin").contains("branch not found"));
        assert!(m(
            "fatal: unable to access 'https://github.com/': Could not resolve host: github.com"
        )
        .contains("network"));
        assert!(m("fatal: '/x/y.git' does not appear to be a git repository\nfatal: Could not read from remote repository.\n\nPlease make sure you have the correct access rights\nand the repository exists.")
            .contains("repository not found"));
        assert!(m("fatal: odd failure\nhint: more").contains("fatal: odd failure"));
        let o = m("weird ghs_SECRETSECRET1234 thing");
        assert!(!o.contains("SECRET") && o.contains("***"));
    }
}
