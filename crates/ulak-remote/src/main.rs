//! `ulak-remote`: the hub binary. Config by env: ULAK_DIR (default ./ulak-data), ULAK_ADDR
//! (default 0.0.0.0:8443), ULAK_SANS (comma list, default localhost,127.0.0.1),
//! ULAK_MCP_URL, ULAK_RP_ID / ULAK_ORIGIN (passkey relying party; default first SAN), ULAK_WEB_DIR (web build served at /), ULAK_HTTP_ADDR (optional plain-HTTP listener for a reverse proxy, e.g. 0.0.0.0:8080), ULAK_TRUSTED_PROXIES (comma list of proxy IPs/CIDRs whose X-Forwarded-For is believed; default none), ULAK_MAX_CPU_PCT / ULAK_MAX_GPU_PCT (optional hub-side spawn caps, default 100 = off), ULAK_GITHUB_API (GitHub REST base, default https://api.github.com; http only for loopback), ULAK_RELEASE_REPO (owner/name, default ensaremirerol/ulak), ULAK_RELEASE_INTERVAL_HOURS (default 6), ULAK_RELEASE_CREDENTIAL (stored credential id, preferred) or ULAK_RELEASE_TOKEN_FILE / ULAK_GH_TOKEN (kept in memory only) for the hub release cache that nodes update from, ULAK_BOOTSTRAP_OTP=node|ui|browser to print a one-time code at start.
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::io::AsyncBufReadExt;
use ulak_proto::{CredentialId, GitToken};
use ulak_remote::{Ca, DeviceKind, Hub, HubConfig, Store, TrustedProxies};

fn parse_kind(s: &str) -> Option<DeviceKind> {
    match s.trim() {
        "node" | "device" => Some(DeviceKind::Node),
        "ui" => Some(DeviceKind::Ui),
        "browser" => Some(DeviceKind::Browser),
        _ => None,
    }
}

fn print_otp(hub: &Hub, kind: DeviceKind) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let offer = hub.enroller().begin(kind, now);
    #[cfg(unix)]
    println!(
        "{}",
        ulak_remote::admin::otp_line(offer.otp.expose(), &offer.hub_fingerprint)
    );
    #[cfg(not(unix))]
    println!(
        "OTP {} (hub CA sha256 {})",
        offer.otp.expose(),
        offer.hub_fingerprint
    );
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--version") {
        let v = ulak_remote::version_info();
        println!(
            "ulak-remote {} ({}) proto {}",
            v.version(),
            v.git(),
            v.proto()
        );
        return Ok(());
    }
    let env = |k: &str| std::env::var(k).ok();
    #[cfg(unix)]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if args.first().map(String::as_str) == Some("admin") {
            return match ulak_remote::admin::cli(&args[1..], env("ULAK_DIR"), env("ULAK_SANS")) {
                Ok(out) => {
                    println!("{out}");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("ulak-remote admin: {e}");
                    std::process::exit(1);
                }
            };
        }
    }
    let dir = PathBuf::from(env("ULAK_DIR").unwrap_or_else(|| "ulak-data".into()));
    let addr: SocketAddr = env("ULAK_ADDR")
        .unwrap_or_else(|| "0.0.0.0:8443".into())
        .parse()?;
    let sans: Vec<String> = env("ULAK_SANS")
        .unwrap_or_else(|| "localhost,127.0.0.1".into())
        .split(',')
        .map(str::to_owned)
        .collect();
    let ca = Ca::load_or_create(&dir).map_err(|e| anyhow::anyhow!(e))?;
    let store = Arc::new(Store::open(&dir.join("hub.db"))?);
    // Test knobs: ULAK_ASK_TIMEOUT_MS, ULAK_NUDGE_TICK_MS, ULAK_NUDGE_EVERY_MS, ULAK_CERT_DAYS, ULAK_RENEW_WINDOW_DAYS.
    fn num<T: std::str::FromStr>(k: &str) -> anyhow::Result<Option<T>> {
        match std::env::var(k) {
            Ok(v) => v.parse().map(Some).map_err(|_| anyhow::anyhow!("bad {k}")),
            Err(_) => Ok(None),
        }
    }
    let d = HubConfig::default();
    let ms = |k| num::<u64>(k).map(|v| v.map(std::time::Duration::from_millis));
    // ULAK_DEV_HTTP=1: also serve plain HTTP on 127.0.0.1:$ULAK_DEV_HTTP_PORT (default 8080), and
    // unless ULAK_RP_ID / ULAK_ORIGIN say otherwise, run passkeys for http://localhost:<port>.
    let dev_http = env("ULAK_DEV_HTTP").as_deref() == Some("1");
    let dev_port: u16 = num("ULAK_DEV_HTTP_PORT")?.unwrap_or(8080);
    let http_addr: Option<SocketAddr> = env("ULAK_HTTP_ADDR")
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.parse())
        .transpose()
        .map_err(|e| anyhow::anyhow!("bad ULAK_HTTP_ADDR: {e}"))?;
    let trusted = TrustedProxies::parse(&env("ULAK_TRUSTED_PROXIES").unwrap_or_default())
        .map_err(|e| anyhow::anyhow!("bad ULAK_TRUSTED_PROXIES: {e}"))?;
    if http_addr.is_some_and(|a| !a.ip().is_loopback()) && trusted.is_empty() {
        eprintln!("WARNING: ULAK_HTTP_ADDR is not loopback and ULAK_TRUSTED_PROXIES is empty: X-Forwarded-For is ignored, so every client behind the proxy shares one enrollment rate limit. Set ULAK_TRUSTED_PROXIES to the proxy address.");
    }
    let release_credential = env("ULAK_RELEASE_CREDENTIAL")
        .filter(|v| !v.trim().is_empty())
        .map(|v| CredentialId::new(v.trim()))
        .transpose()
        .map_err(|e| anyhow::anyhow!("bad ULAK_RELEASE_CREDENTIAL: {e}"))?;
    let token_text = match env("ULAK_RELEASE_TOKEN_FILE") {
        Some(f) => Some(
            std::fs::read_to_string(&f)
                .map_err(|e| anyhow::anyhow!("cannot read ULAK_RELEASE_TOKEN_FILE {f}: {e}"))?,
        ),
        None => env("ULAK_GH_TOKEN"),
    };
    let release_token = token_text
        .map(|t| GitToken::new(t.trim()))
        .transpose()
        .map_err(|_| anyhow::anyhow!("the release token (ULAK_RELEASE_TOKEN_FILE / ULAK_GH_TOKEN) is not a valid GitHub token"))?;
    let cfg = HubConfig {
        trusted_proxies: trusted,
        ask_timeout: ms("ULAK_ASK_TIMEOUT_MS")?.unwrap_or(d.ask_timeout),
        nudge_tick: ms("ULAK_NUDGE_TICK_MS")?.unwrap_or(d.nudge_tick),
        nudge_every_ms: num("ULAK_NUDGE_EVERY_MS")?.unwrap_or(d.nudge_every_ms),
        cert_days: num("ULAK_CERT_DAYS")?.unwrap_or(d.cert_days),
        renew_window_days: num("ULAK_RENEW_WINDOW_DAYS")?.unwrap_or(d.renew_window_days),
        thresholds: ulak_remote::Thresholds::new(
            num("ULAK_MAX_CPU_PCT")?.unwrap_or(100.0),
            num("ULAK_MAX_GPU_PCT")?.unwrap_or(100.0),
        ),
        mcp_url: env("ULAK_MCP_URL"),
        rp_id: env("ULAK_RP_ID").or_else(|| dev_http.then(|| "localhost".into())),
        origin: env("ULAK_ORIGIN")
            .or_else(|| dev_http.then(|| format!("http://localhost:{dev_port}"))),
        web_dir: env("ULAK_WEB_DIR").map(PathBuf::from),
        secrets_key_path: Some(dir.join("secrets.key")),
        github_api: env("ULAK_GITHUB_API").unwrap_or(d.github_api),
        release_dir: Some(dir.join("releases")),
        release_repo: env("ULAK_RELEASE_REPO").unwrap_or(d.release_repo),
        release_key: ulakd::update::trust::release_key().ok(),
        release_token,
        release_credential,
        release_interval: std::time::Duration::from_secs(
            3600 * num::<u64>("ULAK_RELEASE_INTERVAL_HOURS")?
                .filter(|h| *h > 0)
                .unwrap_or(6),
        ),
        ..HubConfig::default()
    };
    println!(
        "release cache: {} every {} h, {}",
        cfg.release_repo,
        cfg.release_interval.as_secs() / 3600,
        if cfg.release_credential.is_some() {
            "stored credential"
        } else if cfg.release_token.is_some() {
            "token from env/file"
        } else {
            "no token (unauthenticated: public repos only, low rate limit)"
        }
    );
    if cfg.release_key.is_none() {
        eprintln!("WARNING: this build has no release signing key; the hub release cache is off");
    }
    let hub = Hub::new(store, ca, &sans, cfg)?;
    let running = hub.bind(addr).await?;
    println!("ulak-remote listening on https://{}", running.local_addr());
    let http = match http_addr {
        Some(a) => {
            let r = hub.bind_http(a).await?;
            println!(
                "ULAK_HTTP_ADDR: plain http://{} (behind a TLS-terminating proxy)",
                r.local_addr()
            );
            Some(r)
        }
        None => None,
    };
    let dev = if dev_http {
        let r = hub
            .bind_dev_http(SocketAddr::from(([127, 0, 0, 1], dev_port)))
            .await?;
        println!(
            "ULAK_DEV_HTTP: plain http://{} (development only)",
            r.local_addr()
        );
        Some(r)
    } else {
        None
    };
    if let Some(kind) = env("ULAK_BOOTSTRAP_OTP").as_deref().and_then(parse_kind) {
        print_otp(&hub, kind);
    }
    #[cfg(unix)]
    let admin_sock = {
        let mut listen = vec![format!("https://{}", running.local_addr())];
        listen.extend(http.iter().map(|r| format!("http://{}", r.local_addr())));
        listen.extend(dev.iter().map(|r| format!("http://{}", r.local_addr())));
        match hub
            .serve_admin(&dir.join(ulak_remote::admin::SOCKET_NAME), listen)
            .await
        {
            Ok(s) => {
                println!("admin socket {}", s.path().display());
                Some(s)
            }
            Err(e) => {
                eprintln!("WARNING: admin socket disabled: {e}");
                None
            }
        }
    };
    // Admin console on stdin: `revoke <device-id>` (closes its sockets), `otp node|ui|browser` (prints a new code).
    let admin = hub.clone();
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(kind) = line.strip_prefix("otp ").and_then(parse_kind) {
                print_otp(&admin, kind);
                continue;
            }
            let Some(id) = line.strip_prefix("revoke ") else {
                continue;
            };
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64);
            match ulak_proto::NodeId::new(id.trim()) {
                Ok(id) => match admin.revoke_device(&id, now) {
                    Ok(()) => println!("revoked {}", id.as_str()),
                    Err(e) => eprintln!("revoke failed: {e}"),
                },
                Err(e) => eprintln!("bad id: {e}"),
            }
        }
    });
    // ctrl-c, and SIGTERM (what `docker stop` sends) so the admin socket is removed.
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! { r = tokio::signal::ctrl_c() => r?, _ = term.recv() => {} }
        if let Some(s) = admin_sock {
            s.close();
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    running.abort();
    if let Some(h) = http {
        h.abort();
    }
    if let Some(d) = dev {
        d.abort();
    }
    Ok(())
}
