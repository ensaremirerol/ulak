//! allow_push: the token lives in a 0600 file, never in the argv or environment handed to tmux.
//! Own test binary: it prepends a stub `tmux` to PATH, which must not leak into other tests.
#![cfg(unix)]
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::Path};
use ulak_proto::*;
use ulakd::{git::GitCfg, hub::AgentHost, tmux::Tmux};

const TOKEN: &str = "ghs_TOPSECRETTOKEN99";

fn script(p: &Path, body: &str) {
    std::fs::write(p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn spec(id: &str) -> SpawnSpec {
    SpawnSpec::new_repo(
        AgentId::new(id).unwrap(),
        AgentProfile::new("sh", vec![], None).unwrap(),
        Source::Repo {
            repo: RepoRef::new("octo/hello").unwrap(),
            branch: None,
            credential: CredentialId::new("c1").unwrap(),
            allow_push: true,
            fresh: false,
        },
        "go".into(),
        BTreeMap::new(),
    )
    .unwrap()
}

#[tokio::test]
async fn token_is_not_in_tmux_argv_or_env_and_the_file_is_cleaned_up() {
    let t = tempfile::tempdir().unwrap();
    let bin = t.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let rec = t.path().join("rec");
    std::fs::create_dir(&rec).unwrap();
    script(
        &bin.join("tmux"),
        &format!(
            "R={}\nprintf '%s\\n' \"$@\" >> $R/argv\nenv >> $R/env\ncase \"$*\" in *new-session*) [ -e $R/fail ] && {{ echo boom >&2; exit 1; }};; esac\nexit 0",
            rec.display()
        ),
    );
    script(
        &bin.join("git"),
        "case \"$*\" in *clone*) for a; do d=$a; done; mkdir -p \"$d\";; esac",
    );
    let old = std::env::var("PATH").unwrap_or_default();
    // SAFETY-free: the only test in this binary, so nothing races on the environment.
    std::env::set_var("PATH", format!("{}:{old}", bin.display()));

    let tm = Tmux::new(Some("stub".into()))
        .with_delays(
            std::time::Duration::from_millis(1),
            std::time::Duration::from_millis(1),
        )
        .with_git(GitCfg::new(t.path().join("work"), t.path().join("run")));
    let id = AgentId::new("p1").unwrap();
    let dir = t.path().join("run/git-p1");

    tm.git_access(id.clone(), GitToken::new(TOKEN).unwrap());
    tm.spawn_agent(&spec("p1")).await.unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    assert_eq!(mode(&dir.join("token")), 0o600);
    assert_eq!(std::fs::read_to_string(dir.join("token")).unwrap(), TOKEN);
    let argv = std::fs::read_to_string(rec.join("argv")).unwrap();
    assert!(argv.contains("new-session") && argv.contains("GIT_ASKPASS="));
    assert!(!argv.contains(TOKEN) && !argv.contains("ULAK_GIT_TOKEN"));
    assert!(!std::fs::read_to_string(rec.join("env"))
        .unwrap()
        .contains(TOKEN));
    tm.kill_agent(&id).await.unwrap();
    assert!(!dir.exists(), "removed on kill");

    // a failing new-session leaves no token file either
    std::fs::write(rec.join("fail"), "").unwrap();
    let id2 = AgentId::new("p2").unwrap();
    tm.git_access(id2.clone(), GitToken::new(TOKEN).unwrap());
    assert!(tm.spawn_agent(&spec("p2")).await.is_err());
    assert!(!t.path().join("run/git-p2").exists());
    std::fs::remove_file(rec.join("fail")).unwrap();

    // the startup sweep removes the token of an agent that is gone
    std::fs::create_dir_all(t.path().join("run/git-gone")).unwrap();
    std::fs::write(t.path().join("run/git-gone/token"), TOKEN).unwrap();
    tm.adopt().await.unwrap(); // stub list-sessions prints nothing: nobody is alive
    assert!(!t.path().join("run/git-gone").exists());
    assert!(!std::fs::read_to_string(rec.join("argv"))
        .unwrap()
        .contains(TOKEN));
}
