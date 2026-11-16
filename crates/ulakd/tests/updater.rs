//! The whole updater against an in-process fake GitHub (plain http, allowed only by the
//! `test-key` feature that this crate's dev-dependency on itself turns on).
use axum::{
    extract::{Path as AxPath, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use ulak_proto::{UpdateState, VersionInfo};
use ulakd::{
    errors::UpdateError,
    update::{config::UpdateConfig, http::Http, install, trust, Outcome, Updater, RESTART_EXIT},
};

const TARGET: &str = "ulakd-x86_64-unknown-linux-gnu.tar.gz";
const NEW_BIN: &[u8] = b"#!/bin/sh\necho new ulakd\n";

#[derive(Default)]
struct Inner {
    tag: Mutex<String>,
    prerelease: Mutex<bool>,
    files: Mutex<HashMap<String, Vec<u8>>>,
    size_override: Mutex<HashMap<String, u64>>,
    redirect: Mutex<bool>,
    short_body: Mutex<bool>,
    base: Mutex<String>,
    /// (path, had Authorization header)
    hits: Mutex<Vec<(String, bool)>>,
}
type Fake = Arc<Inner>;

fn tar_gz(name: &str, data: &[u8]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o755);
    h.set_cksum();
    b.append_data(&mut h, name, data).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &b.into_inner().unwrap()).unwrap();
    gz.finish().unwrap()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Inner {
    /// Publishes `tag` with a correctly signed tarball (the signing key may differ from the trusted one).
    fn publish(&self, tag: &str, signer: &SigningKey, bin: &[u8]) {
        let tgz = tar_gz("ulakd", bin);
        let sums = format!("{}  {TARGET}\n", hex(&Sha256::digest(&tgz)));
        let mut f = self.files.lock().unwrap();
        f.insert(
            format!("{TARGET}.sig"),
            signer.sign(&tgz).to_bytes().to_vec(),
        );
        f.insert(
            "SHA256SUMS.sig".into(),
            signer.sign(sums.as_bytes()).to_bytes().to_vec(),
        );
        f.insert(TARGET.into(), tgz);
        f.insert("SHA256SUMS".into(), sums.into_bytes());
        *self.tag.lock().unwrap() = tag.into();
    }
    fn hits(&self, prefix: &str) -> Vec<(String, bool)> {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.starts_with(prefix))
            .cloned()
            .collect()
    }
}

async fn latest(State(s): State<Fake>, h: HeaderMap) -> Response {
    s.hits.lock().unwrap().push((
        "/repos/o/r/releases/latest".into(),
        h.contains_key("authorization"),
    ));
    let base = s.base.lock().unwrap().clone();
    let sizes = s.size_override.lock().unwrap();
    let assets: Vec<_> = s
        .files
        .lock()
        .unwrap()
        .iter()
        .map(|(n, d)| {
            let size = sizes.get(n).copied().unwrap_or(d.len() as u64);
            format!(r#"{{"name":"{n}","url":"{base}/assets/{n}","size":{size}}}"#)
        })
        .collect();
    let body = format!(
        r#"{{"tag_name":"{}","draft":false,"prerelease":{},"assets":[{}]}}"#,
        s.tag.lock().unwrap(),
        s.prerelease.lock().unwrap(),
        assets.join(",")
    );
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn asset(State(s): State<Fake>, AxPath(name): AxPath<String>, h: HeaderMap) -> Response {
    s.hits
        .lock()
        .unwrap()
        .push((format!("/assets/{name}"), h.contains_key("authorization")));
    assert_eq!(h.get("accept").unwrap(), "application/octet-stream");
    if *s.redirect.lock().unwrap() {
        return (
            StatusCode::FOUND,
            [(header::LOCATION, format!("/blob/{name}?sig=secret"))],
        )
            .into_response();
    }
    serve(&s, &name)
}

async fn blob(State(s): State<Fake>, AxPath(name): AxPath<String>, h: HeaderMap) -> Response {
    s.hits
        .lock()
        .unwrap()
        .push((format!("/blob/{name}"), h.contains_key("authorization")));
    serve(&s, &name)
}

fn serve(s: &Inner, name: &str) -> Response {
    let data = s.files.lock().unwrap().get(name).cloned().unwrap();
    if *s.short_body.lock().unwrap() && name == TARGET {
        // promises everything, sends half, then the connection breaks
        let n = data.len();
        let half = axum::body::Bytes::copy_from_slice(&data[..n / 2]);
        let chunks = vec![Ok(half), Err(std::io::Error::other("cut"))];
        let body = axum::body::Body::from_stream(futures_util::stream::iter(chunks));
        return ([(header::CONTENT_LENGTH, n.to_string())], body).into_response();
    }
    data.into_response()
}

async fn fake() -> (Fake, String) {
    let s: Fake = Arc::default();
    let app = Router::new()
        .route("/repos/o/r/releases/latest", get(latest))
        .route("/assets/{name}", get(asset))
        .route("/blob/{name}", get(blob))
        .with_state(Arc::clone(&s));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    *s.base.lock().unwrap() = base.clone();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (s, base)
}

struct Rig {
    _dir: tempfile::TempDir,
    exe: PathBuf,
    conf: PathBuf,
    u: Arc<Updater>,
    exit: Arc<AtomicI32>,
}

fn signer(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn rig_with(base: &str, current: &str, auto: bool, key: Option<&SigningKey>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("bin/ulakd");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "OLD").unwrap();
    let conf = dir.path().join("conf");
    let cfg = UpdateConfig {
        repo: "o/r".into(),
        api_base: base.into(),
        ..UpdateConfig::default()
    };
    let info = VersionInfo::new(current, "g", 1, "linux", "x86_64").unwrap();
    let key = key
        .map(|k| Ok(k.verifying_key()))
        .unwrap_or(Err(UpdateError::NoKey));
    let exit = Arc::new(AtomicI32::new(0));
    let mut u = Updater::new(
        cfg,
        auto,
        Some("tok".into()),
        info,
        exe.clone(),
        conf.clone(),
        key,
    );
    let e = Arc::clone(&exit);
    u.exit = Box::new(move |c| e.store(c, Ordering::SeqCst));
    u.restart_delay = Duration::ZERO;
    u.initial_delay = Duration::ZERO;
    Rig {
        _dir: dir,
        exe,
        conf,
        u: Arc::new(u),
        exit,
    }
}

fn rig(base: &str) -> Rig {
    rig_with(base, "0.1.0", false, Some(&signer(1)))
}

fn state(r: &Rig) -> UpdateState {
    r.u.subscribe()
        .borrow()
        .clone()
        .expect("a state was reported")
}

fn failed(r: &Rig) -> String {
    match state(r) {
        UpdateState::Failed { message } => message,
        s => panic!("expected Failed, got {s:?}"),
    }
}

fn untouched(r: &Rig) {
    assert_eq!(std::fs::read(&r.exe).unwrap(), b"OLD");
    assert!(!install::prev_path(&r.exe).exists());
    assert!(!r.exe.with_file_name("ulakd.new").exists());
    assert_eq!(r.exit.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn update_now_installs_verifies_and_exits_for_restart() {
    let (f, base) = fake().await;
    f.publish("v0.2.0", &signer(1), NEW_BIN);
    *f.redirect.lock().unwrap() = true;
    let r = rig(&base);
    r.u.on_update_now().await;
    assert_eq!(std::fs::read(&r.exe).unwrap(), NEW_BIN);
    assert_eq!(std::fs::read(install::prev_path(&r.exe)).unwrap(), b"OLD");
    assert_eq!(r.exit.load(Ordering::SeqCst), RESTART_EXIT);
    assert_eq!(state(&r), UpdateState::Restarting);
    assert!(r.conf.join("update-pending.json").exists());
    // the token went to the API host only, never to the redirect target
    assert!(f.hits("/repos").iter().all(|h| h.1));
    assert!(f.hits("/assets").iter().all(|h| h.1));
    let blobs = f.hits("/blob");
    assert_eq!(blobs.len(), 4);
    assert!(
        blobs.iter().all(|h| !h.1),
        "Authorization leaked to the redirect target"
    );
}

#[tokio::test]
async fn check_reports_available_without_installing() {
    let (f, base) = fake().await;
    f.publish("v0.2.0", &signer(1), NEW_BIN);
    let r = rig(&base);
    r.u.on_check().await;
    assert_eq!(
        state(&r),
        UpdateState::Available {
            version: "0.2.0".into()
        }
    );
    untouched(&r);
    assert!(f.hits("/assets").is_empty());
}

#[tokio::test]
async fn older_equal_prerelease_and_junk_tags_are_not_installed() {
    for (tag, pre) in [
        ("v0.1.0", false),
        ("v0.0.9", false),
        ("v9.0.0", true),
        ("v9.0.0-rc1", false),
        ("nightly", false),
    ] {
        let (f, base) = fake().await;
        f.publish(tag, &signer(1), NEW_BIN);
        *f.prerelease.lock().unwrap() = pre;
        let r = rig(&base);
        r.u.on_update_now().await;
        assert!(
            matches!(state(&r), UpdateState::UpToDate { checked_ms } if checked_ms > 0),
            "{tag}"
        );
        untouched(&r);
        assert!(f.hits("/assets").is_empty(), "{tag}");
    }
}

#[tokio::test]
async fn bad_signature_is_refused() {
    let (f, base) = fake().await;
    f.publish("v0.2.0", &signer(9), NEW_BIN); // signed by a key we do not trust
    let r = rig(&base);
    r.u.on_update_now().await;
    assert!(failed(&r).contains("signature"), "{}", failed(&r));
    untouched(&r);
}

#[tokio::test]
async fn tampered_tarball_is_refused() {
    let (f, base) = fake().await;
    f.publish("v0.2.0", &signer(1), NEW_BIN);
    f.files
        .lock()
        .unwrap()
        .insert(TARGET.into(), tar_gz("ulakd", b"evil"));
    let r = rig(&base);
    r.u.on_update_now().await;
    assert!(failed(&r).contains("signature"), "{}", failed(&r));
    untouched(&r);
}

#[tokio::test]
async fn bad_checksum_is_refused_even_when_sums_are_signed() {
    let (f, base) = fake().await;
    f.publish("v0.2.0", &signer(1), NEW_BIN);
    let sums = format!("{}  {TARGET}\n", "a".repeat(64));
    f.files.lock().unwrap().insert(
        "SHA256SUMS.sig".into(),
        signer(1).sign(sums.as_bytes()).to_bytes().to_vec(),
    );
    f.files
        .lock()
        .unwrap()
        .insert("SHA256SUMS".into(), sums.into_bytes());
    let r = rig(&base);
    r.u.on_update_now().await;
    assert!(failed(&r).contains("checksum"), "{}", failed(&r));
    untouched(&r);
}

#[tokio::test]
async fn truncated_download_is_refused() {
    let (f, base) = fake().await;
    f.publish("v0.2.0", &signer(1), NEW_BIN);
    *f.short_body.lock().unwrap() = true; // Content-Length promises more than arrives
    let r = rig(&base);
    r.u.on_update_now().await;
    assert!(failed(&r).contains("update server"), "{}", failed(&r));
    untouched(&r);

    // the listing says N bytes, the server delivers fewer
    let (f, base) = fake().await;
    f.publish("v0.2.0", &signer(1), NEW_BIN);
    let n = f.files.lock().unwrap()[TARGET].len() as u64;
    f.size_override
        .lock()
        .unwrap()
        .insert(TARGET.into(), n + 10);
    let r = rig(&base);
    r.u.on_update_now().await;
    assert!(failed(&r).contains("truncated"), "{}", failed(&r));
    untouched(&r);
}

#[tokio::test]
async fn oversize_asset_is_refused_before_downloading() {
    let (f, base) = fake().await;
    f.publish("v0.2.0", &signer(1), NEW_BIN);
    f.size_override
        .lock()
        .unwrap()
        .insert(TARGET.into(), 500 * 1024 * 1024);
    let r = rig(&base);
    r.u.on_update_now().await;
    assert!(failed(&r).contains("limit"), "{}", failed(&r));
    assert!(f.hits("/assets").is_empty());
    untouched(&r);
}

#[test]
fn http_get_enforces_the_size_cap() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (f, base) = rt.block_on(fake());
    f.publish("v0.2.0", &signer(1), NEW_BIN);
    let url = format!("{base}/assets/SHA256SUMS");
    let h = Http::new();
    assert!(h.get(&url, None, "application/octet-stream", 1000).is_ok());
    assert!(matches!(
        h.get(&url, None, "application/octet-stream", 10),
        Err(UpdateError::TooLarge(10))
    ));
}

#[tokio::test]
async fn no_trusted_key_fails_closed_without_downloading() {
    let (f, base) = fake().await;
    f.publish("v0.2.0", &signer(1), NEW_BIN);
    let r = rig_with(&base, "0.1.0", true, None);
    r.u.on_update_now().await;
    assert!(
        failed(&r).contains("no release signing key configured"),
        "{}",
        failed(&r)
    );
    assert!(failed(&r).contains("gen-release-key.sh"));
    assert!(f.hits("/assets").is_empty());
    untouched(&r);
}

#[tokio::test]
async fn missing_platform_asset_is_reported() {
    let (f, base) = fake().await;
    f.publish("v0.2.0", &signer(1), NEW_BIN);
    f.files.lock().unwrap().remove("SHA256SUMS.sig");
    let r = rig(&base);
    r.u.on_update_now().await;
    assert!(failed(&r).contains("SHA256SUMS.sig"), "{}", failed(&r));
    untouched(&r);
}

async fn until(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..100 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn auto_update_installs_when_on_and_only_reports_when_off() {
    let (f, base) = fake().await;
    f.publish("v0.2.0", &signer(1), NEW_BIN);
    let off = rig_with(&base, "0.1.0", false, Some(&signer(1)));
    tokio::spawn(Arc::clone(&off.u).auto_loop());
    until("available", || {
        matches!(
            off.u.subscribe().borrow().clone(),
            Some(UpdateState::Available { .. })
        )
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    untouched(&off);

    let on = rig_with(&base, "0.1.0", true, Some(&signer(1)));
    tokio::spawn(Arc::clone(&on.u).auto_loop());
    until("restart", || on.exit.load(Ordering::SeqCst) == RESTART_EXIT).await;
    assert_eq!(std::fs::read(&on.exe).unwrap(), NEW_BIN);
}

#[tokio::test]
async fn a_rolled_back_version_is_not_auto_installed_again_but_can_be_forced() {
    let (f, base) = fake().await;
    f.publish("v0.2.0", &signer(1), NEW_BIN);
    let r = rig_with(&base, "0.1.0", true, Some(&signer(1)));
    std::fs::create_dir_all(&r.conf).unwrap();
    std::fs::write(r.conf.join("update-skip"), "0.2.0").unwrap();
    assert_eq!(
        r.u.run(true, true).await.unwrap().unwrap(),
        Outcome::Available("0.2.0".into())
    );
    untouched(&r);
    assert_eq!(
        r.u.run(true, false).await.unwrap().unwrap(),
        Outcome::Installed("0.2.0".into())
    );
}

#[tokio::test]
async fn install_then_early_death_rolls_back() {
    let (f, base) = fake().await;
    f.publish("v0.2.0", &signer(1), NEW_BIN);
    let r = rig(&base);
    r.u.on_update_now().await;
    assert_eq!(
        install::startup(&r.conf, &r.exe, 1_000).unwrap(),
        install::Startup::Probation
    );
    assert_eq!(
        install::startup(&r.conf, &r.exe, 5_000).unwrap(),
        install::Startup::RolledBack {
            bad: "0.2.0".into()
        }
    );
    assert_eq!(std::fs::read(&r.exe).unwrap(), b"OLD");
}

#[test]
fn default_build_ignores_the_test_key_override_and_http() {
    // A normal (feature-less) build must not honour ULAKD_TEST_RELEASE_KEY, whatever the env says.
    let sk = signer(5);
    let key_hex = hex(&sk.verifying_key().to_bytes());
    // target/debug/deps/updater-xxxx -> target (shared with the other tests: no second aws-lc build)
    let exe = std::env::current_exe().unwrap();
    let target = exe.ancestors().nth(3).unwrap().to_owned();
    let cargo = std::env::var("CARGO").unwrap_or("cargo".into());
    let ok = std::process::Command::new(cargo)
        .args(["build", "-p", "ulakd", "--bin", "ulakd", "--target-dir"])
        .arg(&target)
        .env("ULAKD_TEST_RELEASE_KEY", &key_hex)
        .env("ULAK_BUILD_VERSION", "99.0.0")
        .status()
        .unwrap()
        .success();
    assert!(ok);
    let out = std::process::Command::new(target.join("debug/ulakd"))
        .arg("--version")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let committed = trust::parse_key(include_str!("../release-key.pub")).is_ok();
    assert!(
        text.starts_with("ulakd 0.1.0 "),
        "version override leaked: {text}"
    );
    assert_eq!(
        text.contains("release key: configured"),
        committed,
        "{text}"
    );
    // and in this test build (feature on) the same env would have been honoured only at compile time
}
