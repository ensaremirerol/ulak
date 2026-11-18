//! tmux agent manager. Every command is an argv vector handed to `tmux`; no shell is involved
//! (the agent binary is started through `env`, not `sh -c`).
use crate::{
    agents::{binary_name, is_installed, match_login, Agents, Exec, Installer, Recipe},
    errors::AgentError,
    git::{self, GitCfg, GitStore},
    hub::{AgentHost, CleanReq, HostEvent},
    mcp::{self, Prepared},
    repos::{self, Cleaned, Settings},
};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command, sync::mpsc};
use ulak_proto::{
    AgentId, AgentStatus, BlockerKind, BranchName, CleanItem, GitToken, HubProfile, NodeSettings,
    ProfileInfo, ProfileKey, RepoEntry, RepoRef, Source, SpawnSpec,
};

/// Session option that remembers the profile key, so a restarted daemon can re-adopt the agent.
const PROFILE_OPT: &str = "@ulak_profile";

/// The local config (`agents.json`, `ulakd.json`) and what is in effect: local overlaid with the
/// hub's last `Config` (not persisted; a restart is local until the next one arrives).
#[derive(Debug)]
struct Live {
    local_agents: Agents,
    local_settings: Settings,
    agents: Arc<Agents>,
    /// Profile names that came from (or were overridden by) the hub.
    hub_names: Arc<BTreeSet<String>>,
    settings: Arc<Settings>,
}

/// Per profile: (blocked, generation bumped on every resolve).
type Blocks = Arc<Mutex<HashMap<ProfileKey, (bool, u64)>>>;
/// Clone paths an agent is starting in (canonical), before its tmux session exists.
type Claims = Arc<Mutex<HashMap<PathBuf, AgentId>>>;

/// A claimed clone path; released on drop (also when the spawn future is dropped).
struct Claim {
    claims: Claims,
    path: PathBuf,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.claims.lock().expect("claims lock").remove(&self.path);
    }
}

/// The tmux server is not running (or exited while we asked, which it does after its last
/// session): no sessions.
fn no_server(e: &AgentError) -> bool {
    matches!(e, AgentError::Tmux { stderr, .. }
        if stderr.contains("no server running")
            || stderr.contains("No such file")
            || stderr.contains("server exited unexpectedly"))
}

fn s(x: &str) -> String {
    x.to_owned()
}

fn valid_env_name(n: &str) -> bool {
    let mut c = n.chars();
    c.next()
        .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
        && c.all(|f| f.is_ascii_alphanumeric() || f == '_')
}

/// Argv (after `tmux [-L sock]`) that starts the detached session for `spec`.
/// `CLAUDE_CONFIG_DIR` comes from the profile unless the spec env sets it explicitly.
pub fn new_session_argv(spec: &SpawnSpec) -> Result<Vec<String>, AgentError> {
    new_session_argv_with(spec, &Prepared::default())
}

/// [`new_session_argv`] plus the MCP additions: `extra.env` never overrides the spec's env,
/// `extra.args` follow the profile args.
pub fn new_session_argv_with(
    spec: &SpawnSpec,
    extra: &Prepared,
) -> Result<Vec<String>, AgentError> {
    new_session_argv_exec(spec, &Exec::plain(spec), extra)
}

/// [`new_session_argv_with`] for a resolved profile: env precedence is spec, then MCP, then
/// the profile's own (`agents.json`) env.
pub fn new_session_argv_exec(
    spec: &SpawnSpec,
    exec: &Exec,
    extra: &Prepared,
) -> Result<Vec<String>, AgentError> {
    let mut env = spec.env().clone();
    for (k, v) in extra.env.iter().chain(&exec.env) {
        env.entry(k.clone()).or_insert_with(|| v.clone());
    }
    if let Some(bad) = env.keys().find(|k| !valid_env_name(k)) {
        return Err(AgentError::BadEnvName(bad.clone()));
    }
    let mut a = vec![
        s("new-session"),
        s("-d"),
        s("-s"),
        spec.agent_id().tmux_session(),
        s("-c"),
        spec.workdir().to_string_lossy().into_owned(),
        s("-x"),
        s("200"),
        s("-y"),
        s("50"),
        s("--"),
        s("env"),
    ];
    a.extend(env.iter().map(|(k, v)| format!("{k}={v}")));
    a.push(exec.binary.clone());
    a.extend(exec.args.iter().cloned());
    a.extend(extra.args.iter().cloned());
    Ok(a)
}

fn check_text(text: &str) -> Result<(), AgentError> {
    if text.chars().any(char::is_control) {
        Err(AgentError::ControlChars)
    } else {
        Ok(())
    }
}

pub fn send_text_argv(id: &AgentId, text: &str) -> Result<Vec<String>, AgentError> {
    check_text(text)?;
    Ok(vec![
        s("send-keys"),
        s("-t"),
        id.tmux_session(),
        s("-l"),
        s("--"),
        text.to_owned(),
    ])
}

pub fn send_enter_argv(id: &AgentId) -> Vec<String> {
    vec![s("send-keys"), s("-t"), id.tmux_session(), s("Enter")]
}

pub fn kill_argv(id: &AgentId) -> Vec<String> {
    vec![s("kill-session"), s("-t"), id.tmux_session()]
}

pub fn list_argv() -> Vec<String> {
    vec![s("list-sessions"), s("-F"), s("#{session_name}")]
}

fn set_profile_argv(id: &AgentId, key: &ProfileKey) -> Vec<String> {
    vec![
        s("set-option"),
        s("-t"),
        id.tmux_session(),
        s(PROFILE_OPT),
        key.as_str().to_owned(),
    ]
}

fn capture_plain_argv(id: &AgentId) -> Vec<String> {
    vec![s("capture-pane"), s("-p"), s("-t"), id.tmux_session()]
}

pub fn cursor_argv(id: &AgentId) -> Vec<String> {
    vec![
        s("display-message"),
        s("-p"),
        s("-t"),
        id.tmux_session(),
        s("#{cursor_x} #{cursor_y}"),
    ]
}

/// Parses `display-message` output `"<x> <y>"` (0-based).
fn parse_cursor(out: &str) -> Option<(usize, usize)> {
    let mut it = out.split_whitespace();
    let (x, y) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    it.next().is_none().then_some((x, y))
}

/// One full redraw: clear, the captured screen without trailing blank rows, then the cursor.
/// The cursor goes where tmux says it is (1-based CUP), else it stays after the last text.
fn redraw_frame(screen: &str, cursor: Option<(usize, usize)>) -> String {
    let mut lines: Vec<&str> = screen.split('\n').collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let mut frame = format!("\x1b[H\x1b[2J{}", lines.join("\r\n"));
    if let Some((x, y)) = cursor {
        frame.push_str(&format!("\x1b[{};{}H", y + 1, x + 1));
    }
    frame
}

pub fn capture_argv(id: &AgentId) -> Vec<String> {
    vec![
        s("capture-pane"),
        s("-p"),
        s("-e"),
        s("-t"),
        id.tmux_session(),
    ]
}

fn load_buffer_argv(id: &AgentId) -> Vec<String> {
    vec![s("load-buffer"), s("-b"), id.tmux_session(), s("-")]
}

fn paste_argv(id: &AgentId, bracketed: bool) -> Vec<String> {
    let mut a = vec![s("paste-buffer"), s("-d"), s("-b"), id.tmux_session()];
    if bracketed {
        a.push(s("-p"));
    }
    a.extend([s("-t"), id.tmux_session()]);
    a
}

#[derive(Debug, Clone)]
pub struct Tmux {
    socket: Option<String>,
    live: Arc<RwLock<Live>>,
    installer: Arc<Installer>,
    blocks: Blocks,
    /// (runtime dir for per-agent MCP configs, hub CA file); `None`: agents get no MCP wiring.
    mcp: Option<(PathBuf, PathBuf)>,
    /// Repo spawns: clone settings; `None` refuses them.
    git: Option<GitCfg>,
    tokens: Arc<GitStore>,
    scan: Duration,
    slow_scan: Duration,
    events: mpsc::UnboundedSender<HostEvent>,
    event_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<HostEvent>>>,
    startup_delay: Duration,
    submit_delay: Duration,
    /// Agents whose prompt could not be typed; the hub session drains it.
    prompt_failed: mpsc::UnboundedSender<AgentId>,
    prompt_failures: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<AgentId>>>,
    claims: Claims,
    /// Held across scan+pick+claim (spawn) and scan+judge+delete (clean).
    repo_lock: Arc<tokio::sync::Mutex<()>>,
}

impl Tmux {
    /// `socket`: tmux `-L` name; `None` uses the user's default server.
    pub fn new(socket: Option<String>) -> Self {
        Self::with_agents(socket, Agents::default())
    }

    /// Like [`Tmux::new`] with the install/login recipes of `agents.json`.
    pub fn with_agents(socket: Option<String>, agents: Agents) -> Self {
        let (prompt_failed, rx) = mpsc::unbounded_channel();
        let (events, event_rx) = mpsc::unbounded_channel();
        Self {
            socket,
            live: Arc::new(RwLock::new(Live {
                local_agents: agents.clone(),
                local_settings: Settings::default(),
                agents: Arc::new(agents),
                hub_names: Arc::default(),
                settings: Arc::default(),
            })),
            installer: Arc::default(),
            blocks: Arc::default(),
            mcp: None,
            git: None,
            tokens: Arc::default(),
            scan: Duration::from_secs(1),
            slow_scan: Duration::from_secs(30),
            events,
            event_rx: Arc::new(tokio::sync::Mutex::new(event_rx)),
            startup_delay: Duration::from_millis(1500),
            submit_delay: Duration::from_millis(300),
            prompt_failed,
            prompt_failures: Arc::new(tokio::sync::Mutex::new(rx)),
            claims: Claims::default(),
            repo_lock: Arc::default(),
        }
    }

    /// `ulakd.json`: repo roots, index interval, cleanup policy.
    pub fn with_settings(self, settings: Settings) -> Self {
        {
            let mut l = self.live.write().expect("live config lock");
            l.settings = Arc::new(settings.clone());
            l.local_settings = settings;
        }
        self
    }

    /// Applies the hub's `Config`: profiles and settings overlay the local ones, replacing the
    /// previous `Config`. Running agents are unaffected; new spawns use the new set.
    pub fn apply_config(&self, profiles: &[HubProfile], settings: &NodeSettings) {
        let mut l = self.live.write().expect("live config lock");
        let (agents, names) = l.local_agents.with_hub(profiles);
        l.agents = Arc::new(agents);
        l.hub_names = Arc::new(names);
        l.settings = Arc::new(l.local_settings.overlay(settings));
    }

    /// The profiles in effect (local plus hub).
    pub fn agents(&self) -> Arc<Agents> {
        Arc::clone(&self.live.read().expect("live config lock").agents)
    }

    /// The settings in effect (`ulakd.json` plus hub).
    pub fn settings(&self) -> Arc<Settings> {
        Arc::clone(&self.live.read().expect("live config lock").settings)
    }

    /// Wires spawned agents to the hub's MCP server (see [`mcp`]): configs live in `runtime_dir`,
    /// `ca_file` is the hub CA.
    pub fn with_mcp(mut self, runtime_dir: PathBuf, ca_file: PathBuf) -> Self {
        self.mcp = Some((runtime_dir, ca_file));
        self
    }

    /// Enables repo spawns (clone into `cfg.work_root`).
    pub fn with_git(mut self, cfg: GitCfg) -> Self {
        self.git = Some(cfg);
        self
    }

    /// Per-agent run files: the MCP config and the agent's git helper.
    fn drop_mcp_file(&self, id: &AgentId) {
        if let Some((dir, _)) = &self.mcp {
            let _ = std::fs::remove_file(mcp::file_for(dir, id));
        }
        if let Some(g) = &self.git {
            let _ = std::fs::remove_dir_all(git::helper_dir(&g.run_dir, id));
        }
    }

    /// Clones the repo of a `Source::Repo` spec and returns the spec that starts in the clone.
    /// The token is taken out of the store first (an install may outlast its expiry).
    async fn prepare_repo(
        &self,
        spec: &SpawnSpec,
    ) -> Result<(SpawnSpec, Option<Claim>), AgentError> {
        let Source::Repo {
            repo,
            branch,
            allow_push,
            fresh,
            ..
        } = spec.source()
        else {
            return Ok((spec.clone(), None));
        };
        let id = spec.agent_id();
        let token: GitToken = self.tokens.take(id).ok_or_else(|| {
            AgentError::Git("no git access arrived for this agent (or it expired)".into())
        })?;
        let cfg = self
            .git
            .as_ref()
            .ok_or_else(|| AgentError::Git("repo spawns are not enabled on this node".into()))?;
        self.ensure_installed(spec, &self.agents().exec_spec(spec))
            .await?;
        let reused = if fresh {
            None
        } else {
            self.reuse_clone(cfg, id, &repo, branch.as_ref(), &token, allow_push)
                .await?
        };
        let (dir, claim) = match reused {
            Some(r) => r,
            None => {
                let work = cfg
                    .work_root
                    .canonicalize()
                    .unwrap_or_else(|_| cfg.work_root.clone());
                let claim = {
                    let _g = self.repo_lock.lock().await;
                    self.claim(work.join(id.as_str()).join(repo.name()), id)
                };
                let dir =
                    git::clone_repo(cfg, id, &repo, branch.as_ref(), &token, allow_push).await?;
                (dir, claim)
            }
        };
        let mut env = spec.env().clone();
        env.remove(git::TOKEN_ENV);
        if allow_push {
            let h = git::Helper::create_with_token(git::helper_dir(&cfg.run_dir, id), &token)?;
            env.insert(
                "GIT_ASKPASS".into(),
                h.script.to_string_lossy().into_owned(),
            );
            env.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
            std::mem::forget(h); // lives until the agent ends (drop_mcp_file / adopt sweep)
        }
        let spec = SpawnSpec::new(
            id.clone(),
            spec.profile().clone(),
            dir,
            spec.prompt().to_owned(),
            env,
        )?;
        Ok((spec, Some(claim)))
    }

    fn claim(&self, path: PathBuf, id: &AgentId) -> Claim {
        self.claims
            .lock()
            .expect("claims lock")
            .insert(path.clone(), id.clone());
        Claim {
            claims: Arc::clone(&self.claims),
            path,
        }
    }

    fn ctx(&self, cfg: &GitCfg) -> repos::Ctx {
        repos::Ctx {
            program: cfg.program().to_owned(),
            host: cfg.host().to_owned(),
            work_root: cfg.work_root.clone(),
            roots: self.settings().repo_roots.clone(),
        }
    }

    /// Canonical paths in use: claims plus the start folders of the `ulak-*` sessions. An error
    /// means "unknown": callers must not delete anything then.
    async fn busy(&self) -> Result<HashSet<PathBuf>, AgentError> {
        let argv = vec![
            s("list-sessions"),
            s("-F"),
            s("#{session_name} #{session_path}"),
        ];
        let out = match self.run("list-sessions", argv, None).await {
            Ok(o) => o,
            Err(e) if no_server(&e) => String::new(),
            Err(e) => return Err(e),
        };
        let mut b: Vec<PathBuf> = out
            .lines()
            .filter_map(|l| l.strip_prefix("ulak-")?.split_once(' '))
            .map(|(_, p)| PathBuf::from(p))
            .collect();
        b.extend(self.claims.lock().expect("claims lock").keys().cloned());
        Ok(b.into_iter()
            .map(|p| p.canonicalize().unwrap_or(p))
            .collect())
    }

    /// Scan under the repo lock (the caller holds it).
    async fn scan_locked(&self, ctx: &repos::Ctx, busy: &HashSet<PathBuf>) -> Vec<repos::Found> {
        let (ctx, busy) = (ctx.clone(), busy.clone());
        tokio::task::spawn_blocking(move || repos::scan(&ctx, &busy, repos::now_ms()))
            .await
            .expect("scan task does not panic")
    }

    /// Reuses an unused work-root clone of `repo`: claims it, fetches, checks out `branch`.
    /// `Ok(None)`: none to reuse (or in-use state unknown), clone anew. A dirty clone is an error.
    async fn reuse_clone(
        &self,
        cfg: &GitCfg,
        id: &AgentId,
        repo: &RepoRef,
        branch: Option<&BranchName>,
        token: &GitToken,
        allow_push: bool,
    ) -> Result<Option<(PathBuf, Claim)>, AgentError> {
        let ctx = self.ctx(cfg);
        let (path, claim) = {
            let _g = self.repo_lock.lock().await;
            let Ok(busy) = self.busy().await else {
                return Ok(None);
            };
            // ponytail: a full scan per spawn; index by full_name if roots get large
            let mut found = self.scan_locked(&ctx, &busy).await;
            // only the daemon's own clones: a user's checkout is never switched under them
            found.retain(|f| f.entry.managed);
            let Some(f) = repos::pick(&found, repo, &busy) else {
                return Ok(None);
            };
            let p = PathBuf::from(&f.entry.path);
            (p.clone(), self.claim(p, id))
        };
        let helper = git::Helper::create(cfg.work_root.join(format!(".askpass-{}", id.as_str())))?;
        let env = git::clone_env(cfg.host(), &helper.script, token);
        let (repo, branch, p, timeout) = (repo.clone(), branch.cloned(), path.clone(), cfg.timeout);
        tokio::task::spawn_blocking(move || -> Result<(), AgentError> {
            repos::reuse(&ctx, &repo, branch.as_ref(), &p, &env, timeout)?;
            // same push guard rail as a fresh clone
            let args: &[&str] = if allow_push {
                &["config", "--local", "--unset-all", "remote.origin.pushurl"]
            } else {
                &["config", "--local", "remote.origin.pushurl", "no-push-here"]
            };
            let o = repos::git(
                &ctx.program,
                &p,
                args,
                &(vec![], vec![]),
                Duration::from_secs(30),
            )
            .map_err(AgentError::Git)?;
            // exit 5: nothing to unset
            if !o.ok && !allow_push {
                return Err(AgentError::Git(
                    "cannot set the push guard on the clone".into(),
                ));
            }
            Ok(())
        })
        .await
        .expect("reuse task does not panic")?;
        drop(helper);
        Ok(Some((path, claim)))
    }

    /// Runs a profile's install command now (a reinstall if it is installed already).
    pub async fn install_profile(&self, name: &str) -> Result<(), String> {
        let agents = Arc::clone(&self.live.read().expect("live config lock").agents);
        if !agents.names().any(|n| n == name) {
            return Err(format!("no profile named {name} on this node"));
        }
        let r = agents.recipe(name);
        let binary = r.binary.as_deref().map_or_else(
            || name.to_owned(),
            |b| {
                repos::expand_tilde(Path::new(b))
                    .to_string_lossy()
                    .into_owned()
            },
        );
        self.installer.ensure(&binary, &r).await
    }

    /// The node's named profiles (`agents.json` plus defaults plus the hub's).
    pub async fn profiles(&self) -> Vec<ProfileInfo> {
        let (agents, hub) = {
            let l = self.live.read().expect("live config lock");
            (Arc::clone(&l.agents), Arc::clone(&l.hub_names))
        };
        let blocked: HashSet<String> = self
            .blocks
            .lock()
            .expect("blocks lock")
            .iter()
            .filter(|(_, b)| b.0)
            .map(|(k, _)| k.as_str().to_owned())
            .collect();
        let infos = agents.names().map(|name| {
            let r = agents.recipe(name);
            let tilde = |p: &str| {
                repos::expand_tilde(Path::new(p))
                    .to_string_lossy()
                    .into_owned()
            };
            let binary = r.binary.as_deref().map_or_else(|| name.to_owned(), tilde);
            let config_dir = r.config_dir.as_deref().map(tilde);
            let login_blocked = blocked.contains(name);
            let from_hub = hub.contains(name);
            let installable = r.install.as_ref().is_some_and(|a| !a.is_empty());
            let name = name.to_owned();
            async move {
                ProfileInfo {
                    installed: is_installed(&r, &binary).await,
                    name,
                    binary,
                    config_dir,
                    login_blocked,
                    from_hub,
                    installable,
                }
            }
        });
        futures_util::future::join_all(infos).await
    }

    /// The repo index; `None` when repo spawns are off on this node.
    pub async fn repo_index(&self) -> Option<Vec<RepoEntry>> {
        let ctx = self.ctx(self.git.as_ref()?);
        let _g = self.repo_lock.lock().await;
        let busy = match self.busy().await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("ulakd: repo index: cannot list sessions: {e}");
                return None;
            }
        };
        Some(repos::entries(&self.scan_locked(&ctx, &busy).await))
    }

    /// Judges and (unless `dry_run`) deletes the given index entries, all under the repo lock.
    async fn clean_with(
        &self,
        dry_run: bool,
        select: impl FnOnce(Vec<repos::Found>) -> Result<Vec<repos::Found>, CleanItem> + Send + 'static,
        now: u64,
    ) -> Option<Cleaned> {
        let cfg = self.git.as_ref()?;
        let ctx = self.ctx(cfg);
        let _g = self.repo_lock.lock().await;
        let busy = match self.busy().await {
            Ok(b) => b,
            Err(e) => {
                return Some(Cleaned {
                    removed: Vec::new(),
                    kept: vec![CleanItem {
                        path: cfg.work_root.to_string_lossy().into_owned(),
                        full_name: None,
                        reason: format!("cannot tell which clones are in use: {e}"),
                    }],
                })
            }
        };
        let found = self.scan_locked(&ctx, &busy).await;
        let c = tokio::task::spawn_blocking(move || match select(found) {
            Ok(t) => repos::clean(&ctx, &t, &busy, dry_run, &|_| None, now),
            Err(item) => Cleaned {
                removed: Vec::new(),
                kept: vec![item],
            },
        })
        .await
        .expect("clean task does not panic");
        Some(c)
    }

    /// Every eligible work-root clone. `None`: repo spawns are off.
    pub async fn clean_repos(&self, dry_run: bool) -> Option<Cleaned> {
        self.clean_with(
            dry_run,
            |f| Ok(f.into_iter().filter(|f| f.entry.managed).collect()),
            repos::now_ms(),
        )
        .await
    }

    /// One clone; `path` must equal the path of an indexed entry (compared as a string).
    pub async fn clean_repo(&self, path: String) -> Option<Cleaned> {
        self.clean_with(
            false,
            move |f| {
                let t: Vec<_> = f.into_iter().filter(|f| f.entry.path == path).collect();
                if t.is_empty() {
                    Err(CleanItem {
                        path,
                        full_name: None,
                        reason: "not an indexed clone of this node".into(),
                    })
                } else {
                    Ok(t)
                }
            },
            repos::now_ms(),
        )
        .await
    }

    /// Automatic cleanup (when `clean.auto`): clones idle for `after_exit_hours` at `now`.
    pub async fn maintain(&self, now: u64) -> Option<Cleaned> {
        let settings = self.settings();
        if !settings.clean.auto {
            return None;
        }
        let hours = settings.clean.after_exit_hours;
        self.clean_with(false, move |f| Ok(repos::auto_due(&f, now, hours)), now)
            .await
    }

    /// Waits before typing the prompt (the agent TUI must be up) and before pressing Enter.
    pub fn with_delays(mut self, startup: Duration, submit: Duration) -> Self {
        self.startup_delay = startup;
        self.submit_delay = submit;
        self
    }

    /// Pane scan intervals: `fast` inside the profile's `login_scan_secs` window, then `slow`.
    pub fn with_scan(mut self, fast: Duration, slow: Duration) -> Self {
        self.scan = fast;
        self.slow_scan = slow;
        self
    }

    fn emit(&self, e: HostEvent) {
        // the receiver lives as long as any Tmux clone, including this one
        let _ = self.events.send(e);
    }

    async fn run(
        &self,
        action: &'static str,
        args: Vec<String>,
        stdin: Option<&[u8]>,
    ) -> Result<String, AgentError> {
        let mut cmd = Command::new("tmux");
        if let Some(sock) = &self.socket {
            cmd.args(["-L", sock]);
        }
        cmd.args(&args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(data).await?;
        }
        let out = child.wait_with_output().await?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(AgentError::Tmux {
                action,
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            })
        }
    }

    async fn paste(&self, id: &AgentId, data: &[u8], bracketed: bool) -> Result<(), AgentError> {
        self.run("load-buffer", load_buffer_argv(id), Some(data))
            .await?;
        self.run("paste-buffer", paste_argv(id, bracketed), None)
            .await?;
        Ok(())
    }

    async fn ensure_running(&self, id: &AgentId) -> Result<(), AgentError> {
        if self.list_agents().await?.contains(id) {
            Ok(())
        } else {
            Err(AgentError::NotRunning(id.as_str().to_owned()))
        }
    }

    pub async fn list_agents(&self) -> Result<Vec<AgentId>, AgentError> {
        match self.run("list-sessions", list_argv(), None).await {
            Ok(out) => Ok(out
                .lines()
                .filter_map(|l| l.strip_prefix("ulak-"))
                .filter_map(|id| AgentId::new(id).ok())
                .collect()),
            // no server running == no sessions
            Err(e) if no_server(&e) => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    /// Makes sure the agent binary exists, installing it from the local recipe if needed.
    async fn ensure_installed(&self, spec: &SpawnSpec, exec: &Exec) -> Result<(), AgentError> {
        let (binary, recipe, id) = (exec.binary.as_str(), &exec.recipe, spec.agent_id());
        if is_installed(recipe, binary).await {
            return Ok(());
        }
        self.emit(HostEvent::Status {
            agent: id.clone(),
            status: AgentStatus::Installing,
        });
        let Err(detail) = self.installer.ensure(binary, recipe).await else {
            return Ok(());
        };
        let profile = spec.profile().key()?;
        self.emit(HostEvent::Blocked {
            agent: id.clone(),
            profile,
            kind: BlockerKind::Install,
            detail: detail.clone(),
        });
        Err(AgentError::Install(detail))
    }

    pub async fn spawn_agent(&self, spec: &SpawnSpec) -> Result<(), AgentError> {
        let owned;
        let _claim; // released once the session (and its start folder) exists or the spawn failed
        let spec = if matches!(spec.source(), Source::Repo { .. }) {
            (owned, _claim) = self.prepare_repo(spec).await?;
            &owned
        } else {
            spec
        };
        let r = self.spawn_prepared(spec).await;
        if r.is_err() {
            if let Some(g) = &self.git {
                // a failed spawn leaves no helper behind (an existing agent keeps its own)
                if !matches!(r, Err(AgentError::AlreadyExists(_))) {
                    let _ = std::fs::remove_dir_all(git::helper_dir(&g.run_dir, spec.agent_id()));
                }
            }
        }
        r
    }

    async fn spawn_prepared(&self, spec: &SpawnSpec) -> Result<(), AgentError> {
        let exec = self.agents().exec_spec(spec);
        let extra = match &self.mcp {
            Some((dir, ca)) => mcp::prepare(&exec.recipe, spec, ca, dir)?,
            None => Prepared::default(),
        };
        let argv = new_session_argv_exec(spec, &exec, &extra)?;
        if let Err(e) = self.ensure_installed(spec, &exec).await {
            self.drop_mcp_file(spec.agent_id());
            return Err(e);
        }
        match self.run("new-session", argv, None).await {
            Err(AgentError::Tmux { stderr, .. }) if stderr.contains("duplicate session") => {
                // the file belongs to the running agent of that name
                return Err(AgentError::AlreadyExists(
                    spec.agent_id().as_str().to_owned(),
                ));
            }
            Err(e) => {
                self.drop_mcp_file(spec.agent_id());
                return Err(e);
            }
            Ok(_) => {}
        };
        let key = spec.profile().key()?;
        if let Err(e) = self
            .run("set-option", set_profile_argv(spec.agent_id(), &key), None)
            .await
        {
            eprintln!("ulakd: cannot tag the session with its profile: {e}");
        }
        self.watch_login(spec.agent_id().clone(), key, true);
        // Typing the prompt waits for the TUI to come up; do it off the caller's loop.
        let (me, id, prompt) = (
            self.clone(),
            spec.agent_id().clone(),
            spec.prompt().to_owned(),
        );
        tokio::spawn(async move {
            if let Err(e) = me.type_prompt(&id, &prompt).await {
                eprintln!("ulakd: typing the prompt for {id:?} failed: {e}");
                // receiver lives as long as any Tmux clone, including this one
                let _ = me.prompt_failed.send(id);
            }
        });
        Ok(())
    }

    /// Re-adopts the `ulak-*` sessions of a previous daemon run: login watchers restart for
    /// every session that carries its profile tag. (Listing them is [`Tmux::list_agents`].)
    pub async fn adopt(&self) -> Result<usize, AgentError> {
        let argv = vec![
            s("list-sessions"),
            s("-F"),
            format!("#{{session_name}} #{{{PROFILE_OPT}}}"),
        ];
        let out = match self.run("list-sessions", argv, None).await {
            Ok(o) => o,
            Err(AgentError::Tmux { stderr, .. })
                if stderr.contains("no server running") || stderr.contains("No such file") =>
            {
                if let Some((dir, _)) = &self.mcp {
                    mcp::cleanup(dir, &[]);
                }
                if let Some(g) = &self.git {
                    git::cleanup(&g.run_dir, &[]);
                }
                return Ok(0);
            }
            Err(e) => return Err(e),
        };
        let mut n = 0;
        if let Some((dir, _)) = &self.mcp {
            let alive: Vec<AgentId> = out
                .lines()
                .filter_map(|l| l.split(' ').next()?.strip_prefix("ulak-"))
                .filter_map(|i| AgentId::new(i).ok())
                .collect();
            mcp::cleanup(dir, &alive);
        }
        if let Some(g) = &self.git {
            let alive: Vec<AgentId> = out
                .lines()
                .filter_map(|l| l.split(' ').next()?.strip_prefix("ulak-"))
                .filter_map(|i| AgentId::new(i).ok())
                .collect();
            git::cleanup(&g.run_dir, &alive);
        }
        for l in out.lines() {
            let Some((name, key)) = l.split_once(' ') else {
                continue;
            };
            let (Some(id), Ok(key)) = (
                name.strip_prefix("ulak-")
                    .and_then(|i| AgentId::new(i).ok()),
                ProfileKey::new(key.trim()),
            ) else {
                continue;
            };
            self.watch_login(id, key, false);
            n += 1;
        }
        Ok(n)
    }

    /// Scans the pane for the profile's login patterns until the session is gone: every `scan`
    /// for the first `login_scan_secs`, then every `slow_scan`. One blocker per profile until
    /// `blocker_resolved`; a phrase still on screen at resolve time does not re-trigger until
    /// it has left the screen once.
    fn watch_login(&self, id: AgentId, key: ProfileKey, fresh: bool) {
        let name = binary_name(key.as_str().split('@').next().unwrap_or_default()).to_owned();
        let Recipe {
            login_patterns,
            login_scan_secs,
            ..
        } = self.agents().recipe(&name);
        if login_patterns.is_empty() {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            let gen_of = |me: &Tmux| {
                me.blocks
                    .lock()
                    .expect("blocks lock")
                    .get(&key)
                    .map_or(0, |b| b.1)
            };
            let mut gen_seen = gen_of(&me);
            let mut stale = false;
            if fresh {
                // the TUI needs a moment before there is anything to scan
                tokio::time::sleep(me.scan).await;
            }
            loop {
                let Ok(text) = me.run("capture-pane", capture_plain_argv(&id), None).await else {
                    me.drop_mcp_file(&id);
                    break; // session is gone
                };
                let hit = match_login(&login_patterns, &text);
                let g = gen_of(&me);
                if g != gen_seen {
                    gen_seen = g;
                    stale = hit.is_some();
                }
                match hit {
                    None => stale = false,
                    Some(_) if stale => {}
                    Some(line) => {
                        let first = {
                            let mut b = me.blocks.lock().expect("blocks lock");
                            let e = b.entry(key.clone()).or_default();
                            !std::mem::replace(&mut e.0, true)
                        };
                        if first {
                            me.emit(HostEvent::Status {
                                agent: id.clone(),
                                status: AgentStatus::NeedsLogin,
                            });
                            me.emit(HostEvent::Blocked {
                                agent: id.clone(),
                                profile: key.clone(),
                                kind: BlockerKind::Login,
                                detail: line,
                            });
                        }
                    }
                }
                let fast = started.elapsed() < Duration::from_secs(login_scan_secs);
                tokio::time::sleep(if fast { me.scan } else { me.slow_scan }).await;
            }
        });
    }

    async fn type_prompt(&self, id: &AgentId, prompt: &str) -> Result<(), AgentError> {
        tokio::time::sleep(self.startup_delay).await;
        self.paste(id, prompt.as_bytes(), true).await?;
        tokio::time::sleep(self.submit_delay).await;
        self.run("send-keys", send_enter_argv(id), None)
            .await
            .map(drop)
    }

    pub async fn kill_agent(&self, id: &AgentId) -> Result<(), AgentError> {
        let r = self.run("kill-session", kill_argv(id), None).await;
        self.drop_mcp_file(id);
        r.map(drop)
    }

    /// Types `text` then Enter. Text with control characters is rejected.
    pub async fn nudge_agent(&self, id: &AgentId, text: &str) -> Result<(), AgentError> {
        let argv = send_text_argv(id, text)?;
        self.run("send-keys", argv, None).await?;
        tokio::time::sleep(self.submit_delay).await;
        self.run("send-keys", send_enter_argv(id), None)
            .await
            .map(drop)
    }

    /// Raw terminal input (keystrokes from the UI), pasted as-is.
    pub async fn input_agent(&self, id: &AgentId, data: &[u8]) -> Result<(), AgentError> {
        self.ensure_running(id).await?;
        self.paste(id, data, false).await
    }

    /// Streams the pane as terminal bytes: each change is a full redraw (clear + screen).
    /// Dropping the receiver stops the poller.
    // ponytail: capture-pane polling at 250 ms, full-screen redraws; pipe-pane diffs if bandwidth matters.
    pub async fn attach_agent(&self, id: &AgentId) -> Result<mpsc::Receiver<Vec<u8>>, AgentError> {
        self.ensure_running(id).await?;
        let (tx, rx) = mpsc::channel(8);
        let (me, id) = (self.clone(), id.clone());
        tokio::spawn(async move {
            let mut last = String::new();
            loop {
                let Ok(screen) = me.run("capture-pane", capture_argv(&id), None).await else {
                    break; // session is gone
                };
                if screen != last {
                    let cursor = me
                        .run("display-message", cursor_argv(&id), None)
                        .await
                        .ok()
                        .and_then(|o| parse_cursor(&o));
                    let frame = redraw_frame(&screen, cursor);
                    if tx.send(frame.into_bytes()).await.is_err() {
                        break;
                    }
                    last = screen;
                } else if tx.is_closed() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });
        Ok(rx)
    }
}

impl AgentHost for Tmux {
    fn git_access(&self, agent: AgentId, token: GitToken) {
        self.tokens.put(agent, token);
    }
    async fn spawn(&self, spec: &SpawnSpec) -> Result<(), AgentError> {
        self.spawn_agent(spec).await
    }
    async fn kill(&self, id: &AgentId) -> Result<(), AgentError> {
        self.kill_agent(id).await
    }
    async fn nudge(&self, id: &AgentId, text: &str) -> Result<(), AgentError> {
        self.nudge_agent(id, text).await
    }
    async fn input(&self, id: &AgentId, data: &[u8]) -> Result<(), AgentError> {
        self.input_agent(id, data).await
    }
    async fn list(&self) -> Result<Vec<AgentId>, AgentError> {
        self.list_agents().await
    }
    async fn attach(&self, id: &AgentId) -> Result<mpsc::Receiver<Vec<u8>>, AgentError> {
        self.attach_agent(id).await
    }
    async fn next_prompt_failure(&self) -> Option<AgentId> {
        self.prompt_failures.lock().await.recv().await
    }
    async fn next_event(&self) -> Option<HostEvent> {
        self.event_rx.lock().await.recv().await
    }
    fn blocker_resolved(&self, profile: &ProfileKey) {
        let mut b = self.blocks.lock().expect("blocks lock");
        let e = b.entry(profile.clone()).or_default();
        *e = (false, e.1 + 1);
    }
    async fn profiles(&self) -> Option<Vec<ProfileInfo>> {
        Some(Tmux::profiles(self).await)
    }
    async fn install(&self, name: String) -> Result<(), String> {
        self.install_profile(&name).await
    }
    async fn repo_index(&self) -> Option<Vec<RepoEntry>> {
        Tmux::repo_index(self).await
    }
    async fn clean(&self, req: CleanReq) -> Option<Cleaned> {
        match req {
            CleanReq::All { dry_run } => self.clean_repos(dry_run).await,
            CleanReq::One { path } => self.clean_repo(path).await,
        }
    }
    fn maintenance_interval(&self) -> Duration {
        Duration::from_secs(self.settings().index_interval_mins.max(1) * 60)
    }
    fn apply_config(&self, profiles: &[HubProfile], settings: &NodeSettings) {
        Tmux::apply_config(self, profiles, settings);
    }
    async fn maintain(&self) -> Option<Cleaned> {
        Tmux::maintain(self, repos::now_ms()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, path::PathBuf};
    use ulak_proto::AgentProfile;

    fn spec(cfg: Option<&str>, env: &[(&str, &str)]) -> SpawnSpec {
        SpawnSpec::new(
            AgentId::new("a1").unwrap(),
            AgentProfile::new(
                "claude",
                vec!["--flag".into(), "x y".into()],
                cfg.map(PathBuf::from),
            )
            .unwrap(),
            PathBuf::from("/work/dir"),
            "do it".into(),
            env.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
        )
        .unwrap()
    }

    #[test]
    fn redraw_trims_trailing_blank_rows_and_keeps_content() {
        let f = redraw_frame("a\n\nb\n   \n\n\n", None);
        assert_eq!(f, "\x1b[H\x1b[2Ja\r\n\r\nb");
        // leading and inner blank rows stay
        assert_eq!(redraw_frame("\n\nx\n", None), "\x1b[H\x1b[2J\r\n\r\nx");
    }

    #[test]
    fn redraw_all_blank_pane_is_just_a_clear() {
        assert_eq!(redraw_frame("\n\n\n", None), "\x1b[H\x1b[2J");
        assert_eq!(redraw_frame("", Some((0, 0))), "\x1b[H\x1b[2J\x1b[1;1H");
    }

    #[test]
    fn redraw_places_the_cursor_where_tmux_reports_it() {
        assert_eq!(
            redraw_frame("ab\ncd\n\n", Some((3, 1))),
            "\x1b[H\x1b[2Jab\r\ncd\x1b[2;4H"
        );
    }

    #[test]
    fn cursor_output_parses_strictly() {
        assert_eq!(parse_cursor("12 3\n"), Some((12, 3)));
        assert_eq!(parse_cursor(""), None);
        assert_eq!(parse_cursor("1"), None);
        assert_eq!(parse_cursor("a b"), None);
        assert_eq!(parse_cursor("1 2 3"), None);
        assert!(cursor_argv(&AgentId::new("a1").unwrap())
            .contains(&"#{cursor_x} #{cursor_y}".to_owned()));
    }

    #[test]
    fn new_session_argv_has_env_and_no_shell() {
        let a = new_session_argv(&spec(Some("/cfg"), &[("TOKEN", "t o k")])).unwrap();
        assert_eq!(
            a,
            [
                "new-session",
                "-d",
                "-s",
                "ulak-a1",
                "-c",
                "/work/dir",
                "-x",
                "200",
                "-y",
                "50",
                "--",
                "env",
                "CLAUDE_CONFIG_DIR=/cfg",
                "TOKEN=t o k",
                "claude",
                "--flag",
                "x y"
            ]
        );
    }

    #[test]
    fn mcp_additions_follow_profile_args_and_never_override_env() {
        let extra = Prepared {
            args: vec!["--mcp-config".into(), "/run/m.json".into()],
            env: [("NODE_EXTRA_CA_CERTS".to_string(), "/ca.pem".to_string())].into(),
            file: None,
        };
        let a = new_session_argv_with(&spec(None, &[]), &extra).unwrap();
        assert_eq!(
            a[12..],
            [
                "NODE_EXTRA_CA_CERTS=/ca.pem",
                "claude",
                "--flag",
                "x y",
                "--mcp-config",
                "/run/m.json"
            ]
        );
        let a = new_session_argv_with(&spec(None, &[("NODE_EXTRA_CA_CERTS", "/mine")]), &extra)
            .unwrap();
        assert!(a.contains(&"NODE_EXTRA_CA_CERTS=/mine".to_string()));
        assert!(!a.contains(&"NODE_EXTRA_CA_CERTS=/ca.pem".to_string()));
    }

    #[test]
    fn explicit_env_beats_profile_config_dir() {
        let a = new_session_argv(&spec(Some("/cfg"), &[("CLAUDE_CONFIG_DIR", "/mine")])).unwrap();
        assert!(a.contains(&"CLAUDE_CONFIG_DIR=/mine".to_string()));
        assert!(!a.contains(&"CLAUDE_CONFIG_DIR=/cfg".to_string()));
    }

    #[tokio::test]
    async fn two_claude_profiles_run_claude_and_block_independently() {
        use crate::agents::tests::{spec_for, TWO_CLAUDES};
        let agents = Agents::from_json(TWO_CLAUDES).unwrap();
        let home = std::env::var("HOME").unwrap();
        let sp = spec_for("claude-work", None, &[("A", "x")]);
        let a = new_session_argv_exec(&sp, &agents.exec_spec(&sp), &Prepared::default()).unwrap();
        assert_eq!(
            a[12..],
            [
                "A=x".to_string(),
                format!("CLAUDE_CONFIG_DIR={home}/.claude-work"),
                "claude".into(),
                "--y".into()
            ]
        );
        let tm = Tmux::with_agents(Some(format!("ulakd-unit-{}", std::process::id())), agents);
        let work = sp.profile().key().unwrap();
        tm.blocks.lock().unwrap().insert(work.clone(), (true, 0));
        let blocked =
            |p: &[ProfileInfo], n: &str| p.iter().find(|i| i.name == n).unwrap().login_blocked;
        let p = tm.profiles().await;
        assert!(
            blocked(&p, "claude-work") && !blocked(&p, "claude-personal") && !blocked(&p, "claude")
        );
        let i = p.iter().find(|i| i.name == "claude-personal").unwrap();
        assert_eq!(
            (i.binary.as_str(), i.config_dir.clone()),
            ("claude", Some(format!("{home}/.claude-personal")))
        );
        tm.blocker_resolved(&work);
        assert!(!blocked(&tm.profiles().await, "claude-work"));
    }

    #[test]
    fn bad_env_name_rejected() {
        assert!(matches!(
            new_session_argv(&spec(None, &[("A=B", "x")])),
            Err(AgentError::BadEnvName(_))
        ));
        assert!(new_session_argv(&spec(None, &[("1A", "x")])).is_err());
    }

    #[test]
    fn nudge_rejects_control_chars() {
        let id = AgentId::new("a1").unwrap();
        assert!(matches!(
            send_text_argv(&id, "a\nb"),
            Err(AgentError::ControlChars)
        ));
        assert!(send_text_argv(&id, "a\x1b[31m").is_err());
        assert_eq!(
            send_text_argv(&id, "-n check q-1").unwrap(),
            ["send-keys", "-t", "ulak-a1", "-l", "--", "-n check q-1"]
        );
    }

    #[test]
    fn small_argvs() {
        let id = AgentId::new("a1").unwrap();
        assert_eq!(kill_argv(&id), ["kill-session", "-t", "ulak-a1"]);
        assert_eq!(
            send_enter_argv(&id),
            ["send-keys", "-t", "ulak-a1", "Enter"]
        );
        assert_eq!(
            paste_argv(&id, true),
            ["paste-buffer", "-d", "-b", "ulak-a1", "-p", "-t", "ulak-a1"]
        );
    }
}
