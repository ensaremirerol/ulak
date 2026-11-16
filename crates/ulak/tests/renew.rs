//! Certificate renewal against the real hub binary: a short-lived cert is swapped near expiry
//! (client helper, running `ulakd`, and the UI client), a long-lived one is refused.
mod common;
use common::{bin, start_hub_env};
use std::{
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::process::Command;
use ulak::{model::ConnStatus, net::start_in};
use ulakd::{config::Identity, errors::JoinError, renew};

async fn enroll(hub: &mut common::Hub, kind: &str, dir: &std::path::Path) -> Identity {
    let (otp, fp) = hub.otp(kind).await;
    let mut ask = |q: &str| -> std::io::Result<String> {
        Ok(if q.contains("fingerprint") {
            fp.clone()
        } else {
            otp.clone()
        })
    };
    ulakd::join::join(&hub.url, dir, &mut ask).await.unwrap();
    Identity::load(dir).unwrap()
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ulak-renew-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[tokio::test(flavor = "multi_thread")]
async fn short_lived_certs_are_renewed_and_used() {
    let root = tmp("short");
    let mut hub = start_hub_env(&root.join("hub"), &[("ULAK_CERT_DAYS", "20")]).await;
    let _ = hub.line("OTP ").await; // bootstrap code, unused

    // the helper: due at once (20 d <= 30 d), new cert for the same device, files replaced
    let ui_dir = root.join("ui");
    let old = enroll(&mut hub, "ui", &ui_dir).await;
    assert!(renew::is_due(&old.cert_pem, renew::now_ms()).unwrap());
    let new = renew::renew_if_due(&ui_dir, &old)
        .await
        .unwrap()
        .expect("renewed");
    assert_eq!(new.node_id, old.node_id);
    assert_ne!(new.cert_pem, old.cert_pem);
    assert_eq!(
        std::fs::read_to_string(ui_dir.join("cert.pem")).unwrap(),
        new.cert_pem
    );
    assert!(!ui_dir.join("cert.pem.new").exists());
    assert!(
        renew::not_after_ms(&new.cert_pem).unwrap() >= renew::not_after_ms(&old.cert_pem).unwrap()
    );

    // the UI client connects with the renewed identity (and renews itself on connect)
    let mut app = ulak::app::App {
        net: Some(start_in(&new, Some(ui_dir.clone()), Arc::new(|| {})).unwrap()),
        ..Default::default()
    };
    wait(|| {
        app.pump();
        app.ui.conn == ConnStatus::Connected
    })
    .await;

    // a running ulakd renews at startup and keeps its connection to the hub
    let node_dir = root.join("node");
    let n_old = enroll(&mut hub, "node", &node_dir).await;
    let mut daemon = Command::new(bin("ulakd", "ulakd"))
        .arg("run")
        .env("ULAKD_DIR", &node_dir)
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    wait(|| std::fs::read_to_string(node_dir.join("cert.pem")).is_ok_and(|c| c != n_old.cert_pem))
        .await;
    wait(|| {
        app.pump();
        app.ui.nodes.iter().any(|n| n.id == n_old.node_id)
    })
    .await;
    let _ = daemon.start_kill();
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test(flavor = "multi_thread")]
async fn fresh_cert_is_not_renewed() {
    let root = tmp("fresh");
    let mut hub = start_hub_env(&root.join("hub"), &[]).await;
    let _ = hub.line("OTP ").await;
    let dir = root.join("ui");
    let id = enroll(&mut hub, "ui", &dir).await;
    assert!(renew::renew_if_due(&dir, &id).await.unwrap().is_none());
    // forcing it anyway: the hub says 409 and nothing on disk changes
    let r = renew::renew(&dir, &id).await;
    assert!(
        matches!(r, Err(JoinError::Rejected { status: 409, .. })),
        "{r:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("cert.pem")).unwrap(),
        id.cert_pem
    );
    let _ = std::fs::remove_dir_all(root);
}

async fn wait(mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(40);
    while !cond() {
        assert!(Instant::now() < end, "timed out");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
