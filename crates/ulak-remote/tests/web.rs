//! Passkey login, sessions, the admin API and static serving.
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::{
    rustls::{pki_types::ServerName, ClientConfig, RootCertStore},
    TlsConnector,
};
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, Message},
    Connector,
};
use ulak_proto::*;
use ulak_remote::*;
use webauthn_authenticator_rs::{prelude::WebauthnAuthenticator, softpasskey::SoftPasskey};
use webauthn_rs::prelude::{
    CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
    RequestChallengeResponse, Url,
};

struct Fx {
    hub: Arc<Hub>,
    run: Running,
    ca_pem: String,
}

async fn start(cfg: HubConfig) -> Fx {
    let dir = std::env::temp_dir().join(format!("ulak-web-{}", uuid::Uuid::now_v7()));
    let ca = Ca::load_or_create(&dir).unwrap();
    let ca_pem = ca.cert_pem().to_owned();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let hub = Hub::new(store, ca, &["localhost".into(), "127.0.0.1".into()], cfg).unwrap();
    let run = hub.bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    Fx { hub, run, ca_pem }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn origin(fx: &Fx) -> Url {
    Url::parse(&format!("https://localhost:{}", fx.run.local_addr().port())).unwrap()
}

struct Resp {
    status: u16,
    head: String,
    body: String,
}
impl Resp {
    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
        })
    }
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }
}

#[derive(Clone)]
struct Identity {
    cert: String,
    key: String,
}

fn tls(ca_pem: &str, identity: Option<&Identity>) -> ClientConfig {
    let mut roots = RootCertStore::empty();
    for c in rustls_pemfile::certs(&mut ca_pem.as_bytes()) {
        roots.add(c.unwrap()).unwrap();
    }
    let b = ClientConfig::builder().with_root_certificates(roots);
    match identity {
        None => b.with_no_client_auth(),
        Some(i) => {
            let chain = rustls_pemfile::certs(&mut i.cert.as_bytes())
                .map(|c| c.unwrap())
                .collect();
            let key = rustls_pemfile::private_key(&mut i.key.as_bytes())
                .unwrap()
                .unwrap();
            b.with_client_auth_cert(chain, key).unwrap()
        }
    }
}

async fn req(
    fx: &Fx,
    id: Option<&Identity>,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: &str,
) -> Resp {
    let addr = fx.run.local_addr();
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let conn = TlsConnector::from(Arc::new(tls(&fx.ca_pem, id)));
    let mut s = conn
        .connect(ServerName::try_from("127.0.0.1").unwrap(), tcp)
        .await
        .unwrap();
    let extra: String = headers
        .iter()
        .map(|(k, v)| format!("{k}: {v}\r\n"))
        .collect();
    let r = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(r.as_bytes()).await.unwrap();
    let mut out = String::new();
    let _ = s.read_to_string(&mut out).await;
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    Resp {
        status: head.split_whitespace().nth(1).unwrap().parse().unwrap(),
        head: head.to_owned(),
        body: body.to_owned(),
    }
}

async fn post(fx: &Fx, path: &str, body: Value) -> Resp {
    req(fx, None, "POST", path, &[], &body.to_string()).await
}

fn bearer(t: &str) -> [(&'static str, String); 1] {
    [("Authorization", format!("Bearer {t}"))]
}

fn browser_otp(fx: &Fx) -> String {
    fx.hub
        .enroller()
        .begin(DeviceKind::Browser, now())
        .otp
        .expose()
        .to_owned()
}

async fn register_at(fx: &Fx, sp: &mut SoftPasskey, otp: &str, name: &str, o: Url) -> Resp {
    let s = post(
        fx,
        "/auth/register/start",
        json!({"otp": otp, "name": name}),
    )
    .await;
    if s.status != 200 {
        return s;
    }
    let s = s.json();
    let opts: CreationChallengeResponse = serde_json::from_value(s["options"].clone()).unwrap();
    let cred: RegisterPublicKeyCredential = sp.do_registration(o, opts).unwrap();
    post(
        fx,
        "/auth/register/finish",
        json!({"challenge_id": s["challenge_id"], "credential": cred}),
    )
    .await
}

async fn register(fx: &Fx, sp: &mut SoftPasskey, name: &str) -> Resp {
    let otp = browser_otp(fx);
    register_at(fx, sp, &otp, name, origin(fx)).await
}

async fn login_at(fx: &Fx, sp: &mut SoftPasskey, o: Url) -> Resp {
    let s = post(fx, "/auth/login/start", json!({})).await;
    if s.status != 200 {
        return s;
    }
    let s = s.json();
    let opts: RequestChallengeResponse = serde_json::from_value(s["options"].clone()).unwrap();
    let cred: PublicKeyCredential = sp.do_authentication(o, opts).unwrap();
    post(
        fx,
        "/auth/login/finish",
        json!({"challenge_id": s["challenge_id"], "credential": cred}),
    )
    .await
}

async fn login(fx: &Fx, sp: &mut SoftPasskey) -> String {
    let r = login_at(fx, sp, origin(fx)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.json()["token"].as_str().unwrap().to_owned()
}

async fn api(fx: &Fx, token: &str, method: &str, path: &str, body: &str) -> Resp {
    req(fx, None, method, path, &bearer(token), body).await
}

#[tokio::test]
async fn register_then_login_issues_cookie_and_bearer_session() {
    let fx = start(HubConfig::default()).await;
    let mut sp = SoftPasskey::new(true);
    let r = register(&fx, &mut sp, "laptop").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["name"], "laptop");

    let r = login_at(&fx, &mut sp, origin(&fx)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let cookie = r.header("set-cookie").expect("session cookie");
    for part in ["HttpOnly", "Secure", "SameSite=Strict", "Path=/"] {
        assert!(cookie.contains(part), "{cookie}");
    }
    let token = r.json()["token"].as_str().unwrap().to_owned();
    assert!(cookie.starts_with(&format!("ulak_session={token}")));
    assert!(r.json()["expires_ms"].as_u64().unwrap() > now());

    assert_eq!(
        api(&fx, &token, "GET", "/api/devices", "").await.status,
        200
    );
    let c = [("Cookie", format!("ulak_session={token}"))];
    assert_eq!(
        req(&fx, None, "GET", "/api/devices", &c, "").await.status,
        200
    );
    assert_eq!(
        req(&fx, None, "GET", "/api/devices", &[], "").await.status,
        401
    );
    assert_eq!(
        api(&fx, "nope", "GET", "/api/devices", "").await.status,
        401
    );
}

#[tokio::test]
async fn several_passkeys_all_log_in_and_challenges_are_single_use() {
    let fx = start(HubConfig::default()).await;
    let (mut a, mut b) = (SoftPasskey::new(true), SoftPasskey::new(true));
    assert_eq!(register(&fx, &mut a, "one").await.status, 200);
    assert_eq!(register(&fx, &mut b, "two").await.status, 200);
    login(&fx, &mut a).await;
    let t = login(&fx, &mut b).await;
    let list = api(&fx, &t, "GET", "/api/passkeys", "").await.json();
    assert_eq!(list["passkeys"].as_array().unwrap().len(), 2);
    assert!(list["passkeys"][0].get("credential").is_none());

    // Replaying a finish with a spent challenge id fails.
    let s = post(&fx, "/auth/login/start", json!({})).await.json();
    let opts: RequestChallengeResponse = serde_json::from_value(s["options"].clone()).unwrap();
    let cred = a.do_authentication(origin(&fx), opts).unwrap();
    let body = json!({"challenge_id": s["challenge_id"], "credential": cred});
    assert_eq!(
        post(&fx, "/auth/login/finish", body.clone()).await.status,
        200
    );
    assert_eq!(post(&fx, "/auth/login/finish", body).await.status, 400);
}

#[tokio::test]
async fn bad_wrong_kind_and_expired_otps_are_rejected() {
    let fx = start(HubConfig::default()).await;
    let mut sp = SoftPasskey::new(true);
    let r = register_at(&fx, &mut sp, "AAAAAAAA", "x", origin(&fx)).await;
    assert_eq!(r.status, 403);
    let node = fx.hub.enroller().begin(DeviceKind::Node, now()).otp;
    let r = register_at(&fx, &mut sp, node.expose(), "x", origin(&fx)).await;
    assert_eq!(r.status, 403);
    let old = fx.hub.enroller().begin(DeviceKind::Browser, 0).otp;
    let r = register_at(&fx, &mut sp, old.expose(), "x", origin(&fx)).await;
    assert_eq!(r.status, 403);
    // A device code is refused with a hint and is not spent.
    let r = register_at(&fx, &mut sp, node.expose(), "x", origin(&fx)).await;
    assert!(r.body.contains("for a device"), "{}", r.body);
    assert!(matches!(
        fx.hub.enroller().consume(&node, now()),
        Err(EnrollError::DeviceCode)
    ));
    // Nothing was registered.
    assert!(fx.hub.store().list_passkeys().unwrap().is_empty());
    // A fresh code still works (begin() clears the lockout), and only once.
    let otp = browser_otp(&fx);
    assert_eq!(
        register_at(&fx, &mut sp, &otp, "ok", origin(&fx))
            .await
            .status,
        200
    );
    let r = register_at(&fx, &mut SoftPasskey::new(true), &otp, "again", origin(&fx)).await;
    assert_eq!(r.status, 403);
}

#[tokio::test]
async fn wrong_origin_is_rejected_for_register_and_login() {
    let fx = start(HubConfig::default()).await;
    // Same rp id (so the authenticator signs) but another port: only the hub can tell.
    let evil = Url::parse("https://localhost:1").unwrap();
    let otp = browser_otp(&fx);
    let r = register_at(&fx, &mut SoftPasskey::new(true), &otp, "x", evil.clone()).await;
    assert_eq!(r.status, 400, "{}", r.body);
    assert!(fx.hub.store().list_passkeys().unwrap().is_empty());

    let mut sp = SoftPasskey::new(true);
    assert_eq!(register(&fx, &mut sp, "ok").await.status, 200);
    let r = login_at(&fx, &mut sp, evil).await;
    assert_eq!(r.status, 401, "{}", r.body);
    assert!(r.header("set-cookie").is_none());
}

#[tokio::test]
async fn login_without_any_passkey_is_refused() {
    let fx = start(HubConfig::default()).await;
    assert_eq!(post(&fx, "/auth/login/start", json!({})).await.status, 403);
}

#[tokio::test]
async fn sessions_expire() {
    let fx = start(HubConfig {
        session_ttl_ms: 300,
        ..HubConfig::default()
    })
    .await;
    let mut sp = SoftPasskey::new(true);
    register(&fx, &mut sp, "p").await;
    let t = login(&fx, &mut sp).await;
    assert_eq!(api(&fx, &t, "GET", "/api/devices", "").await.status, 200);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(api(&fx, &t, "GET", "/api/devices", "").await.status, 401);
}

async fn ws(
    fx: &Fx,
    path: &str,
    cookie: Option<&str>,
) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let url = format!("wss://{}{path}", fx.run.local_addr());
    let mut r = url.into_client_request().unwrap();
    if let Some(c) = cookie {
        r.headers_mut().insert("Cookie", c.parse().unwrap());
    }
    let c = Connector::Rustls(Arc::new(tls(&fx.ca_pem, None)));
    connect_async_tls_with_config(r, None, false, Some(c))
        .await
        .map(|x| x.0)
}
type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn snapshot(w: &mut Ws) {
    let m = tokio::time::timeout(Duration::from_secs(3), w.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Text(t) = m else { panic!("{m:?}") };
    assert!(matches!(
        decode::<HubToUi>(&t).unwrap(),
        HubToUi::Snapshot { .. }
    ));
    let m = w.next().await.unwrap().unwrap();
    let Message::Text(t) = m else { panic!("{m:?}") };
    assert!(matches!(
        decode::<HubToUi>(&t).unwrap(),
        HubToUi::Blockers { .. }
    ));
    let m = w.next().await.unwrap().unwrap();
    let Message::Text(t) = m else { panic!("{m:?}") };
    assert!(matches!(
        decode::<HubToUi>(&t).unwrap(),
        HubToUi::HubVersion(_)
    ));
    let m = w.next().await.unwrap().unwrap();
    let Message::Text(t) = m else { panic!("{m:?}") };
    assert!(matches!(
        decode::<HubToUi>(&t).unwrap(),
        HubToUi::HubConfig { .. }
    ));
}

#[tokio::test]
async fn ws_ui_accepts_a_session_and_revoking_the_passkey_closes_it() {
    let fx = start(HubConfig::default()).await;
    let mut sp = SoftPasskey::new(true);
    let id = register(&fx, &mut sp, "p").await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let t = login(&fx, &mut sp).await;

    assert!(ws(&fx, "/ws/ui", None).await.is_err());
    assert!(ws(&fx, "/ws/ui?token=bogus", None).await.is_err());
    let mut by_query = ws(&fx, &format!("/ws/ui?token={t}"), None).await.unwrap();
    snapshot(&mut by_query).await;
    let mut by_cookie = ws(&fx, "/ws/ui", Some(&format!("ulak_session={t}")))
        .await
        .unwrap();
    snapshot(&mut by_cookie).await;
    // Same capabilities as a cert UI: it can ask for the device list.
    let list = encode(&UiToHub::ListDevices).unwrap();
    futures_util::SinkExt::send(&mut by_query, Message::Text(list.into()))
        .await
        .unwrap();
    let m = by_query.next().await.unwrap().unwrap();
    assert!(matches!(
        decode::<HubToUi>(&m.into_text().unwrap()).unwrap(),
        HubToUi::Devices { .. }
    ));

    let r = api(&fx, &t, "POST", &format!("/api/passkeys/{id}/revoke"), "").await;
    assert_eq!(r.status, 204, "{}", r.body);
    for w in [&mut by_query, &mut by_cookie] {
        let end = tokio::time::timeout(Duration::from_secs(3), async {
            while let Some(Ok(m)) = w.next().await {
                if m.is_close() {
                    return;
                }
            }
        })
        .await;
        assert!(end.is_ok(), "socket stayed open after revoke");
    }
    // The revoked passkey is gone for sessions and for new logins.
    assert_eq!(api(&fx, &t, "GET", "/api/devices", "").await.status, 401);
    assert!(ws(&fx, &format!("/ws/ui?token={t}"), None).await.is_err());
    assert_eq!(post(&fx, "/auth/login/start", json!({})).await.status, 403);
}

#[tokio::test]
async fn admin_api_lists_issues_otps_and_revokes() {
    let fx = start(HubConfig::default()).await;
    let mut sp = SoftPasskey::new(true);
    register(&fx, &mut sp, "p").await;
    let t = login(&fx, &mut sp).await;
    for (m, p) in [
        ("GET", "/api/devices"),
        ("POST", "/api/devices/otp"),
        ("POST", "/api/devices/x/revoke"),
        ("GET", "/api/passkeys"),
        ("POST", "/api/passkeys/pk-00000000/revoke"),
    ] {
        assert_eq!(req(&fx, None, m, p, &[], "{}").await.status, 401, "{m} {p}");
    }

    let o = api(&fx, &t, "POST", "/api/devices/otp", r#"{"kind":"node"}"#).await;
    assert_eq!(o.status, 200, "{}", o.body);
    let o = o.json();
    assert_eq!(o["kind"], "node");
    assert_eq!(o["otp"].as_str().unwrap().len(), 8);
    assert_eq!(o["hub_fingerprint"].as_str().unwrap().len(), 64);
    assert!(o["expires_ms"].as_u64().unwrap() > now());
    let bad = api(&fx, &t, "POST", "/api/devices/otp", r#"{"kind":"toaster"}"#).await;
    assert_eq!(bad.status, 400);

    // Enroll a node with that code, list it, revoke it.
    let key = rcgen::KeyPair::generate().unwrap();
    let er = EnrollRequest::new(
        Otp::new(o["otp"].as_str().unwrap()).unwrap(),
        key.public_key_pem(),
        "box".into(),
    )
    .unwrap();
    let e = post(&fx, "/enroll", serde_json::to_value(&er).unwrap()).await;
    assert_eq!(e.status, 200, "{}", e.body);
    let list = api(&fx, &t, "GET", "/api/devices", "").await.json();
    assert_eq!(list["devices"][0]["id"], "box");
    assert_eq!(list["devices"][0]["revoked"], false);
    assert_eq!(
        api(&fx, &t, "POST", "/api/devices/box/revoke", "")
            .await
            .status,
        204
    );
    assert_eq!(
        api(&fx, &t, "POST", "/api/devices/ghost/revoke", "")
            .await
            .status,
        404
    );
    let list = api(&fx, &t, "GET", "/api/devices", "").await.json();
    assert_eq!(list["devices"][0]["revoked"], true);
    assert_eq!(
        api(&fx, &t, "POST", "/api/passkeys/pk-00000000/revoke", "")
            .await
            .status,
        404
    );
}

#[tokio::test]
async fn admin_api_accepts_any_active_device_cert_but_not_a_revoked_one() {
    let fx = start(HubConfig::default()).await;
    let enroll = |kind| {
        let fx = &fx;
        async move {
            let o = fx.hub.enroller().begin(kind, now());
            let key = rcgen::KeyPair::generate().unwrap();
            let er = EnrollRequest::new(o.otp, key.public_key_pem(), "dev".into()).unwrap();
            let e = post(fx, "/enroll", serde_json::to_value(&er).unwrap()).await;
            let resp: EnrollResponse = decode(&e.body).unwrap();
            Identity {
                cert: resp.cert,
                key: key.serialize_pem(),
            }
        }
    };
    let ui = enroll(DeviceKind::Ui).await;
    let node = enroll(DeviceKind::Node).await;
    assert_eq!(
        req(&fx, Some(&ui), "GET", "/api/devices", &[], "")
            .await
            .status,
        200
    );
    assert_eq!(
        req(&fx, Some(&node), "GET", "/api/devices", &[], "")
            .await
            .status,
        200
    );
}

fn web_dir() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("ulak-site-{}", uuid::Uuid::now_v7()));
    let dir = root.join("web");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(root.join("secret.txt"), "TOP-SECRET").unwrap();
    std::fs::write(dir.join("index.html"), "<h1>ulak</h1>").unwrap();
    std::fs::write(dir.join("ulak_bg.wasm"), b"\0asm\x01\0\0\0").unwrap();
    std::fs::write(dir.join("ulak.js"), "export {}").unwrap();
    std::fs::write(dir.join("sub/page.css"), "a{}").unwrap();
    dir
}

#[tokio::test]
async fn static_files_have_types_security_headers_and_fallback() {
    let fx = start(HubConfig {
        web_dir: Some(web_dir()),
        ..HubConfig::default()
    })
    .await;
    let get = |p: &'static str| req(&fx, None, "GET", p, &[], "");
    let r = get("/").await;
    assert_eq!((r.status, r.body.as_str()), (200, "<h1>ulak</h1>"));
    assert!(r.header("content-type").unwrap().starts_with("text/html"));
    let csp = r.header("content-security-policy").unwrap();
    assert!(csp.contains("default-src 'self'") && csp.contains("frame-ancestors 'none'"));
    assert!(csp.contains("'wasm-unsafe-eval'"));
    // Strict: Rune's styles are made CSP-safe in the bundle (scripts/patch-bridge.py), not allowed here.
    assert!(
        !csp.contains("unsafe-inline") && !csp.contains("style-src-attr"),
        "{csp}"
    );
    assert_eq!(r.header("x-content-type-options").unwrap(), "nosniff");
    assert_eq!(r.header("referrer-policy").unwrap(), "no-referrer");
    assert_eq!(
        get("/ulak_bg.wasm").await.header("content-type").unwrap(),
        "application/wasm"
    );
    assert!(get("/ulak.js")
        .await
        .header("content-type")
        .unwrap()
        .contains("javascript"));
    assert!(get("/sub/page.css")
        .await
        .header("content-type")
        .unwrap()
        .starts_with("text/css"));
    // SPA fallback for extension-less routes, a plain 404 for missing assets.
    assert_eq!(get("/some/route").await.body, "<h1>ulak</h1>");
    assert_eq!(get("/missing.js").await.status, 404);
    // No directory listing.
    assert_eq!(get("/sub/").await.body, "<h1>ulak</h1>");
    // API paths do not fall into the SPA.
    assert_eq!(
        req(&fx, None, "POST", "/nothing", &[], "").await.status,
        404
    );
    // Security headers also on API errors.
    let r = req(&fx, None, "GET", "/api/devices", &[], "").await;
    assert_eq!(r.header("x-content-type-options").unwrap(), "nosniff");
}

#[tokio::test]
async fn static_serving_blocks_path_traversal() {
    let fx = start(HubConfig {
        web_dir: Some(web_dir()),
        ..HubConfig::default()
    })
    .await;
    for p in [
        "/../secret.txt",
        "/sub/../../secret.txt",
        "/%2e%2e/secret.txt",
        "/..%2fsecret.txt",
        "/%2e%2e%2fsecret.txt",
        "/sub/%2e%2e/%2e%2e/secret.txt",
        "/..\\secret.txt",
        "//etc/passwd",
        "/%00",
    ] {
        let r = req(&fx, None, "GET", p, &[], "").await;
        assert!(
            !r.body.contains("TOP-SECRET") && !r.body.contains("root:"),
            "{p}"
        );
        assert!(
            r.status == 404 || r.status == 400 || r.body == "<h1>ulak</h1>",
            "{p}: {}",
            r.status
        );
    }
}

#[tokio::test]
async fn without_web_dir_root_is_404() {
    let fx = start(HubConfig::default()).await;
    assert_eq!(req(&fx, None, "GET", "/", &[], "").await.status, 404);
}

#[tokio::test]
async fn logout_ends_only_that_session_and_closes_its_sockets() {
    let fx = start(HubConfig::default()).await;
    let mut sp = SoftPasskey::new(true);
    register(&fx, &mut sp, "p").await;
    let (a, b) = (login(&fx, &mut sp).await, login(&fx, &mut sp).await);
    let mut wa = ws(&fx, &format!("/ws/ui?token={a}"), None).await.unwrap();
    snapshot(&mut wa).await;
    let mut wb = ws(&fx, &format!("/ws/ui?token={b}"), None).await.unwrap();
    snapshot(&mut wb).await;

    assert_eq!(
        req(&fx, None, "POST", "/auth/logout", &[], "").await.status,
        401
    );
    let r = api(&fx, &a, "POST", "/auth/logout", "").await;
    assert_eq!(r.status, 204, "{}", r.body);
    assert!(r.header("set-cookie").unwrap().contains("Max-Age=0"));
    let end = tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(Ok(m)) = wa.next().await {
            if m.is_close() {
                return;
            }
        }
    })
    .await;
    assert!(end.is_ok(), "socket stayed open after logout");
    assert_eq!(api(&fx, &a, "GET", "/api/devices", "").await.status, 401);
    // The other session of the same passkey is untouched.
    assert_eq!(api(&fx, &b, "GET", "/api/devices", "").await.status, 200);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), wb.next())
            .await
            .is_err(),
        "other session's socket was closed"
    );
}

fn etag_of(r: &Resp) -> String {
    r.header("etag").expect("etag")
}

#[tokio::test]
async fn static_files_have_etag_head_and_304() {
    let fx = start(HubConfig {
        web_dir: Some(web_dir()),
        ..HubConfig::default()
    })
    .await;
    let r = req(&fx, None, "GET", "/ulak.js", &[], "").await;
    let tag = etag_of(&r);
    assert!(tag.starts_with('"') && tag.ends_with('"'), "{tag}");
    // Same bytes, same tag; other bytes, other tag.
    assert_eq!(
        etag_of(&req(&fx, None, "GET", "/ulak.js", &[], "").await),
        tag
    );
    assert_ne!(
        etag_of(&req(&fx, None, "GET", "/sub/page.css", &[], "").await),
        tag
    );

    for inm in [
        tag.clone(),
        format!("\"x\", {tag}"),
        format!("W/{tag}"),
        "*".into(),
    ] {
        let r = req(
            &fx,
            None,
            "GET",
            "/ulak.js",
            &[("If-None-Match", inm.clone())],
            "",
        )
        .await;
        assert_eq!((r.status, r.body.as_str()), (304, ""), "{inm}");
        assert_eq!(etag_of(&r), tag);
        assert!(r.header("content-security-policy").is_some());
    }
    let r = req(
        &fx,
        None,
        "GET",
        "/ulak.js",
        &[("If-None-Match", "\"nope\"".into())],
        "",
    )
    .await;
    assert_eq!((r.status, r.body.as_str()), (200, "export {}"));

    let r = req(&fx, None, "HEAD", "/ulak.js", &[], "").await;
    assert_eq!((r.status, r.body.as_str()), (200, ""));
    assert_eq!(etag_of(&r), tag);
    assert_eq!(r.header("content-length").unwrap(), "9");
    assert!(r.header("content-type").unwrap().contains("javascript"));
    let r = req(&fx, None, "HEAD", "/ulak.js", &[("If-None-Match", tag)], "").await;
    assert_eq!(r.status, 304);
    // Other methods stay refused.
    assert_eq!(
        req(&fx, None, "POST", "/ulak.js", &[], "").await.status,
        404
    );
}

#[tokio::test]
async fn static_serving_refuses_symlinks_that_escape_the_dir() {
    let dir = web_dir();
    let outside = dir.parent().unwrap().join("secret.txt");
    std::os::unix::fs::symlink(&outside, dir.join("leak.txt")).unwrap();
    std::os::unix::fs::symlink(dir.parent().unwrap(), dir.join("up")).unwrap();
    // A link that stays inside is fine.
    std::os::unix::fs::symlink(dir.join("ulak.js"), dir.join("alias.js")).unwrap();
    let fx = start(HubConfig {
        web_dir: Some(dir),
        ..HubConfig::default()
    })
    .await;
    for p in ["/leak.txt", "/up/secret.txt"] {
        let r = req(&fx, None, "GET", p, &[], "").await;
        assert!(!r.body.contains("TOP-SECRET"), "{p}");
        assert!(
            r.status == 404 || r.body == "<h1>ulak</h1>",
            "{p}: {}",
            r.status
        );
    }
    assert_eq!(
        req(&fx, None, "GET", "/leak.txt", &[], "").await.status,
        404
    );
    assert_eq!(
        req(&fx, None, "GET", "/alias.js", &[], "").await.body,
        "export {}"
    );
}
