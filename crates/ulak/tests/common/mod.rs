//! Shared by the real-binary tests: build and start the hub, ask its admin console for codes.
#![allow(dead_code)]
use std::{
    path::{Path, PathBuf},
    process::{Command as Std, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

pub fn bin(pkg: &str, name: &str) -> PathBuf {
    let ok = Std::new(std::env::var("CARGO").unwrap_or("cargo".into()))
        .args(["build", "-p", pkg, "--bin", name])
        .status()
        .unwrap()
        .success();
    assert!(ok, "building {name} failed");
    // target/debug/deps/real_hub-xxxx -> target/debug/<name>
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(name)
}

pub struct Hub {
    pub _child: Child,
    pub stdin: ChildStdin,
    pub out: tokio::io::Lines<BufReader<ChildStdout>>,
    pub url: String,
}

impl Hub {
    pub async fn line(&mut self, prefix: &str) -> String {
        loop {
            let l = timeout(Duration::from_secs(20), self.out.next_line())
                .await
                .expect("hub output timed out")
                .unwrap()
                .expect("hub exited");
            if let Some(rest) = l.strip_prefix(prefix) {
                return rest.to_owned();
            }
        }
    }
    pub async fn otp(&mut self, kind: &str) -> (String, String) {
        self.stdin
            .write_all(format!("otp {kind}\n").as_bytes())
            .await
            .unwrap();
        let l = self.line("OTP ").await;
        let (otp, rest) = l.split_once(' ').unwrap();
        let fp = rest.trim_end_matches(')').rsplit(' ').next().unwrap();
        (otp.to_owned(), fp.to_owned())
    }
}

/// stderr log of a child living in `dir` (`<dir>.err`), so a failing test can print it.
pub fn log_file(dir: &Path) -> Stdio {
    let _ = std::fs::create_dir_all(dir.parent().unwrap());
    Stdio::from(std::fs::File::create(dir.with_extension("err")).unwrap())
}

pub fn read_log(dir: &Path) -> String {
    std::fs::read_to_string(dir.with_extension("err")).unwrap_or_default()
}

pub async fn start_hub(dir: &Path) -> Hub {
    start_hub_env(dir, &[]).await
}

pub async fn start_hub_env(dir: &Path, env: &[(&str, &str)]) -> Hub {
    let mut child = Command::new(bin("ulak-remote", "ulak-remote"))
        .env("ULAK_DIR", dir)
        .env("ULAK_ADDR", "127.0.0.1:0")
        .env("ULAK_BOOTSTRAP_OTP", "node")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log_file(dir))
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let out = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut hub = Hub {
        _child: child,
        stdin,
        out,
        url: String::new(),
    };
    hub.url = hub.line("ulak-remote listening on ").await;
    hub
}
