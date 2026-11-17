//! Repo spawns: credential plumbing against a stub `git`, a real clone from a local bare repo
//! (file:// base, test-key only), and a real tmux agent started in the clone.
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc};
use tokio_tungstenite::{accept_async, tungstenite::Message};
use ulak_proto::*;
use ulakd::{
    errors::{AgentError, TelemetryError},
    git::{clone_repo, GitCfg},
    hub::{connect, run_session, AgentHost, SessionCfg, SharedProbe},
    telemetry::{Probe, Probed},
    tmux::Tmux,
};

const TOKEN: &str = "ghs_TOPSECRETTOKEN99";

fn tok() -> GitToken {
    GitToken::new(TOKEN).unwrap()
}
fn aid(s: &str) -> AgentId {
    AgentId::new(s).unwrap()
}
fn repo() -> RepoRef {
    RepoRef::new("octo/hello").unwrap()
}
fn stub(dir: &Path, body: &str) -> String {
    let p = dir.join("git-stub");
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p.to_string_lossy().into_owned()
}
fn cfg(t: &Path, program: &str) -> GitCfg {
    GitCfg::new(t.join("work"), t.join("run")).with_test_program(program)
}
fn leftovers(root: &Path) -> Vec<String> {
    std::fs::read_dir(root)
        .map(|r| {
            r.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn stub_git_sees_token_only_in_env() {
    let t = tempfile::tempdir().unwrap();
    let rec = t.path().join("rec");
    std::fs::create_dir(&rec).unwrap();
    let body = format!(
        "R={}\nprintf '%s\\n' \"$@\" >> $R/argv\ncase \"$*\" in *clone*) env > $R/env; ls -l \"$GIT_ASKPASS\" > $R/mode; \"$GIT_ASKPASS\" \"Password for 'x': \" > $R/pw; for a; do d=$a; done; mkdir -p \"$d\";; esac",
        rec.display()
    );
    let c = cfg(t.path(), &stub(t.path(), &body));
    let b = BranchName::new("dev").unwrap();
    let dir = clone_repo(&c, &aid("a1"), &repo(), Some(&b), &tok(), false)
        .await
        .unwrap();
    assert_eq!(dir, t.path().join("work/a1/hello"));
    let argv = std::fs::read_to_string(rec.join("argv")).unwrap();
    assert!(!argv.contains(TOKEN) && !argv.contains("x-access-token"));
    assert!(argv.contains("--depth\n1") && argv.contains("--branch\ndev"));
    assert!(argv.contains("https://github.com/octo/hello.git\n"));
    assert!(argv.contains("remote.origin.pushurl\nno-push-here"));
    let env = std::fs::read_to_string(rec.join("env")).unwrap();
    for want in [
        &format!("ULAK_GIT_TOKEN={TOKEN}"),
        "GIT_TERMINAL_PROMPT=0",
        "GIT_CONFIG_NOSYSTEM=1",
        "GCM_INTERACTIVE=never",
    ] {
        assert!(env.contains(want), "{want}");
    }
    assert!(!env.contains("SSH_ASKPASS"));
    assert!(std::fs::read_to_string(rec.join("mode"))
        .unwrap()
        .starts_with("-rwx------"));
    assert_eq!(
        std::fs::read_to_string(rec.join("pw")).unwrap().trim(),
        TOKEN
    );
    assert_eq!(
        leftovers(&t.path().join("work")),
        ["a1"],
        "askpass dir removed"
    );

    // allow_push: no pushurl guard
    std::fs::remove_file(rec.join("argv")).unwrap();
    clone_repo(&c, &aid("a2"), &repo(), None, &tok(), true)
        .await
        .unwrap();
    assert!(!std::fs::read_to_string(rec.join("argv"))
        .unwrap()
        .contains("pushurl"));
}

#[tokio::test]
async fn failures_timeout_and_nonempty_dir() {
    let t = tempfile::tempdir().unwrap();
    let fail = stub(
        t.path(),
        "echo \"fatal: Authentication failed for 'https://github.com/o/r.git/' ghs_TOPSECRETTOKEN99\" >&2; exit 128",
    );
    let c = cfg(t.path(), &fail);
    let e = clone_repo(&c, &aid("f1"), &repo(), None, &tok(), false)
        .await
        .unwrap_err();
    let m = e.to_string();
    assert!(
        m.contains("authentication failed") && !m.contains(TOKEN),
        "{m}"
    );
    assert!(leftovers(&t.path().join("work")).is_empty(), "cleaned up");

    let slow = stub(t.path(), "sleep 30");
    let c2 = cfg(t.path(), &slow).with_timeout(Duration::from_millis(300));
    let t0 = std::time::Instant::now();
    let e = clone_repo(&c2, &aid("f2"), &repo(), None, &tok(), false)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("timed out"));
    assert!(t0.elapsed() < Duration::from_secs(5));
    assert!(leftovers(&t.path().join("work")).is_empty());

    let busy = t.path().join("work/f3/hello");
    std::fs::create_dir_all(&busy).unwrap();
    std::fs::write(busy.join("keep"), "x").unwrap();
    let e = clone_repo(&c, &aid("f3"), &repo(), None, &tok(), false)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("not empty"));
    assert!(busy.join("keep").exists(), "existing data untouched");
}

fn git(dir: &Path, args: &[&str]) {
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
}

/// `<base>/octo/hello.git` (bare, branch `main` plus `dev`); returns file:// base.
fn bare_repo(t: &Path) -> String {
    let src = t.join("src");
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    std::fs::write(src.join("README"), "hi").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-q", "-m", "one"]);
    git(&src, &["branch", "dev"]);
    let bare = t.join("srv/octo/hello.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    git(
        t,
        &[
            "clone",
            "-q",
            "--bare",
            src.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    format!("file://{}", t.join("srv").display())
}

#[tokio::test]
async fn real_clone_from_local_bare_repo() {
    let t = tempfile::tempdir().unwrap();
    let base = bare_repo(t.path());
    let c = GitCfg::new(t.path().join("work"), t.path().join("run")).with_test_host(&base);
    let b = BranchName::new("dev").unwrap();
    let dir = clone_repo(&c, &aid("r1"), &repo(), Some(&b), &tok(), false)
        .await
        .unwrap();
    assert!(dir.join("README").exists());
    let conf = std::fs::read_to_string(dir.join(".git/config")).unwrap();
    assert!(
        !conf.contains(TOKEN) && conf.contains("no-push-here"),
        "{conf}"
    );
    assert!(dir.join(".git/shallow").exists(), "shallow clone");
    let b = BranchName::new("nope").unwrap();
    let e = clone_repo(&c, &aid("r2"), &repo(), Some(&b), &tok(), false)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("branch not found"), "{e}");
    let other = RepoRef::new("octo/missing").unwrap();
    let e = clone_repo(&c, &aid("r3"), &other, None, &tok(), false)
        .await
        .unwrap_err();
    assert!(!e.to_string().contains(TOKEN));
}

fn have_tmux() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success())
}

async fn screen(t: &Tmux, id: &AgentId, needle: &str) -> bool {
    for _ in 0..40 {
        let mut rx = t.attach_agent(id).await.unwrap();
        if let Some(f) = rx.recv().await {
            if String::from_utf8_lossy(&f).contains(needle) {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

fn repo_spec(id: &str, allow_push: bool) -> SpawnSpec {
    SpawnSpec::new_repo(
        aid(id),
        AgentProfile::new(
            "sh",
            vec![
                "-c".into(),
                "echo pwd=$(pwd) tok=[$ULAK_GIT_TOKEN] ask=[$GIT_ASKPASS] file=[$(cat \"${GIT_ASKPASS%/*}/token\" 2>/dev/null)]; exec cat".into(),
            ],
            None,
        )
        .unwrap(),
        Source::Repo {
            repo: repo(),
            branch: None,
            credential: CredentialId::new("c1").unwrap(),
            allow_push,
        },
        "go".into(),
        BTreeMap::new(),
    )
    .unwrap()
}

#[tokio::test]
async fn tmux_repo_agent_starts_in_clone_and_cleans_up() {
    if !have_tmux() {
        eprintln!("tmux not installed; skipping");
        return;
    }
    let t = tempfile::tempdir().unwrap();
    let base = bare_repo(t.path());
    let sock = format!("ulakd-test-repo-{}", std::process::id());
    let tm = Tmux::new(Some(sock.clone()))
        .with_delays(Duration::from_millis(200), Duration::from_millis(50))
        .with_git(GitCfg::new(t.path().join("work"), t.path().join("run")).with_test_host(&base));

    // no GitAccess: fails with a clear reason
    let s = repo_spec("p1", false);
    let e = tm.spawn_agent(&s).await.unwrap_err();
    assert!(
        matches!(&e, AgentError::Git(m) if m.contains("no git access")),
        "{e}"
    );

    // allow_push false: no token in the agent, no helper
    tm.git_access(aid("p1"), tok());
    tm.spawn_agent(&s).await.unwrap();
    let clone = t.path().join("work/p1/hello");
    assert!(screen(&tm, s.agent_id(), &format!("pwd={}", clone.display())).await);
    assert!(screen(&tm, s.agent_id(), "tok=[] ask=[] file=[]").await);
    let conf = std::fs::read_to_string(clone.join(".git/config")).unwrap();
    assert!(!conf.contains(TOKEN));
    assert!(!t.path().join("run/git-p1").exists());
    // single use: a second spawn of the same agent without new access fails
    assert!(tm.spawn_agent(&repo_spec("p1", false)).await.is_err());
    tm.kill_agent(s.agent_id()).await.unwrap();

    // allow_push true: helper and token file in the private run dir
    let s2 = repo_spec("p2", true);
    tm.git_access(aid("p2"), tok());
    tm.spawn_agent(&s2).await.unwrap();
    let h = t.path().join("run/git-p2");
    assert_eq!(
        std::fs::metadata(&h).unwrap().permissions().mode() & 0o777,
        0o700
    );
    // the token is in the 0600 file only, never in the agent's environment
    assert!(screen(&tm, s2.agent_id(), "tok=[] ask=[/").await);
    assert!(screen(&tm, s2.agent_id(), &format!("file=[{TOKEN}]")).await);
    assert_eq!(
        std::fs::metadata(h.join("token"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let conf = std::fs::read_to_string(t.path().join("work/p2/hello/.git/config")).unwrap();
    assert!(!conf.contains(TOKEN) && !conf.contains("no-push-here"));
    tm.kill_agent(s2.agent_id()).await.unwrap();
    assert!(!h.exists(), "helper removed after kill");
    assert!(leftovers(&t.path().join("work"))
        .iter()
        .all(|n| !n.starts_with('.')));
    let _ = Command::new("tmux")
        .args(["-L", &sock, "kill-server"])
        .output();
}

struct Rec {
    seen: Mutex<Vec<String>>,
}
impl AgentHost for Rec {
    fn git_access(&self, agent: AgentId, token: GitToken) {
        assert_eq!(token.expose(), TOKEN);
        self.seen
            .lock()
            .unwrap()
            .push(format!("access:{}", agent.as_str()));
    }
    async fn spawn(&self, s: &SpawnSpec) -> Result<(), AgentError> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("spawn:{}", s.agent_id().as_str()));
        Err(AgentError::Git("boom".into()))
    }
    async fn kill(&self, _: &AgentId) -> Result<(), AgentError> {
        Ok(())
    }
    async fn nudge(&self, _: &AgentId, _: &str) -> Result<(), AgentError> {
        Ok(())
    }
    async fn input(&self, _: &AgentId, _: &[u8]) -> Result<(), AgentError> {
        Ok(())
    }
    async fn list(&self) -> Result<Vec<AgentId>, AgentError> {
        Ok(vec![])
    }
    async fn attach(&self, _: &AgentId) -> Result<mpsc::Receiver<Vec<u8>>, AgentError> {
        Ok(mpsc::channel(1).1)
    }
}
struct P;
impl Probe for P {
    fn sample(&mut self) -> Result<Probed, TelemetryError> {
        Ok(Probed {
            telemetry: Telemetry::new(0.1, 5.0, None, 20.0, 1)?,
            state: NodeState::Idle,
        })
    }
}

#[tokio::test]
async fn session_delivers_git_access_before_spawn_and_reports_failed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/ws/daemon", listener.local_addr().unwrap());
    let host = Arc::new(Rec {
        seen: Mutex::default(),
    });
    let probe: SharedProbe = Arc::new(Mutex::new(Box::new(P)));
    let cfg = SessionCfg {
        node_id: NodeId::new("n1").unwrap(),
        hostname: "box".into(),
        heartbeat: Duration::from_millis(50),
    };
    let h = Arc::clone(&host);
    tokio::spawn(async move {
        let ws = connect(&url, None).await.unwrap();
        run_session(ws, &cfg, &*h, &probe).await
    });
    let (tcp, _) = listener.accept().await.unwrap();
    let mut hub = accept_async(tcp).await.unwrap();
    let send = |m: HubToDaemon| Message::text(encode(&m).unwrap());
    hub.send(send(HubToDaemon::GitAccess {
        agent: aid("g1"),
        token: tok(),
    }))
    .await
    .unwrap();
    hub.send(send(HubToDaemon::Spawn(repo_spec("g1", false))))
        .await
        .unwrap();
    let mut got = vec![];
    let mut reasons = vec![];
    while !got.contains(&AgentStatus::Failed) {
        let m = tokio::time::timeout(Duration::from_secs(5), hub.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(t) = m {
            match decode(t.as_str()).unwrap() {
                DaemonToHub::AgentStatus { status, .. } => got.push(status),
                DaemonToHub::AgentFailed { agent, reason } => reasons.push((agent, reason)),
                _ => {}
            }
        }
    }
    assert_eq!(got, [AgentStatus::Starting, AgentStatus::Failed]);
    assert_eq!(
        reasons,
        [(aid("g1"), "repository setup failed: boom".to_owned())],
        "the reason precedes the Failed status"
    );
    assert_eq!(*host.seen.lock().unwrap(), ["access:g1", "spawn:g1"]);
}
