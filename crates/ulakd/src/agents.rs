//! `agents.json`: per-profile (binary file name) check, install recipe and login patterns.
//! Install recipes come ONLY from this local file (and the built-in defaults); the hub never
//! supplies a command. Recipes are argv vectors, run without a shell.
use crate::errors::ConfigError;
use futures_util::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    process::Stdio,
    sync::Mutex,
    time::Duration,
};
use tokio::process::Command;

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

    /// The recipe for a binary (file name); unknown profiles get check-only defaults.
    pub fn recipe(&self, binary: &str) -> Recipe {
        self.0.get(binary_name(binary)).cloned().unwrap_or_default()
    }
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
            "{name} is not installed and no install recipe is configured (agents.json)"
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
mod tests {
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
        assert!(e.contains("no install recipe"), "{e}");

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
