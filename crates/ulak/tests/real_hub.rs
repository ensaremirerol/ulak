//! The UI client (`net::start`) against the real `ulak-remote` and `ulakd` binaries over mTLS:
//! enroll a ui device, ask for a device code, expect the OtpOffer and an enrolled node in the
//! Snapshot. Needs `cargo` to build the two binaries; no display is involved.
mod common;
use common::{bin, start_hub};
use std::{
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::process::Command;
use ulak::{
    model::{apply, ConnStatus, State},
    net::start,
};
use ulak_proto::{DeviceKind, UiToHub};
use ulakd::config::Identity;

#[tokio::test(flavor = "multi_thread")]
async fn ui_client_talks_to_the_real_hub() {
    let root = std::env::temp_dir().join(format!("ulak-ui-real-{}", std::process::id()));
    let (hub_dir, node_dir, ui_dir) = (root.join("hub"), root.join("node"), root.join("ui"));
    let mut hub = start_hub(&hub_dir).await;
    let l = hub.line("OTP ").await;
    let (notp, rest) = l.split_once(' ').unwrap();
    let nfp = rest.trim_end_matches(')').rsplit(' ').next().unwrap();

    let ulakd = bin("ulakd", "ulakd");
    let joined = Command::new(&ulakd)
        .args(["join", &hub.url])
        .env("ULAKD_DIR", &node_dir)
        .env("ULAKD_OTP", notp)
        .env("ULAKD_FINGERPRINT", nfp)
        .output()
        .await
        .unwrap();
    assert!(joined.status.success(), "{joined:?}");
    let _daemon = Command::new(&ulakd)
        .arg("run")
        .env("ULAKD_DIR", &node_dir)
        .stdout(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    // Enroll this test as a ui device, exactly like `ulak join`.
    let (uotp, ufp) = hub.otp("ui").await;
    let mut ask = |q: &str| -> std::io::Result<String> {
        Ok(if q.contains("fingerprint") {
            ufp.clone()
        } else {
            uotp.clone()
        })
    };
    ulakd::join::join(&hub.url, &ui_dir, &mut ask)
        .await
        .expect("ui enrollment");

    let id = Identity::load(&ui_dir).unwrap();
    let net = start(&id, Arc::new(|| {})).unwrap();
    let mut st = State::default();
    let mut asked = false;
    let end = Instant::now() + Duration::from_secs(30);
    while (st.otp.is_none() || st.nodes.is_empty()) && Instant::now() < end {
        for e in net.events.try_iter() {
            apply(&mut st, e);
        }
        // Frames queued before the socket is up are dropped by design: ask once connected.
        if !asked && st.conn == ConnStatus::Connected {
            asked = true;
            net.send(vec![UiToHub::AddDevice {
                kind: DeviceKind::Browser,
            }]);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let otp = st
        .otp
        .as_ref()
        .unwrap_or_else(|| panic!("no OtpOffer; conn {:?}, err {:?}", st.conn, st.last_error));
    assert_eq!(otp.kind, DeviceKind::Browser);
    assert_eq!(otp.otp.expose().len(), 8);
    assert!(!otp.hub_fingerprint.is_empty());
    assert!(!st.nodes.is_empty(), "snapshot has no enrolled node");
}
