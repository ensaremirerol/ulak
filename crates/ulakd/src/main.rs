use std::{
    io::{BufRead, Write},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use ulakd::{
    agents::Agents,
    config::{default_dir, Identity},
    hub::{run_forever_in, SharedProbe},
    join::{hostname, join},
    telemetry::SystemProbe,
    tmux::Tmux,
    update::{
        config::{in_container, UpdateConfig},
        install, now_ms, trust, version_info, Outcome, Updater, RESTART_EXIT,
    },
};

const USAGE: &str =
    "usage: ulakd join <https://hub[:port]> [--force] | ulakd run | ulakd update --check|--apply | ulakd --version";

fn prompt(q: &str) -> std::io::Result<String> {
    print!("{q}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

/// Non-interactive join: `ULAKD_FINGERPRINT` and `ULAKD_OTP` answer the two prompts; others read stdin.
fn answer(q: &str) -> std::io::Result<String> {
    let var = if q.contains("fingerprint") {
        "ULAKD_FINGERPRINT"
    } else {
        "ULAKD_OTP"
    };
    match std::env::var(var) {
        Ok(v) => Ok(v),
        Err(_) => prompt(q),
    }
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

fn updater(dir: &Path) -> Result<Arc<Updater>, Box<dyn std::error::Error>> {
    let container = in_container(&env);
    let cfg = UpdateConfig::load_or_init(dir, container)?;
    let (auto, token) = (cfg.auto(container, &env), cfg.token(&env)?);
    let exe = std::env::current_exe()?;
    Ok(Arc::new(Updater::new(
        cfg,
        auto,
        token,
        version_info(),
        exe,
        dir.to_owned(),
        trust::release_key(),
    )))
}

/// Clone settings: work root `$ULAKD_WORK_DIR` (default `<config>/work`), timeout
/// `$ULAKD_CLONE_TIMEOUT_SECS` (default 300).
fn git_cfg(dir: &Path) -> ulakd::git::GitCfg {
    let root = std::env::var_os("ULAKD_WORK_DIR").map_or_else(|| dir.join("work"), Into::into);
    let mut c = ulakd::git::GitCfg::new(root, dir.join("run"));
    if let Some(s) = std::env::var("ULAKD_CLONE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
    {
        c = c.with_timeout(Duration::from_secs(s));
    }
    // Only the test-key build reads this; the default build has no such knob.
    #[cfg(feature = "test-key")]
    if let Ok(b) = std::env::var("ULAKD_TEST_GIT_BASE") {
        c = c.with_test_host(&b);
    }
    c
}

/// `ulakd update --check|--apply`: one-shot, prints the outcome.
async fn update_cli(dir: &Path, apply: bool) -> Result<(), Box<dyn std::error::Error>> {
    let u = updater(dir)?;
    match u
        .run(apply, false)
        .await
        .expect("nothing else runs in the CLI")?
    {
        Outcome::UpToDate => println!("ulakd {} is up to date", u.info().version()),
        Outcome::Available(v) => println!("update available: {v} (run `ulakd update --apply`)"),
        Outcome::Installed(v) => {
            println!("installed {v}; restart ulakd to use it (the previous binary is ulakd.prev)")
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = default_dir();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["join", url, ..] => {
            let force = args.iter().any(|a| a == "--force");
            ulakd::join::check_overwrite(&dir, force, "ULAKD_FORCE")?;
            let id = join(url, &dir, &mut answer).await?;
            println!("enrolled as {} (files in {})", id.as_str(), dir.display());
            Ok(())
        }
        ["--version"] | ["-V"] | ["version"] => {
            let v = version_info();
            let key = if trust::release_key().is_ok() {
                "configured"
            } else {
                "NOT configured (updates disabled)"
            };
            println!(
                "ulakd {} (git {}, proto {}, {} {})\nrelease key: {key}",
                v.version(),
                v.git(),
                v.proto(),
                v.os(),
                v.arch()
            );
            Ok(())
        }
        ["update", flag @ ("--check" | "--apply")] => {
            if let Err(e) = update_cli(&dir, flag == "--apply").await {
                eprintln!("ulakd update: {e}");
                std::process::exit(1);
            }
            Ok(())
        }
        ["run"] => {
            let exe = std::env::current_exe()?;
            match install::startup(&dir, &exe, now_ms()) {
                Ok(install::Startup::RolledBack { bad }) => {
                    eprintln!("ulakd: version {bad} died right after its update; restored ulakd.prev, restarting");
                    std::process::exit(RESTART_EXIT);
                }
                Ok(install::Startup::Probation) => {
                    let d = dir.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(install::CONFIRM_AFTER_SECS)).await;
                        install::confirm(&d);
                    });
                }
                Ok(install::Startup::Normal) => {}
                Err(e) => eprintln!("ulakd: update marker: {e}"),
            }
            let updates = match updater(&dir) {
                Ok(u) => {
                    tokio::spawn(Arc::clone(&u).auto_loop());
                    Some(u)
                }
                Err(e) => {
                    eprintln!("ulakd: self-update disabled: {e}");
                    None
                }
            };
            let id = Identity::load(&dir)?;
            let probe: SharedProbe =
                Arc::new(Mutex::new(Box::new(SystemProbe::new(id.guard.clone()))));
            let agents = Agents::load_or_init(&dir)?;
            let host = Tmux::with_agents(std::env::var("ULAKD_TMUX_SOCKET").ok(), agents)
                .with_mcp(dir.join("run"), dir.join("ca.pem"))
                .with_git(git_cfg(&dir));
            host.adopt().await?; // agents of a previous run keep being watched for login prompts
            tokio::select! {
                r = run_forever_in(&id, Some(&dir), hostname(), Duration::from_secs(5), &host, probe, updates) => { r?; }
                _ = tokio::signal::ctrl_c() => {}
            }
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_git_base_only_under_test_key() {
        std::env::set_var("ULAKD_TEST_GIT_BASE", "file:///x");
        let host = super::git_cfg(std::path::Path::new("/nonexistent"))
            .host()
            .to_owned();
        assert_eq!(host == "file:///x", cfg!(feature = "test-key"));
        if !cfg!(feature = "test-key") {
            assert_eq!(host, "https://github.com");
        }
    }
}
