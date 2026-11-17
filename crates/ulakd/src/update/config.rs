//! `update.json` in the ulakd config dir (written with defaults on first run).
use super::trust::ALLOW_HTTP;
use crate::errors::UpdateError;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Hub,
    Github,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateConfig {
    /// Where releases come from. Unset = `hub` for an enrolled daemon, else `github`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    /// `owner/name` of the GitHub repo that publishes releases.
    pub repo: String,
    pub api_base: String,
    /// File holding a GitHub token with read access to the releases (the repo is private).
    /// `ULAKD_GH_TOKEN` takes precedence. The token goes only into an Authorization header.
    pub token_file: Option<String>,
    pub interval_hours: u64,
    /// `None` = default: on, except inside a container. `ULAKD_AUTO_UPDATE` overrides.
    pub auto_update: Option<bool>,
    /// Only `stable` exists.
    pub channel: String,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            source: None,
            repo: "ensaremirerol/ulak".into(),
            api_base: "https://api.github.com".into(),
            token_file: None,
            interval_hours: 6,
            auto_update: None,
            channel: "stable".into(),
        }
    }
}

/// `/.dockerenv` or `ULAKD_IN_CONTAINER` (anything but empty/0).
pub fn in_container(env: &dyn Fn(&str) -> Option<String>) -> bool {
    Path::new("/.dockerenv").exists() || env("ULAKD_IN_CONTAINER").is_some_and(|v| !flag_off(&v))
}

fn flag_off(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "" | "0" | "false" | "off" | "no"
    )
}

impl UpdateConfig {
    /// Reads `update.json`; on first run writes the defaults (auto_update spelled out).
    pub fn load_or_init(dir: &Path, container: bool) -> Result<Self, UpdateError> {
        let path = dir.join("update.json");
        let cfg = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Self {
                    auto_update: Some(!container),
                    ..Self::default()
                };
                std::fs::create_dir_all(dir)?;
                std::fs::write(&path, serde_json::to_string_pretty(&cfg)? + "\n")?;
                cfg
            }
            Err(e) => return Err(e.into()),
        };
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), UpdateError> {
        let bad = |m: &str| Err(UpdateError::Config(m.to_owned()));
        let part = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        };
        match self.repo.split_once('/') {
            Some((o, r)) if part(o) && part(r) => {}
            _ => return bad("repo must look like owner/name"),
        }
        if self.channel != "stable" {
            return bad("channel must be \"stable\"");
        }
        if self.interval_hours == 0 || self.interval_hours > 24 * 90 {
            return bad("interval_hours must be 1..=2160");
        }
        let https = self.api_base.starts_with("https://");
        let http = ALLOW_HTTP && self.api_base.starts_with("http://");
        if !(https || http) || self.api_base.chars().any(|c| c.is_control() || c == ' ') {
            return bad("api_base must be an https:// URL");
        }
        Ok(())
    }

    /// The configured source, else hub when `dir` holds a hub identity, else github.
    pub fn source(&self, dir: &Path) -> Source {
        self.source.unwrap_or_else(|| {
            let enrolled = ["hub_url", "ca.pem", "cert.pem", "key.pem"]
                .iter()
                .all(|f| dir.join(f).is_file());
            if enrolled {
                Source::Hub
            } else {
                Source::Github
            }
        })
    }

    /// `ULAKD_AUTO_UPDATE` > file > (not in a container).
    pub fn auto(&self, container: bool, env: &dyn Fn(&str) -> Option<String>) -> bool {
        match env("ULAKD_AUTO_UPDATE") {
            Some(v) if !v.trim().is_empty() => !flag_off(&v),
            _ => self.auto_update.unwrap_or(!container),
        }
    }

    /// `ULAKD_GH_TOKEN`, else the first line of `token_file`. Never logged.
    pub fn token(
        &self,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Option<String>, UpdateError> {
        let raw = match env("ULAKD_GH_TOKEN").filter(|t| !t.trim().is_empty()) {
            Some(t) => t,
            None => match &self.token_file {
                Some(f) => std::fs::read_to_string(f).map_err(|e| {
                    UpdateError::Config(format!("cannot read token_file: {}", e.kind()))
                })?,
                None => return Ok(None),
            },
        };
        let t = raw.lines().next().unwrap_or("").trim().to_owned();
        if t.is_empty() {
            return Ok(None);
        }
        if t.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(UpdateError::Config(
                "token contains whitespace or control characters".into(),
            ));
        }
        Ok(Some(t))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn first_run_writes_defaults_and_container_turns_auto_off() {
        let d = tempfile::tempdir().unwrap();
        let host = UpdateConfig::load_or_init(&d.path().join("a"), false).unwrap();
        assert_eq!(host.repo, "ensaremirerol/ulak");
        assert_eq!((host.interval_hours, host.channel.as_str()), (6, "stable"));
        assert!(host.auto(false, &no_env));
        let c = UpdateConfig::load_or_init(&d.path().join("b"), true).unwrap();
        assert!(!c.auto(true, &no_env));
        // the file is now authoritative, whatever the environment says later
        let again = UpdateConfig::load_or_init(&d.path().join("b"), false).unwrap();
        assert_eq!(again.auto_update, Some(false));
    }

    #[test]
    fn env_overrides_auto() {
        let c = UpdateConfig::default();
        assert!(!c.auto(false, &|k| (k == "ULAKD_AUTO_UPDATE").then(|| "0".into())));
        assert!(c.auto(true, &|k| (k == "ULAKD_AUTO_UPDATE").then(|| "1".into())));
        assert!(!c.auto(true, &no_env));
        assert!(in_container(
            &|k| (k == "ULAKD_IN_CONTAINER").then(|| "1".into())
        ));
    }

    #[test]
    fn rejects_bad_values() {
        let ok = UpdateConfig::default();
        for c in [
            UpdateConfig {
                repo: "nope".into(),
                ..ok.clone()
            },
            UpdateConfig {
                repo: "a/b/c".into(),
                ..ok.clone()
            },
            UpdateConfig {
                channel: "beta".into(),
                ..ok.clone()
            },
            UpdateConfig {
                interval_hours: 0,
                ..ok.clone()
            },
            UpdateConfig {
                api_base: "ftp://x".into(),
                ..ok.clone()
            },
        ] {
            assert!(c.validate().is_err(), "{c:?}");
        }
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn token_from_env_or_file_and_checked() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("t");
        std::fs::write(&f, "ghp_file\nignored\n").unwrap();
        let c = UpdateConfig {
            token_file: Some(f.display().to_string()),
            ..UpdateConfig::default()
        };
        assert_eq!(c.token(&no_env).unwrap().as_deref(), Some("ghp_file"));
        let env = |k: &str| (k == "ULAKD_GH_TOKEN").then(|| "ghp_env".to_owned());
        assert_eq!(c.token(&env).unwrap().as_deref(), Some("ghp_env"));
        assert_eq!(UpdateConfig::default().token(&no_env).unwrap(), None);
        let bad = |k: &str| (k == "ULAKD_GH_TOKEN").then(|| "a b".to_owned());
        assert!(c.token(&bad).is_err());
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;

    #[test]
    fn source_defaults_by_identity_and_parses_lowercase() {
        let d = tempfile::tempdir().unwrap();
        let c = UpdateConfig::default();
        assert_eq!(c.source(d.path()), Source::Github);
        for f in ["hub_url", "ca.pem", "cert.pem"] {
            std::fs::write(d.path().join(f), "x").unwrap();
        }
        assert_eq!(c.source(d.path()), Source::Github); // incomplete identity
        std::fs::write(d.path().join("key.pem"), "x").unwrap();
        assert_eq!(c.source(d.path()), Source::Hub);
        let g: UpdateConfig = serde_json::from_str(r#"{"source":"github"}"#).unwrap();
        assert_eq!(g.source(d.path()), Source::Github);
        let h: UpdateConfig = serde_json::from_str(r#"{"source":"hub"}"#).unwrap();
        assert_eq!(h.source, Some(Source::Hub));
        assert!(serde_json::from_str::<UpdateConfig>(r#"{"source":"x"}"#).is_err());
        // first-run file does not pin a source
        let fresh = UpdateConfig::load_or_init(&d.path().join("n"), false).unwrap();
        assert!(!std::fs::read_to_string(d.path().join("n/update.json"))
            .unwrap()
            .contains("source"));
        assert_eq!(fresh.source, None);
    }
}
