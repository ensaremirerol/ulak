//! Admin socket: modes, hostile input, OTP flow, revoke.
#![cfg(unix)]
use std::{
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    sync::Arc,
};
use ulak_remote::admin::{call, AdminRequest, AdminResponse, AdminSocket};
use ulak_remote::*;

struct Fx {
    hub: Arc<Hub>,
    run: Running,
    sock: AdminSocket,
    path: PathBuf,
    dir: PathBuf,
}

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("ulak-admin-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
    d
}

async fn start() -> Fx {
    let dir = tmp();
    let ca = Ca::load_or_create(&dir).unwrap();
    let hub = Hub::new(
        Arc::new(Store::open_in_memory().unwrap()),
        ca,
        &["localhost".into(), "127.0.0.1".into()],
        HubConfig::default(),
    )
    .unwrap();
    let run = hub.bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let path = dir.join("admin.sock");
    let sock = hub.serve_admin(&path, vec!["x".into()]).await.unwrap();
    Fx {
        hub,
        run,
        sock,
        path,
        dir,
    }
}

async fn call_async(path: &Path, r: AdminRequest) -> Result<AdminResponse, String> {
    let p = path.to_owned();
    tokio::task::spawn_blocking(move || call(&p, r))
        .await
        .unwrap()
}

async fn raw(path: &Path, bytes: &[u8]) -> String {
    let (path, bytes) = (path.to_owned(), bytes.to_owned());
    tokio::task::spawn_blocking(move || raw_sync(&path, &bytes))
        .await
        .unwrap()
}

fn raw_sync(path: &Path, bytes: &[u8]) -> String {
    let mut s = UnixStream::connect(path).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let _ = s.write_all(bytes);
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

#[tokio::test]
async fn socket_is_0600_and_removed_on_close() {
    let fx = start().await;
    let mode = std::fs::metadata(&fx.path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    fx.sock.close();
    assert!(!fx.path.exists());
    fx.run.abort();
}

#[tokio::test]
async fn stale_socket_is_replaced_and_other_files_refused() {
    let fx = start().await;
    let p2 = fx.dir.join("stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&p2).unwrap()); // leaves the file, nobody listens
    let s = fx.hub.serve_admin(&p2, vec![]).await.unwrap();
    s.close();
    // a live hub's socket is not stolen
    assert!(fx.hub.serve_admin(&fx.path, vec![]).await.is_err());
    let f = fx.dir.join("file");
    std::fs::write(&f, "x").unwrap();
    assert!(fx.hub.serve_admin(&f, vec![]).await.is_err());
    assert_eq!(std::fs::read(&f).unwrap(), b"x");
    // world-writable parent
    let w = tmp();
    std::fs::set_permissions(&w, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(fx.hub.serve_admin(&w.join("a.sock"), vec![]).await.is_err());
}

#[tokio::test]
async fn hostile_lines_are_rejected_and_hub_survives() {
    let fx = start().await;
    for bad in [
        &b"garbage\n"[..],
        b"{\"v\":1,\"op\":\"nope\"}\n",
        b"{\"v\":2,\"op\":\"status\"}\n",
        b"\xff\xfe\n",
        b"{\"v\":1,\"op\":\"revoke\",\"id\":\"../x y\"}\n",
    ] {
        let r = raw(&fx.path, bad).await;
        assert!(r.contains("\"res\":\"error\""), "{bad:?} -> {r:?}");
    }
    let big = vec![b'a'; 100_000];
    assert!(raw(&fx.path, &big).await.contains("too long"));
    // a connection that sends nothing is cut by the timeout, not hung forever (checked by
    // the others still being served meanwhile)
    let _idle = UnixStream::connect(&fx.path).unwrap();
    assert!(matches!(
        call_async(&fx.path, AdminRequest::Status).await,
        Ok(AdminResponse::Status { .. })
    ));
}

#[tokio::test]
async fn concurrent_connections_work() {
    let fx = start().await;
    let hs: Vec<_> = (0..16)
        .map(|_| {
            let p = fx.path.clone();
            tokio::spawn(async move { call_async(&p, AdminRequest::Devices).await })
        })
        .collect();
    for h in hs {
        assert!(matches!(
            h.await.unwrap(),
            Ok(AdminResponse::Devices { .. })
        ));
    }
}

#[tokio::test]
async fn fingerprint_matches_otp_offer_and_ca_file() {
    let fx = start().await;
    let AdminResponse::Fingerprint {
        fingerprint,
        ca_pem,
    } = call_async(&fx.path, AdminRequest::Fingerprint)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        fingerprint,
        fx.hub.enroller().begin(DeviceKind::Ui, 0).hub_fingerprint
    );
    assert_eq!(
        ca_pem,
        std::fs::read_to_string(fx.dir.join("ca.pem")).unwrap()
    );
}

#[tokio::test]
async fn missing_socket_gives_clear_error() {
    let e = call(&tmp().join("admin.sock"), AdminRequest::Status).unwrap_err();
    assert!(e.contains("not running"), "{e}");
}

#[tokio::test]
async fn otp_reply_has_expiry_and_fingerprint() {
    // Enrollment itself is covered end to end in tests/hub.rs.
    let fx = start().await;
    let AdminResponse::Otp {
        expires_in_s,
        fingerprint,
        ..
    } = call_async(
        &fx.path,
        AdminRequest::Otp {
            kind: DeviceKind::Ui,
        },
    )
    .await
    .unwrap()
    else {
        panic!()
    };
    assert!((290..=300).contains(&expires_in_s));
    assert_eq!(fingerprint.len(), 64);
}

#[tokio::test]
async fn revoke_lists_and_reports_unknown() {
    let fx = start().await;
    let id = Devices::add(&**fx.hub.store(), "box", DeviceKind::Node, "pk", 1).unwrap();
    let AdminResponse::Devices { devices } =
        call_async(&fx.path, AdminRequest::Devices).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(devices.len(), 1);
    call_async(
        &fx.path,
        AdminRequest::Revoke {
            id: id.as_str().into(),
        },
    )
    .await
    .unwrap();
    let AdminResponse::Status { devices, .. } =
        call_async(&fx.path, AdminRequest::Status).await.unwrap()
    else {
        panic!()
    };
    assert_eq!(devices, 0);
    assert!(
        call_async(&fx.path, AdminRequest::RevokePasskey { id: "pk-zz".into() })
            .await
            .is_err()
    );
}
