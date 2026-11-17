//! Connects spawned agents to the hub's MCP endpoint: a per-agent config file (mode 0600) for
//! `claude`, an argv template for other profiles, and `NODE_EXTRA_CA_CERTS` for the hub CA.
use crate::{
    agents::{binary_name, Mcp, Recipe},
    errors::AgentError,
};
use std::{
    collections::BTreeMap,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
use ulak_proto::{AgentId, SpawnSpec};

/// Env var the hub puts the agent token in; the config refers to it, so the file holds no secret.
pub const TOKEN_ENV: &str = "ULAK_AGENT_TOKEN";
const URL_ENV: &str = "ULAK_MCP_URL";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Prepared {
    /// Appended after the profile args.
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// Config file to delete when the agent ends.
    pub file: Option<PathBuf>,
}

pub fn file_for(dir: &Path, id: &AgentId) -> PathBuf {
    dir.join(format!("mcp-{}.json", id.as_str()))
}

/// The JSON handed to `claude --mcp-config`.
pub fn claude_config(url: &str) -> serde_json::Value {
    serde_json::json!({"mcpServers": {"ulak": {"type": "http", "url": url,
        "headers": {"Authorization": format!("Bearer ${{{TOKEN_ENV}}}")}}}})
}

/// Mode of a recipe: explicit `mcp`, else derived from the binary name.
fn mode(recipe: &Recipe, binary: &str) -> Mcp {
    recipe.mcp.unwrap_or(if binary_name(binary) == "claude" {
        Mcp::Claude
    } else {
        Mcp::None
    })
}

/// What to add to the spawn of `spec`. Nothing unless the spec carries `ULAK_MCP_URL` and the
/// agent token (a spawn started by the hub always does).
pub fn prepare(
    recipe: &Recipe,
    spec: &SpawnSpec,
    ca_file: &Path,
    dir: &Path,
) -> Result<Prepared, AgentError> {
    let env = spec.env();
    let (Some(url), true) = (env.get(URL_ENV), env.contains_key(TOKEN_ENV)) else {
        return Ok(Prepared::default());
    };
    let mut p = Prepared::default();
    let template =
        !recipe.mcp_args.is_empty() && mode(recipe, spec.profile().binary()) != Mcp::Claude;
    if template {
        let ca = ca_file.to_string_lossy();
        p.args = recipe
            .mcp_args
            .iter()
            .map(|a| {
                a.replace("{mcp_url}", url)
                    .replace("{token_env}", TOKEN_ENV)
                    .replace("{ca_file}", &ca)
            })
            .collect();
    } else if mode(recipe, spec.profile().binary()) == Mcp::Claude {
        std::fs::create_dir_all(dir)?;
        let f = file_for(dir, spec.agent_id());
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true).mode(0o600);
        o.open(&f)?
            .write_all(claude_config(url).to_string().as_bytes())?;
        p.args = vec!["--mcp-config".into(), f.to_string_lossy().into_owned()];
        p.file = Some(f);
    } else {
        return Ok(p);
    }
    p.env.insert(
        "NODE_EXTRA_CA_CERTS".into(),
        ca_file.to_string_lossy().into_owned(),
    );
    Ok(p)
}

/// Deletes config files of agents not in `alive` (daemon start).
pub fn cleanup(dir: &Path, alive: &[AgentId]) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(id) = name
            .strip_prefix("mcp-")
            .and_then(|n| n.strip_suffix(".json"))
        else {
            continue;
        };
        if !alive.iter().any(|a| a.as_str() == id) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use ulak_proto::AgentProfile;

    fn spec(bin: &str, env: &[(&str, &str)]) -> SpawnSpec {
        SpawnSpec::new(
            AgentId::new("a1").unwrap(),
            AgentProfile::new(bin, vec![], None).unwrap(),
            "/w".into(),
            "p".into(),
            env.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
        .unwrap()
    }
    const ENV: &[(&str, &str)] = &[
        ("ULAK_MCP_URL", "https://h:1/mcp"),
        ("ULAK_AGENT_TOKEN", "tok"),
    ];
    fn tmp(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ulakd-mcp-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn claude_gets_0600_config_without_token_and_ca_env() {
        let d = tmp("claude");
        let p = prepare(
            &Recipe::default(),
            &spec("/usr/bin/claude", ENV),
            Path::new("/c/ca.pem"),
            &d,
        )
        .unwrap();
        let f = p.file.clone().unwrap();
        assert_eq!(p.args, ["--mcp-config", f.to_str().unwrap()]);
        assert_eq!(p.env["NODE_EXTRA_CA_CERTS"], "/c/ca.pem");
        assert_eq!(
            std::fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let txt = std::fs::read_to_string(&f).unwrap();
        assert!(!txt.contains("tok"));
        let v: serde_json::Value = serde_json::from_str(&txt).unwrap();
        let s = &v["mcpServers"]["ulak"];
        assert_eq!(
            (s["type"].as_str(), s["url"].as_str()),
            (Some("http"), Some("https://h:1/mcp"))
        );
        assert_eq!(s["headers"]["Authorization"], "Bearer ${ULAK_AGENT_TOKEN}");
        std::fs::write(d.join("mcp-dead.json"), "{}").unwrap();
        cleanup(&d, &[AgentId::new("a1").unwrap()]);
        assert!(f.exists() && !d.join("mcp-dead.json").exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn none_without_hub_env_or_for_other_binaries_or_mode_none() {
        let d = tmp("none");
        let ca = Path::new("/c");
        assert_eq!(
            prepare(&Recipe::default(), &spec("claude", &[]), ca, &d).unwrap(),
            Prepared::default()
        );
        assert_eq!(
            prepare(&Recipe::default(), &spec("antigravity", ENV), ca, &d).unwrap(),
            Prepared::default()
        );
        let off = Recipe {
            mcp: Some(Mcp::None),
            ..Recipe::default()
        };
        assert_eq!(
            prepare(&off, &spec("claude", ENV), ca, &d).unwrap(),
            Prepared::default()
        );
        assert!(!d.exists());
    }

    #[test]
    fn template_for_custom_profiles() {
        let r = Recipe {
            mcp_args: vec![
                "--mcp={mcp_url}".into(),
                "{token_env}".into(),
                "{ca_file}".into(),
            ],
            ..Recipe::default()
        };
        let p = prepare(
            &r,
            &spec("antigravity", ENV),
            Path::new("/c/ca.pem"),
            Path::new("/nope"),
        )
        .unwrap();
        assert_eq!(
            p.args,
            ["--mcp=https://h:1/mcp", "ULAK_AGENT_TOKEN", "/c/ca.pem"]
        );
        assert!(p.file.is_none());
        assert_eq!(p.env["NODE_EXTRA_CA_CERTS"], "/c/ca.pem");
    }
}
