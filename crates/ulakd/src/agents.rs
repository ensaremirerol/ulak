//! `agents.json`: per-profile check, install recipe and login patterns. A profile is named by
//! the spawn's `profile.binary` (its file name); an entry may run another `binary` with its own
//! `args`, `config_dir` and `env` (e.g. two claude accounts as `claude-work` and
//! `claude-personal`), and then inherits whatever it leaves out from that binary's entry.
//! Local recipes are argv vectors, run without a shell.
//!
//! Hub profiles ([`Agents::with_hub`]) set `binary`, `args`, `config_dir`, `env` and optionally an
//! install script (run as `sh -c <script>`; the hub can already start any program through a
//! custom spawn, so this grants it nothing new). A hub profile of a local name keeps that entry's
//! check/login/MCP fields and its install recipe unless the hub gives a script; a new name
//! inherits them from its binary's local entry, if any. Check commands never come from the hub.
use crate::{errors::ConfigError, repos::expand_tilde};
use futures_util::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::Path,
    process::Stdio,
    sync::Mutex,
    time::Duration,
};
use tokio::process::Command;
use ulak_proto::{HubProfile, SpawnSpec};

const DETAIL_MAX: usize = 400;

fn d_install_timeout() -> u64 {
    600
}
fn d_scan() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    /// Exit 0 = installed. `None`: the binary is found in PATH.
    #[serde(default)]
    pub check: Option<Vec<String>>,
    /// Argv (first element resolved via PATH). `None`: no recipe, a missing binary is a blocker.
    #[serde(default)]
    pub install: Option<Vec<String>>,
    #[serde(default = "d_install_timeout")]
    pub install_timeout_secs: u64,
    /// Case-insensitive substrings that mean "the agent wants the user to log in".
    #[serde(default)]
    pub login_patterns: Vec<String>,
    /// Fast scanning window after spawn; afterwards the pane is scanned at a low rate.
    #[serde(default = "d_scan")]
    pub login_scan_secs: u64,
    /// How the agent learns about the hub's MCP server: `"claude"` (config file +
    /// `--mcp-config`) or `"none"`. Absent: `claude` for the binary `claude`, else `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<Mcp>,
    /// Extra argv for non-claude agents; placeholders `{mcp_url}`, `{token_env}` (name of the env
    /// var holding the agent token) and `{ca_file}` (hub CA pem). Used when `mcp` is not `claude`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_args: Vec<String>,
    /// The executable this profile runs (`~` expanded on the node). Absent: the profile name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// Argv placed before the spawn's own profile args.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Config folder (`~` expanded); for claude binaries it becomes `CLAUDE_CONFIG_DIR`. The
    /// spawn's own `profile.config_dir` wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_dir: Option<String>,
    /// Extra environment; the spawn's env wins.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// How one spawn is actually started (see [`Agents::exec_spec`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exec {
    pub binary: String,
    pub args: Vec<String>,
    /// Lower priority than the spec's env.
    pub env: BTreeMap<String, String>,
    pub config_dir: Option<String>,
    pub recipe: Recipe,
}

impl Exec {
    /// The spawn as given, without any `agents.json` entry.
    pub fn plain(spec: &SpawnSpec) -> Self {
        let config_dir = spec
            .profile()
            .config_dir()
            .map(|d| d.to_string_lossy().into_owned());
        Self {
            binary: spec.profile().binary().to_owned(),
            args: spec.profile().args().to_vec(),
            env: config_dir
                .iter()
                .map(|d| ("CLAUDE_CONFIG_DIR".to_owned(), d.clone()))
                .collect(),
            config_dir,
            recipe: Recipe::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mcp {
    Claude,
    None,
}

impl Default for Recipe {
    fn default() -> Self {
        Self {
            check: None,
            install: None,
            install_timeout_secs: d_install_timeout(),
            login_patterns: Vec::new(),
            login_scan_secs: d_scan(),
            mcp: None,
            mcp_args: Vec::new(),
            binary: None,
            args: Vec::new(),
            config_dir: None,
            env: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Agents(BTreeMap<String, Recipe>);

fn argv(a: &[&str]) -> Option<Vec<String>> {
    Some(a.iter().map(|s| (*s).to_owned()).collect())
}

impl Default for Agents {
    fn default() -> Self {
        let claude = Recipe {
            check: argv(&["claude", "--version"]),
            install: argv(&["npm", "install", "-g", "@anthropic-ai/claude-code"]),
            login_patterns: [
                "please run /login",
                "not logged in",
                "invalid api key",
                "please log in",
                "run /login",
            ]
            .map(str::to_owned)
            .to_vec(),
            ..Recipe::default()
        };
        // antigravity: check only; no install recipe is known.
        Self(BTreeMap::from([
            ("claude".to_owned(), claude),
            ("antigravity".to_owned(), Recipe::default()),
        ]))
    }
}

impl Agents {
    /// Defaults overlaid with `agents.json` (a file entry replaces the default of that name).
    /// A missing file is written with the defaults.
    pub fn load_or_init(dir: &Path) -> Result<Self, ConfigError> {
        let path = dir.join("agents.json");
        match std::fs::read_to_string(&path) {
            Ok(s) => Self::from_json(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let d = Self::default();
                std::fs::create_dir_all(dir)?;
                std::fs::write(&path, serde_json::to_string_pretty(&d)?)?;
                Ok(d)
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn from_json(s: &str) -> Result<Self, ConfigError> {
        let file: BTreeMap<String, Recipe> = serde_json::from_str(s)?;
        let mut me = Self::default();
        me.0.extend(file);
        Ok(me)
    }

    /// The recipe of a profile (file name); unknown profiles get check-only defaults. An entry
    /// with another `binary` takes `check`, `install`, `login_patterns` and `mcp` from that
    /// binary's entry when it has none of its own.
    pub fn recipe(&self, name: &str) -> Recipe {
        let name = binary_name(name);
        let mut r = self.0.get(name).cloned().unwrap_or_default();
        let Some(base_name) = r.binary.as_deref().map(binary_name).map(str::to_owned) else {
            return r;
        };
        if base_name == name {
            return r;
        }
        if let Some(base) = self.0.get(&base_name) {
            r.check = r.check.or_else(|| base.check.clone());
            r.install = r.install.or_else(|| base.install.clone());
            if r.login_patterns.is_empty() {
                r.login_patterns = base.login_patterns.clone();
            }
            r.mcp = r.mcp.or(base.mcp);
            if r.mcp_args.is_empty() {
                r.mcp_args = base.mcp_args.clone();
            }
        }
        if r.mcp.is_none() && base_name == "claude" {
            r.mcp = Some(Mcp::Claude); // the name no longer says "claude"
        }
        r
    }

    /// Resolves the profile of `spec` (named by its `profile.binary`) to what actually runs.
    pub fn exec_spec(&self, spec: &SpawnSpec) -> Exec {
        let mut e = Exec::plain(spec);
        let recipe = self.recipe(spec.profile().binary());
        if let Some(b) = &recipe.binary {
            e.binary = expand_tilde(Path::new(b)).to_string_lossy().into_owned();
        }
        e.args = recipe.args.iter().cloned().chain(e.args).collect();
        for (k, v) in &recipe.env {
            e.env.entry(k.clone()).or_insert_with(|| v.clone());
        }
        if e.config_dir.is_none() {
            e.config_dir = recipe
                .config_dir
                .as_deref()
                .map(|d| expand_tilde(Path::new(d)).to_string_lossy().into_owned());
            if let Some(d) = e
                .config_dir
                .as_ref()
                .filter(|_| binary_name(&e.binary) == "claude")
            {
                e.env.insert("CLAUDE_CONFIG_DIR".into(), d.clone());
            }
        }
        e.recipe = recipe;
        e
    }

    /// These (local) profiles overlaid with the hub's: returns the merged set and the names that
    /// came from the hub. Invalid hub profiles are skipped with a log line.
    pub fn with_hub(&self, hub: &[HubProfile]) -> (Self, BTreeSet<String>) {
        let mut me = self.clone();
        let mut names = BTreeSet::new();
        for p in hub {
            if let Err(e) = check_hub_profile(p) {
                eprintln!("ulakd: ignoring hub profile: {e}");
                continue;
            }
            // These fields (and an install script) come from the hub; never check/login/MCP.
            let base = self.0.get(&p.name).cloned().unwrap_or_default();
            let install = match &p.install {
                Some(script) => Some(vec!["sh".to_owned(), "-c".to_owned(), script.clone()]),
                None => base.install.clone(),
            };
            let r = Recipe {
                binary: Some(p.binary.clone()),
                args: p.args.clone(),
                config_dir: p.config_dir.clone(),
                env: p.env.clone(),
                install,
                ..base
            };
            me.0.insert(p.name.clone(), r);
            names.insert(p.name.clone());
        }
        (me, names)
    }

    /// Every named profile (defaults plus `agents.json`), sorted by name.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

fn printable(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.chars().count()) && !s.chars().any(char::is_control)
}

/// The limits documented on [`HubProfile`]. The error never echoes env values.
pub fn check_hub_profile(p: &HubProfile) -> Result<(), String> {
    let name_ok = (1..=64).contains(&p.name.len())
        && p.name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && p.name != "."
        && p.name != "..";
    if !name_ok {
        return Err("bad name".into());
    }
    let n = &p.name;
    if !printable(&p.binary, 256) {
        return Err(format!("{n}: bad binary"));
    }
    if p.args.len() > 32 || !p.args.iter().all(|a| printable(a, 512)) {
        return Err(format!("{n}: bad args"));
    }
    if p.config_dir.as_deref().is_some_and(|d| !printable(d, 512)) {
        return Err(format!("{n}: bad config_dir"));
    }
    let key_ok = |k: &str| {
        let mut c = k.chars();
        c.next()
            .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
            && c.all(|f| f.is_ascii_alphanumeric() || f == '_')
            && !k.starts_with("ULAK")
    };
    if p.env.len() > 32 || !p.env.keys().all(|k| key_ok(k)) {
        return Err(format!("{n}: bad env"));
    }
    if p.install
        .as_deref()
        .is_some_and(|s| !(1..=4096).contains(&s.chars().count()) || s.contains('\0'))
    {
        return Err(format!("{n}: bad install script"));
    }
    Ok(())
}

/// `/usr/bin/claude` -> `claude`.
pub fn binary_name(binary: &str) -> &str {
    Path::new(binary)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(binary)
}

/// The first line of `text` that contains one of `patterns` (case-insensitive), trimmed.
pub fn match_login(patterns: &[String], text: &str) -> Option<String> {
    let pats: Vec<String> = patterns
        .iter()
        .filter(|p| !p.is_empty())
        .map(|p| p.to_lowercase())
        .collect();
    text.lines()
        .find(|l| {
            let l = l.to_lowercase();
            pats.iter().any(|p| l.contains(p))
        })
        .map(|l| l.chars().filter(|c| !c.is_control()).collect::<String>())
        .map(|l| l.trim().to_owned())
}

/// Last 20 non-empty lines, control characters stripped, joined and cut to the detail limit.
pub fn tail_detail(out: &str) -> String {
    let lines: Vec<String> = out
        .lines()
        .map(|l| l.chars().filter(|c| !c.is_control()).collect::<String>())
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect();
    let joined = lines[lines.len().saturating_sub(20)..].join(" | ");
    let n = joined.chars().count();
    joined.chars().skip(n.saturating_sub(DETAIL_MAX)).collect()
}

fn in_path(bin: &str) -> bool {
    #[cfg(unix)]
    fn exec(m: &std::fs::Metadata) -> bool {
        std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0
    }
    #[cfg(not(unix))]
    fn exec(_: &std::fs::Metadata) -> bool {
        true
    }
    let ok = |p: &Path| p.metadata().is_ok_and(|m| m.is_file() && exec(&m));
    if bin.contains('/') {
        return ok(Path::new(bin));
    }
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| ok(&d.join(bin))))
}

/// Is the agent installed? `binary` is the profile's binary as given in the spec.
pub async fn is_installed(r: &Recipe, binary: &str) -> bool {
    let Some(a) = r.check.as_ref().filter(|a| !a.is_empty()) else {
        return in_path(binary);
    };
    let run = Command::new(&a[0])
        .args(&a[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(
        tokio::time::timeout(Duration::from_secs(15), run).await,
        Ok(Ok(s)) if s.success()
    )
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

async fn install_flow(binary: String, r: Recipe) -> Result<(), String> {
    let name = binary_name(&binary).to_owned();
    let Some(a) = r.install.as_ref().filter(|a| !a.is_empty()) else {
        return Err(format!(
            "{name} is not installed and has no install command (agents.json or the hub profile)"
        ));
    };
    let secs = r.install_timeout_secs;
    let mut cmd = Command::new(&a[0]);
    cmd.args(&a[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Own process group so a timeout can kill the whole install tree (Unix only).
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => format!(
            "cannot install {name}: `{}` was not found in PATH (install it first)",
            a[0]
        ),
        _ => format!("cannot run install command `{}`: {e}", a[0]),
    })?;
    let pid = child.id();
    match tokio::time::timeout(Duration::from_secs(secs), child.wait_with_output()).await {
        Err(_) => {
            kill_group(pid).await;
            Err(format!("installing {name} timed out after {secs}s"))
        }
        Ok(Err(e)) => Err(format!("installing {name} failed: {e}")),
        Ok(Ok(out)) if !out.status.success() => {
            let text = format!(
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            Err(format!(
                "installing {name} failed ({}): {}",
                out.status,
                tail_detail(&text)
            ))
        }
        Ok(Ok(_)) if !is_installed(&r, &binary).await => Err(format!(
            "installing {name} finished but it is still not found"
        )),
        Ok(Ok(_)) => Ok(()),
    }
}

type Flow = Shared<BoxFuture<'static, Result<(), String>>>;

/// Runs install recipes; concurrent requests for one profile share a single run.
#[derive(Default)]
pub struct Installer {
    inflight: Mutex<HashMap<String, Flow>>,
}

impl std::fmt::Debug for Installer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Installer")
    }
}

impl Installer {
    pub async fn ensure(&self, binary: &str, r: &Recipe) -> Result<(), String> {
        let name = binary_name(binary).to_owned();
        let flow = {
            let mut m = self
                .inflight
                .lock()
                .expect("installer lock is not poisoned");
            m.entry(name.clone())
                .or_insert_with(|| install_flow(binary.to_owned(), r.clone()).boxed().shared())
                .clone()
        };
        let res = flow.await;
        // ponytail: a waiter of an old run may drop a newer run's entry; worst case one duplicate install.
        self.inflight
            .lock()
            .expect("installer lock is not poisoned")
            .remove(&name);
        res
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ulakd-agents-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn script(path: &Path, body: &str) {
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn mcp_fields_parse_and_default_to_absent() {
        let a = Agents::from_json(
            r#"{"x":{"mcp":"none","mcp_args":["--m","{mcp_url}"]},"claude":{"mcp":"claude"}}"#,
        )
        .unwrap();
        assert_eq!(a.recipe("x").mcp, Some(Mcp::None));
        assert_eq!(a.recipe("x").mcp_args, ["--m", "{mcp_url}"]);
        assert_eq!(a.recipe("claude").mcp, Some(Mcp::Claude));
        assert_eq!(Agents::default().recipe("claude").mcp, None);
        assert!(!serde_json::to_string(&Agents::default())
            .unwrap()
            .contains("mcp"));
    }

    #[test]
    fn defaults_and_parsing() {
        let a = Agents::default();
        let c = a.recipe("claude");
        assert_eq!(c.install.as_ref().unwrap()[0], "npm");
        assert_eq!(c.install_timeout_secs, 600);
        assert_eq!(c.login_scan_secs, 300);
        assert!(a.recipe("antigravity").install.is_none());
        assert_eq!(a.recipe("/opt/bin/claude"), c);
        assert_eq!(a.recipe("whatever"), Recipe::default());
        let f = Agents::from_json(
            r#"{"claude":{"login_patterns":["x"]},"foo":{"install":["a","b"],"install_timeout_secs":5}}"#,
        )
        .unwrap();
        assert_eq!(f.recipe("claude").login_patterns, ["x"]);
        assert!(f.recipe("claude").install.is_none()); // file entry replaces the default
        assert_eq!(f.recipe("foo").install_timeout_secs, 5);
        assert_eq!(f.recipe("antigravity"), Recipe::default()); // defaults stay
        assert!(Agents::from_json(r#"{"a":{"bogus":1}}"#).is_err());
        assert!(Agents::from_json("nope").is_err());
    }

    pub(crate) const TWO_CLAUDES: &str = r#"{"claude-work":{"binary":"claude","config_dir":"~/.claude-work"},"claude-personal":{"binary":"claude","config_dir":"~/.claude-personal","args":["--x"],"env":{"A":"1"}}}"#;

    pub(crate) fn spec_for(binary: &str, cfg: Option<&str>, env: &[(&str, &str)]) -> SpawnSpec {
        SpawnSpec::new(
            ulak_proto::AgentId::new("a1").unwrap(),
            ulak_proto::AgentProfile::new(binary, vec!["--y".into()], cfg.map(Into::into)).unwrap(),
            "/w".into(),
            "go".into(),
            env.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn named_profiles_resolve_to_their_binary_and_inherit_the_rest() {
        let a = Agents::from_json(TWO_CLAUDES).unwrap();
        let home = std::env::var("HOME").unwrap();
        let w = a.exec_spec(&spec_for("claude-work", None, &[]));
        assert_eq!(w.binary, "claude");
        assert_eq!(w.args, ["--y"]);
        assert_eq!(
            w.config_dir.as_deref(),
            Some(format!("{home}/.claude-work").as_str())
        );
        assert_eq!(w.env["CLAUDE_CONFIG_DIR"], format!("{home}/.claude-work"));
        // check, install and login patterns come from `claude`; mcp follows the real binary
        let base = Agents::default().recipe("claude");
        assert_eq!(w.recipe.check, base.check);
        assert_eq!(w.recipe.install, base.install);
        assert_eq!(w.recipe.login_patterns, base.login_patterns);
        assert_eq!(w.recipe.mcp, Some(Mcp::Claude));
        assert_eq!(a.recipe("claude-work").login_patterns, base.login_patterns);
        let p = a.exec_spec(&spec_for("claude-personal", None, &[("A", "mine")]));
        assert_eq!(p.args, ["--x", "--y"]);
        assert_eq!(p.env["A"], "1"); // lower priority: the spec env still wins in the argv
        assert_eq!(
            p.env["CLAUDE_CONFIG_DIR"],
            format!("{home}/.claude-personal")
        );
        // the spawn's own config_dir wins
        let o = a.exec_spec(&spec_for("claude-work", Some("/own"), &[]));
        assert_eq!(o.env["CLAUDE_CONFIG_DIR"], "/own");
        // no entry: the spawn as given
        let plain = spec_for("claude", None, &[]);
        assert_eq!(a.exec_spec(&plain).binary, "claude");
        assert!(a.exec_spec(&plain).env.is_empty());
        // a config_dir on a non-claude binary sets no CLAUDE_CONFIG_DIR
        let x = Agents::from_json(r#"{"g":{"binary":"gemini","config_dir":"/c"}}"#).unwrap();
        let e = x.exec_spec(&spec_for("g", None, &[]));
        assert!(
            e.env.is_empty() && e.config_dir.as_deref() == Some("/c") && e.recipe.mcp.is_none()
        );
        // the blocker keys differ, so the two accounts block independently
        let k = |b: &str| spec_for(b, None, &[]).profile().key().unwrap();
        assert_ne!(k("claude-work"), k("claude-personal"));
        assert_eq!(a.names().filter(|n| n.starts_with("claude")).count(), 3);
        assert!(serde_json::to_string(&Agents::default())
            .unwrap()
            .find("binary")
            .is_none());
    }

    pub(crate) fn hub(name: &str, binary: &str, cfg: Option<&str>) -> HubProfile {
        HubProfile {
            name: name.into(),
            binary: binary.into(),
            args: vec![],
            config_dir: cfg.map(Into::into),
            env: BTreeMap::new(),
            install: None,
        }
    }

    #[test]
    fn hub_install_script_becomes_sh_c_and_local_recipe_stays_otherwise() {
        let local = Agents::from_json(
            r#"{"tool":{"install":["local-installer"]},"other":{"install":["other-installer"]}}"#,
        )
        .unwrap();
        let (a, _) = local.with_hub(&[
            HubProfile {
                install: Some("curl -fsSL https://x/i.sh | bash".into()),
                ..hub("tool", "/opt/tool", None)
            },
            hub("other", "/opt/other", None),
            hub("via-tool", "tool", None),
            HubProfile {
                install: Some("a\0b".into()),
                ..hub("nul", "x", None)
            },
        ]);
        assert_eq!(
            a.recipe("tool").install,
            Some(vec![
                "sh".into(),
                "-c".into(),
                "curl -fsSL https://x/i.sh | bash".into()
            ])
        );
        // no hub script: a local name keeps its own recipe
        assert_eq!(
            a.recipe("other").install,
            Some(vec!["other-installer".to_owned()])
        );
        // a new name inherits from its binary's entry, here the hub-scripted `tool`
        assert_eq!(a.recipe("via-tool").install, a.recipe("tool").install);
        assert!(
            !a.names().any(|n| n == "nul"),
            "a NUL script drops the profile"
        );
    }

    #[test]
    fn hub_profiles_overlay_but_never_bring_commands() {
        let local = Agents::from_json(
            r#"{"tool":{"install":["local-installer"],"login_patterns":["log in"],"binary":"tool-bin","args":["--old"]}}"#,
        )
        .unwrap();
        let base = Agents::default().recipe("claude");
        let over = HubProfile {
            args: vec!["--new".into()],
            env: BTreeMap::from([("A".to_owned(), "1".to_owned())]),
            ..hub("tool", "/opt/tool", Some("~/.tool"))
        };
        let (a, names) = local.with_hub(&[
            hub("claude-work", "claude", Some("~/.claude-work")),
            over,
            hub("mystery", "unknown-bin", None),
            hub("bad/name", "claude", None),
            HubProfile {
                env: BTreeMap::from([("ULAK_X".to_owned(), "v".to_owned())]),
                ..hub("sneaky", "claude", None)
            },
        ]);
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            ["claude-work", "mystery", "tool"]
        );
        // a new name inherits install and login from its binary's local entry
        let w = a.recipe("claude-work");
        assert_eq!(w.install, base.install);
        assert_eq!(w.login_patterns, base.login_patterns);
        assert_eq!(w.mcp, Some(Mcp::Claude));
        // an override keeps the local commands, takes the hub's binary/args/config_dir/env
        let t = a.recipe("tool");
        assert_eq!(t.install.unwrap(), ["local-installer"]);
        assert_eq!(t.login_patterns, ["log in"]);
        assert_eq!(t.binary.as_deref(), Some("/opt/tool"));
        assert_eq!(t.args, ["--new"]);
        assert_eq!(t.config_dir.as_deref(), Some("~/.tool"));
        assert_eq!(t.env["A"], "1");
        // an unknown binary gets no commands
        let m = a.recipe("mystery");
        assert!(m.install.is_none() && m.check.is_none());
        assert!(!a.names().any(|n| n == "sneaky" || n == "bad/name"));
        // an empty hub list is the local set
        assert_eq!(local.with_hub(&[]).0, local);
    }

    #[test]
    fn hub_profile_limits() {
        let ok = hub("a.b_c-1", "claude", None);
        assert!(check_hub_profile(&ok).is_ok());
        let bad = [
            hub("", "claude", None),
            hub(&"x".repeat(65), "claude", None),
            hub("..", "claude", None),
            hub("a", "", None),
            hub("a", "cl\nx", None),
            hub("a", &"x".repeat(257), None),
            hub("a", "x", Some("/c\u{7}")),
            HubProfile {
                args: vec!["a".into(); 33],
                ..ok.clone()
            },
            HubProfile {
                args: vec![String::new()],
                ..ok.clone()
            },
            HubProfile {
                env: BTreeMap::from([("1A".into(), "v".into())]),
                ..ok.clone()
            },
        ];
        for b in &bad {
            assert!(check_hub_profile(b).is_err(), "{b:?}");
        }
    }

    #[test]
    fn first_run_writes_defaults() {
        let d = tmp("init").join("sub");
        let a = Agents::load_or_init(&d).unwrap();
        assert_eq!(a, Agents::default());
        let again = Agents::load_or_init(&d).unwrap();
        assert_eq!(again, a);
        assert!(d.join("agents.json").exists());
    }

    #[test]
    fn login_matching() {
        let p = vec!["please run /login".to_owned(), "NOT LOGGED IN".to_owned()];
        let txt = "welcome\n  \x1b[31mPlease Run /Login\x1b[0m to go on\nbye";
        assert_eq!(
            match_login(&p, txt).as_deref(),
            Some("[31mPlease Run /Login[0m to go on")
        );
        assert_eq!(
            match_login(&p, "you are Not Logged In").unwrap(),
            "you are Not Logged In"
        );
        assert!(match_login(&p, "all fine").is_none());
        assert!(match_login(&[String::new()], "all fine").is_none());
    }

    #[test]
    fn tail_is_bounded_and_clean() {
        let text: String = (0..50).map(|i| format!("line{i}\x07\n")).collect();
        let t = tail_detail(&text);
        assert!(t.starts_with("line30") && t.ends_with("line49") && !t.contains('\x07'));
        assert!(tail_detail(&"x".repeat(5000)).chars().count() <= DETAIL_MAX);
    }

    fn recipe_for(dir: &Path, install_body: &str, secs: u64) -> (Recipe, String) {
        let bin = dir.join("fake-agent");
        let inst = dir.join("install.sh");
        script(
            &inst,
            &format!("echo run >> {d}/runs\n{install_body}", d = dir.display()),
        );
        (
            Recipe {
                check: Some(vec![bin.display().to_string()]),
                // via sh: exec'ing a just-written file can hit ETXTBSY while another thread forks
                install: Some(vec!["sh".into(), inst.display().to_string()]),
                install_timeout_secs: secs,
                ..Recipe::default()
            },
            bin.display().to_string(),
        )
    }

    #[tokio::test]
    async fn install_success_and_dedupe() {
        let d = tmp("ok");
        let body = format!(
            "sleep 1\nprintf '#!/bin/sh\\nexit 0\\n' > {b}\nchmod +x {b}",
            b = d.join("fake-agent").display()
        );
        let (r, bin) = recipe_for(&d, &body, 30);
        assert!(!is_installed(&r, &bin).await);
        let inst = Arc::new(Installer::default());
        let rs = futures_util::future::join_all((0..4).map(|_| {
            let (i, r, b) = (Arc::clone(&inst), r.clone(), bin.clone());
            tokio::spawn(async move { i.ensure(&b, &r).await })
        }))
        .await;
        assert!(rs.into_iter().all(|x| x.unwrap().is_ok()));
        assert_eq!(
            std::fs::read_to_string(d.join("runs"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert!(is_installed(&r, &bin).await);
    }

    #[tokio::test]
    async fn install_failure_timeout_and_missing_recipe() {
        let d = tmp("bad");
        let (r, bin) = recipe_for(&d, "echo boom >&2\nexit 3", 30);
        let e = Installer::default().ensure(&bin, &r).await.unwrap_err();
        assert!(e.contains("failed") && e.contains("boom"), "{e}");

        let (r, bin) = recipe_for(&d, "sleep 30", 1);
        let t = std::time::Instant::now();
        let e = Installer::default().ensure(&bin, &r).await.unwrap_err();
        assert!(e.contains("timed out"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(10));

        // exits 0 but the check still fails
        let (r, bin) = recipe_for(&d, "exit 0", 30);
        let e = Installer::default().ensure(&bin, &r).await.unwrap_err();
        assert!(e.contains("still not found"), "{e}");

        let none = Recipe::default();
        let e = Installer::default()
            .ensure("no-such-agent-x", &none)
            .await
            .unwrap_err();
        assert!(e.contains("no install command"), "{e}");

        let npmless = Recipe {
            install: argv(&["no-such-installer-x", "a"]),
            ..Recipe::default()
        };
        let e = Installer::default()
            .ensure("no-such-agent-x", &npmless)
            .await
            .unwrap_err();
        assert!(
            e.contains("no-such-installer-x") && e.contains("not found in PATH"),
            "{e}"
        );
    }
}
