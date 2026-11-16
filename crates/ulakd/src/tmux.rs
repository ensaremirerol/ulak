//! tmux agent manager. Every command is an argv vector handed to `tmux`; no shell is involved
//! (the agent binary is started through `env`, not `sh -c`).
use crate::{errors::AgentError, hub::AgentHost};
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command, sync::mpsc};
use ulak_proto::{AgentId, SpawnSpec};

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
    let mut env = spec.env().clone();
    if let Some(dir) = spec.profile().config_dir() {
        env.entry("CLAUDE_CONFIG_DIR".into())
            .or_insert_with(|| dir.to_string_lossy().into_owned());
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
    a.push(spec.profile().binary().to_owned());
    a.extend(spec.profile().args().iter().cloned());
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
    startup_delay: Duration,
    submit_delay: Duration,
    /// Agents whose prompt could not be typed; the hub session drains it.
    prompt_failed: mpsc::UnboundedSender<AgentId>,
    prompt_failures: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<AgentId>>>,
}

impl Tmux {
    /// `socket`: tmux `-L` name; `None` uses the user's default server.
    pub fn new(socket: Option<String>) -> Self {
        let (prompt_failed, rx) = mpsc::unbounded_channel();
        Self {
            socket,
            startup_delay: Duration::from_millis(1500),
            submit_delay: Duration::from_millis(300),
            prompt_failed,
            prompt_failures: Arc::new(tokio::sync::Mutex::new(rx)),
        }
    }

    /// Waits before typing the prompt (the agent TUI must be up) and before pressing Enter.
    pub fn with_delays(mut self, startup: Duration, submit: Duration) -> Self {
        self.startup_delay = startup;
        self.submit_delay = submit;
        self
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
            Err(AgentError::Tmux { stderr, .. })
                if stderr.contains("no server running") || stderr.contains("No such file") =>
            {
                Ok(Vec::new())
            }
            Err(e) => Err(e),
        }
    }

    pub async fn spawn_agent(&self, spec: &SpawnSpec) -> Result<(), AgentError> {
        let argv = new_session_argv(spec)?;
        match self.run("new-session", argv, None).await {
            Err(AgentError::Tmux { stderr, .. }) if stderr.contains("duplicate session") => {
                return Err(AgentError::AlreadyExists(
                    spec.agent_id().as_str().to_owned(),
                ))
            }
            r => r?,
        };
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

    async fn type_prompt(&self, id: &AgentId, prompt: &str) -> Result<(), AgentError> {
        tokio::time::sleep(self.startup_delay).await;
        self.paste(id, prompt.as_bytes(), true).await?;
        tokio::time::sleep(self.submit_delay).await;
        self.run("send-keys", send_enter_argv(id), None)
            .await
            .map(drop)
    }

    pub async fn kill_agent(&self, id: &AgentId) -> Result<(), AgentError> {
        self.run("kill-session", kill_argv(id), None)
            .await
            .map(drop)
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
                    let frame = format!("\x1b[H\x1b[2J{}", screen.replace('\n', "\r\n"));
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
    fn explicit_env_beats_profile_config_dir() {
        let a = new_session_argv(&spec(Some("/cfg"), &[("CLAUDE_CONFIG_DIR", "/mine")])).unwrap();
        assert!(a.contains(&"CLAUDE_CONFIG_DIR=/mine".to_string()));
        assert!(!a.contains(&"CLAUDE_CONFIG_DIR=/cfg".to_string()));
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
