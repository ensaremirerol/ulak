//! Update source "hub" against an in-process fake hub that requires a client certificate.
//! The hub is untrusted for integrity: tampered files must die in the normal verification.
use ed25519_dalek::{Signer, SigningKey};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls::{
    crypto::aws_lc_rs,
    pki_types::{CertificateDer, PrivateKeyDer},
    server::WebPkiClientVerifier,
    RootCertStore, ServerConfig,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicI32, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_rustls::TlsAcceptor;
use ulak_proto::{UpdateState, VersionInfo};
use ulakd::{
    errors::UpdateError,
    update::{
        config::{Source, UpdateConfig},
        Outcome, Updater, RESTART_EXIT,
    },
};

const TARGET: &str = "ulakd-x86_64-unknown-linux-gnu.tar.gz";
const NO_RELEASE: &str =
    "hub has no release cached yet: ask the admin to run `ulak-remote admin releases-refresh`";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn tar_gz(data: &[u8]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o755);
    h.set_cksum();
    b.append_data(&mut h, "ulakd", data).unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    std::io::Write::write_all(&mut gz, &b.into_inner().unwrap()).unwrap();
    gz.finish().unwrap()
}

#[derive(Default)]
struct Hub {
    /// path -> (status, body)
    routes: Mutex<HashMap<String, (u16, Vec<u8>)>>,
    hits: Mutex<Vec<String>>,
}

fn cert(
    ca: &Issuer<'static, KeyPair>,
    cn: &str,
    sans: Vec<String>,
    eku: ExtendedKeyUsagePurpose,
) -> (String, String, Vec<u8>, Vec<u8>) {
    let key = KeyPair::generate().unwrap();
    let mut p = CertificateParams::new(sans).unwrap();
    p.distinguished_name.push(DnType::CommonName, cn);
    p.extended_key_usages = vec![eku];
    let c = p.signed_by(&key, ca).unwrap();
    (
        c.pem(),
        key.serialize_pem(),
        c.der().to_vec(),
        key.serialize_der(),
    )
}

struct Rig {
    hub: Arc<Hub>,
    u: Arc<Updater>,
    dir: tempfile::TempDir,
    exit: Arc<AtomicI32>,
    github: TcpListener,
}

fn signer() -> SigningKey {
    SigningKey::from_bytes(&[7; 32])
}

impl Hub {
    fn put(&self, path: &str, status: u16, body: Vec<u8>) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.into(), (status, body));
    }
    /// A correctly signed 0.2.0 release as the hub would serve it.
    fn publish(&self) {
        let tgz = tar_gz(b"#!/bin/sh\necho new\n");
        let sums = format!("{}  {TARGET}\n", hex(&Sha256::digest(&tgz)));
        let k = signer();
        let files = [
            (TARGET.to_owned(), tgz.clone()),
            (format!("{TARGET}.sig"), k.sign(&tgz).to_bytes().to_vec()),
            ("SHA256SUMS".to_owned(), sums.clone().into_bytes()),
            (
                "SHA256SUMS.sig".to_owned(),
                k.sign(sums.as_bytes()).to_bytes().to_vec(),
            ),
        ];
        let assets: Vec<_> = files
            .iter()
            .map(|(n, d)| format!(r#"{{"name":"{n}","size":{},"sha256":"x"}}"#, d.len()))
            .collect();
        self.put(
            "/api/releases/latest",
            200,
            format!(
                r#"{{"version":"0.2.0","published_at":"t","assets":[{}],"checked_at":1}}"#,
                assets.join(",")
            )
            .into_bytes(),
        );
        for (n, d) in files {
            self.put(&format!("/api/releases/0.2.0/{n}"), 200, d);
        }
    }
    fn tamper(&self, name: &str) {
        let p = format!("/api/releases/0.2.0/{name}");
        let mut r = self.routes.lock().unwrap();
        let e = r.get_mut(&p).unwrap();
        let last = e.1.len() - 1;
        e.1[last] ^= 1; // same length: the size check passes, verification must not
    }
}

async fn rig(source: Option<Source>) -> Rig {
    let ca_key = KeyPair::generate().unwrap();
    let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.distinguished_name.push(DnType::CommonName, "hub ca");
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca_cert = p.self_signed(&ca_key).unwrap();
    let ca_pem = ca_cert.pem();
    let issuer = Issuer::new(p, ca_key);
    let (_, _, sc, sk) = cert(
        &issuer,
        "hub",
        vec!["127.0.0.1".into()],
        ExtendedKeyUsagePurpose::ServerAuth,
    );
    let (cpem, kpem, _, _) = cert(
        &issuer,
        "node-1",
        vec![],
        ExtendedKeyUsagePurpose::ClientAuth,
    );
    let mut roots = RootCertStore::empty();
    roots.add(ca_cert.der().clone()).unwrap();
    let prov = Arc::new(aws_lc_rs::default_provider());
    let cfg = ServerConfig::builder_with_provider(prov.clone())
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(
            WebPkiClientVerifier::builder_with_provider(Arc::new(roots), prov)
                .build()
                .unwrap(),
        )
        .with_single_cert(
            vec![CertificateDer::from(sc)],
            PrivateKeyDer::try_from(sk).unwrap(),
        )
        .unwrap();
    let acc = TlsAcceptor::from(Arc::new(cfg));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let hub: Arc<Hub> = Arc::default();
    let h = Arc::clone(&hub);
    tokio::spawn(async move {
        loop {
            let (tcp, _) = l.accept().await.unwrap();
            let (acc, hub) = (acc.clone(), Arc::clone(&h));
            tokio::spawn(async move {
                let Ok(mut s) = acc.accept(tcp).await else {
                    return;
                };
                let mut buf = vec![0u8; 8192];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_owned();
                hub.hits.lock().unwrap().push(path.clone());
                let (st, body) = hub
                    .routes
                    .lock()
                    .unwrap()
                    .get(&path)
                    .cloned()
                    .unwrap_or((404, vec![]));
                let head = format!(
                    "HTTP/1.1 {st} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(&body).await;
                let _ = s.shutdown().await;
            });
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let conf = dir.path().join("conf");
    std::fs::create_dir_all(&conf).unwrap();
    std::fs::write(conf.join("hub_url"), format!("https://127.0.0.1:{port}\n")).unwrap();
    std::fs::write(conf.join("ca.pem"), ca_pem).unwrap();
    std::fs::write(conf.join("cert.pem"), cpem).unwrap();
    std::fs::write(conf.join("key.pem"), kpem).unwrap();
    let exe = dir.path().join("bin/ulakd");
    std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
    std::fs::write(&exe, "OLD").unwrap();
    // A GitHub stand-in that must never be contacted.
    let github = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let cfg = UpdateConfig {
        source,
        api_base: format!("http://{}", github.local_addr().unwrap()),
        ..UpdateConfig::default()
    };
    let info = VersionInfo::new("0.1.0", "g", 1, "linux", "x86_64").unwrap();
    let exit = Arc::new(AtomicI32::new(0));
    let mut u = Updater::new(
        cfg,
        true,
        Some("tok".into()),
        info,
        exe,
        conf,
        Ok(signer().verifying_key()),
    );
    let e = Arc::clone(&exit);
    u.exit = Box::new(move |c| e.store(c, Ordering::SeqCst));
    u.restart_delay = Duration::ZERO;
    Rig {
        hub,
        u: Arc::new(u),
        dir,
        exit,
        github,
    }
}

fn failed(r: &Rig) -> String {
    match r.u.subscribe().borrow().clone() {
        Some(UpdateState::Failed { message }) => message,
        s => panic!("expected Failed, got {s:?}"),
    }
}

fn exe_text(r: &Rig) -> String {
    std::fs::read_to_string(r.dir.path().join("bin/ulakd")).unwrap()
}

fn no_github(r: &Rig) {
    assert!(
        r.github.try_accept_is_empty(),
        "GitHub must not be contacted"
    );
}

trait Idle {
    fn try_accept_is_empty(&self) -> bool;
}
impl Idle for TcpListener {
    fn try_accept_is_empty(&self) -> bool {
        use std::task::{Context, Poll};
        let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
        matches!(self.poll_accept(&mut cx), Poll::Pending)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn update_from_the_hub_installs_and_never_touches_github() {
    let r = rig(None).await; // unset source + identity files = hub
    r.hub.publish();
    let out = r.u.run(true, false).await.unwrap().unwrap();
    assert_eq!(out, Outcome::Installed("0.2.0".into()));
    assert_eq!(exe_text(&r), "#!/bin/sh\necho new\n");
    r.u.restart().await;
    assert_eq!(r.exit.load(Ordering::SeqCst), RESTART_EXIT);
    assert_eq!(r.hub.hits.lock().unwrap().len(), 5);
    no_github(&r);
}

#[tokio::test(flavor = "multi_thread")]
async fn check_only_reports_available() {
    let r = rig(Some(Source::Hub)).await;
    r.hub.publish();
    let out = r.u.run(false, false).await.unwrap().unwrap();
    assert_eq!(out, Outcome::Available("0.2.0".into()));
    assert_eq!(exe_text(&r), "OLD");
}

#[tokio::test(flavor = "multi_thread")]
async fn tampered_archive_sig_or_sums_from_the_hub_are_rejected() {
    for (name, want) in [
        (TARGET, "signature check failed"),
        (&format!("{TARGET}.sig") as &str, "signature check failed"),
        ("SHA256SUMS", "signature check failed"),
        ("SHA256SUMS.sig", "signature check failed"),
    ] {
        let r = rig(None).await;
        r.hub.publish();
        r.hub.tamper(name);
        let e = r.u.run(true, false).await.unwrap().unwrap_err();
        assert!(e.to_string().contains(want), "{name}: {e}");
        assert_eq!(exe_text(&r), "OLD", "{name}");
        no_github(&r);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn no_cached_release_is_explained() {
    let r = rig(None).await; // hub answers 404
    assert!(r.u.run(false, false).await.unwrap().is_err());
    assert_eq!(failed(&r), NO_RELEASE);
    assert!(!failed(&r).contains("GitHub"));
    no_github(&r);
}

#[tokio::test(flavor = "multi_thread")]
async fn refusal_is_explained() {
    let r = rig(None).await;
    r.hub.put("/api/releases/latest", 403, vec![]);
    r.u.run(false, false).await;
    assert_eq!(failed(&r), "hub refused the update request");
    r.hub.put("/api/releases/latest", 401, vec![]);
    r.u.run(false, false).await;
    assert_eq!(failed(&r), "hub refused the update request");
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_hub_and_bad_identity_are_explained() {
    let r = rig(None).await;
    let conf = r.dir.path().join("conf");
    // a port nobody listens on
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    std::fs::write(conf.join("hub_url"), format!("https://127.0.0.1:{port}")).unwrap();
    r.u.run(false, false).await;
    let m = failed(&r);
    assert!(m.starts_with("hub update source unreachable: "), "{m}");
    assert!(!m.contains("repository not found"));
    // identity renewed to garbage: still an error message, never a panic
    std::fs::write(conf.join("key.pem"), "nonsense").unwrap();
    r.u.run(false, false).await;
    assert!(failed(&r).starts_with("hub update source unreachable: "));
    no_github(&r);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hub_that_lists_a_bad_version_is_rejected() {
    let r = rig(None).await;
    r.hub.put(
        "/api/releases/latest",
        200,
        br#"{"version":"../x","assets":[]}"#.to_vec(),
    );
    r.u.run(false, false).await;
    assert!(matches!(
        r.u.subscribe().borrow().clone(),
        Some(UpdateState::Failed { .. })
    ));
}

#[test]
fn explicit_github_source_ignores_the_identity() {
    let d = tempfile::tempdir().unwrap();
    let c = UpdateConfig {
        source: Some(Source::Github),
        ..UpdateConfig::default()
    };
    for f in ["hub_url", "ca.pem", "cert.pem", "key.pem"] {
        std::fs::write(d.path().join(f), "x").unwrap();
    }
    assert_eq!(c.source(d.path()), Source::Github);
    let _ = UpdateError::NoKey; // keep the import honest across feature sets
}
