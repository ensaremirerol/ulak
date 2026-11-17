//! Browser access: passkey login and sessions, the admin API, and static serving of the web build.
use super::hub::{now_ms, Hub, Peer};
use crate::*;
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Extension, Json,
};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::json;
use std::sync::Arc;
use ulak_proto::*;
use webauthn_rs::prelude::{
    PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential,
    Uuid,
};

const COOKIE: &str = "ulak_session";
const CHALLENGE_TTL_MS: u64 = 300_000;
const MAX_CHALLENGES: usize = 256;

/// Pending ceremonies by random id: expire after 5 minutes, single use, bounded.
pub(crate) struct Challenges<T>(std::sync::Mutex<std::collections::HashMap<String, (u64, T)>>);

impl<T> Default for Challenges<T> {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl<T> Challenges<T> {
    fn map(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, (u64, T)>> {
        self.0
            .lock()
            .expect("challenge map lock is never held across a panic")
    }

    /// `None` when too many are pending.
    fn put(&self, v: T, now: u64) -> Option<String> {
        let mut m = self.map();
        m.retain(|_, (exp, _)| *exp >= now);
        if m.len() >= MAX_CHALLENGES {
            return None;
        }
        let mut raw = [0u8; 16];
        rand::fill(&mut raw[..]);
        let id = crate::ca::hex(&raw);
        m.insert(id.clone(), (now + CHALLENGE_TTL_MS, v));
        Some(id)
    }

    /// Removes the entry whether or not it is still valid.
    fn take(&self, id: &str, now: u64) -> Option<T> {
        let (exp, v) = self.map().remove(id)?;
        (exp >= now).then_some(v)
    }

    pub(crate) fn sweep(&self, now: u64) {
        self.map().retain(|_, (exp, _)| *exp >= now);
    }
}
/// Single operator: every passkey belongs to this one WebAuthn user.
const OPERATOR: Uuid = Uuid::from_u128(0x756c_616b_0000_4000_8000_0000_0000_0001);

pub(crate) enum Challenge {
    Register(PasskeyRegistration, String),
    Login(PasskeyAuthentication),
}

fn parse<T: DeserializeOwned>(b: &Bytes) -> Result<T, StatusCode> {
    serde_json::from_slice(b).map_err(|_| StatusCode::BAD_REQUEST)
}

fn internal<E>(_: E) -> StatusCode {
    StatusCode::INTERNAL_SERVER_ERROR
}

pub(crate) fn token_of(headers: &HeaderMap, query: Option<&str>) -> Option<String> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let cookie = || {
        headers
            .get_all(header::COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(';'))
            .find_map(|c| c.trim().strip_prefix("ulak_session="))
    };
    let query = || query?.split('&').find_map(|p| p.strip_prefix("token="));
    bearer.or_else(cookie).or_else(query).map(str::to_owned)
}

impl Hub {
    /// Who is calling the UI surface: a live session (bearer, cookie or `?token=`) or a UI client
    /// cert. The id is the passkey id or the device id; revoking either closes its sockets.
    pub(crate) fn ui_caller(
        &self,
        peer: &Peer,
        headers: &HeaderMap,
        query: Option<&str>,
    ) -> Result<NodeId, StatusCode> {
        if let Some(t) = token_of(headers, query) {
            match self.store.verify_session(&t, now_ms()) {
                Ok(id) => return Ok(id),
                Err(AuthError::Invalid) => {}
                Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
            }
        }
        self.authorize(peer)
    }

    fn wa(&self) -> Result<&webauthn_rs::Webauthn, StatusCode> {
        self.webauthn.get().ok_or(StatusCode::SERVICE_UNAVAILABLE)
    }

    fn put_challenge(&self, c: Challenge, now: u64) -> Result<String, StatusCode> {
        self.challenges
            .put(c, now)
            .ok_or(StatusCode::TOO_MANY_REQUESTS)
    }

    /// Single use: the challenge is gone after the first finish, success or not.
    fn take_challenge(&self, id: &str, now: u64) -> Result<Challenge, StatusCode> {
        self.challenges.take(id, now).ok_or(StatusCode::BAD_REQUEST)
    }
}

#[derive(Deserialize)]
struct RegisterStart {
    otp: String,
    name: String,
}

pub(crate) async fn register_start(
    State(h): State<Arc<Hub>>,
    Extension(peer): Extension<Peer>,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let now = now_ms();
    if !h.rate_ok(peer.ip, now) {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    let req: RegisterStart = parse(&body)?;
    let name = req.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let otp = Otp::new(&req.otp).map_err(|_| StatusCode::FORBIDDEN)?;
    match h.enroller().consume(&otp, now) {
        Ok(DeviceKind::Browser) => {}
        Ok(_) | Err(EnrollError::BadOtp) => return Err(StatusCode::FORBIDDEN),
        Err(e @ EnrollError::DeviceCode) => {
            return Ok((StatusCode::FORBIDDEN, e.to_string()).into_response())
        }
        Err(EnrollError::Locked) => return Err(StatusCode::TOO_MANY_REQUESTS),
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
    let exclude: Vec<_> = h
        .store
        .active_passkeys()
        .map_err(internal)?
        .iter()
        .map(|(_, k)| k.cred_id().clone())
        .collect();
    let (options, state) = h
        .wa()?
        .start_passkey_registration(OPERATOR, "operator", &name, Some(exclude))
        .map_err(internal)?;
    let challenge_id = h.put_challenge(Challenge::Register(state, name), now)?;
    Ok(Json(json!({"challenge_id": challenge_id, "options": options})).into_response())
}

#[derive(Deserialize)]
struct RegisterFinish {
    challenge_id: String,
    credential: RegisterPublicKeyCredential,
}

pub(crate) async fn register_finish(
    State(h): State<Arc<Hub>>,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let now = now_ms();
    let req: RegisterFinish = parse(&body)?;
    let Challenge::Register(state, name) = h.take_challenge(&req.challenge_id, now)? else {
        return Err(StatusCode::BAD_REQUEST);
    };
    let key = h
        .wa()?
        .finish_passkey_registration(&req.credential, &state)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let id = h.store.add_passkey(&name, &key, now).map_err(internal)?;
    Ok(Json(json!({"id": id, "name": name})).into_response())
}

pub(crate) async fn login_start(State(h): State<Arc<Hub>>) -> Result<Response, StatusCode> {
    let keys: Vec<_> = h
        .store
        .active_passkeys()
        .map_err(internal)?
        .into_iter()
        .map(|(_, k)| k)
        .collect();
    if keys.is_empty() {
        return Err(StatusCode::FORBIDDEN);
    }
    let (options, state) = h
        .wa()?
        .start_passkey_authentication(&keys)
        .map_err(internal)?;
    let challenge_id = h.put_challenge(Challenge::Login(state), now_ms())?;
    Ok(Json(json!({"challenge_id": challenge_id, "options": options})).into_response())
}

#[derive(Deserialize)]
struct LoginFinish {
    challenge_id: String,
    credential: PublicKeyCredential,
}

pub(crate) async fn login_finish(
    State(h): State<Arc<Hub>>,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let now = now_ms();
    let req: LoginFinish = parse(&body)?;
    let Challenge::Login(state) = h.take_challenge(&req.challenge_id, now)? else {
        return Err(StatusCode::BAD_REQUEST);
    };
    let res = h
        .wa()?
        .finish_passkey_authentication(&req.credential, &state)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let mut keys = h.store.active_passkeys().map_err(internal)?;
    let (id, key) = keys
        .iter_mut()
        .find(|(_, k)| k.cred_id() == res.cred_id())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if key.update_credential(&res) == Some(true) {
        h.store.save_passkey(id, key).map_err(internal)?;
    }
    let ttl = h.cfg.session_ttl_ms;
    let token = h.store.issue_session(id, ttl, now).map_err(internal)?;
    let cookie = format!(
        "{COOKIE}={token}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age={}",
        ttl / 1000
    );
    let mut r = Json(json!({"token": token, "expires_ms": now + ttl})).into_response();
    r.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(internal)?,
    );
    Ok(r)
}

/// Socket-kick key of one session (the passkey id would close the passkey's other sessions too).
pub(crate) fn session_key(token: &str) -> NodeId {
    NodeId::new(&format!("s-{}", &crate::passkeys::hash(token)[..16]))
        .expect("s- plus hex is a valid node id")
}

pub(crate) async fn logout(
    State(h): State<Arc<Hub>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let token = token_of(&headers, None).ok_or(StatusCode::UNAUTHORIZED)?;
    match h.store.verify_session(&token, now_ms()) {
        Ok(_) => {}
        Err(AuthError::Invalid) => return Err(StatusCode::UNAUTHORIZED),
        Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
    h.store.end_session(&token).map_err(internal)?;
    // No open socket is fine.
    let _ = h.kick.send(session_key(&token));
    Ok((
        StatusCode::NO_CONTENT,
        [(
            header::SET_COOKIE,
            format!("{COOKIE}=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0"),
        )],
    )
        .into_response())
}

// ---- admin API ----

pub(crate) async fn version(
    State(h): State<Arc<Hub>>,
    Extension(p): Extension<Peer>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    h.ui_caller(&p, &headers, None)?;
    Ok(Json(crate::version_info()).into_response())
}

pub(crate) async fn devices(
    State(h): State<Arc<Hub>>,
    Extension(p): Extension<Peer>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    h.ui_caller(&p, &headers, None)?;
    let list: Vec<DeviceInfo> = Devices::list(&*h.store)
        .map_err(internal)?
        .iter()
        .map(Into::into)
        .collect();
    Ok(Json(json!({"devices": list})).into_response())
}

#[derive(Deserialize)]
struct OtpReq {
    kind: DeviceKind,
}

pub(crate) async fn device_otp(
    State(h): State<Arc<Hub>>,
    Extension(p): Extension<Peer>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    h.ui_caller(&p, &headers, None)?;
    let OtpReq { kind } = parse(&body)?;
    let o = h.enroller().begin(kind, now_ms());
    Ok(Json(json!({
        "kind": kind,
        "otp": o.otp.expose(),
        "hub_fingerprint": o.hub_fingerprint,
        "expires_ms": o.expires_ms,
    }))
    .into_response())
}

pub(crate) async fn device_revoke(
    State(h): State<Arc<Hub>>,
    Extension(p): Extension<Peer>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    h.ui_caller(&p, &headers, None)?;
    let id = NodeId::new(&id).map_err(|_| StatusCode::NOT_FOUND)?;
    match h.revoke_device(&id, now_ms()) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(DeviceError::NotFound(_)) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub(crate) async fn passkeys(
    State(h): State<Arc<Hub>>,
    Extension(p): Extension<Peer>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    h.ui_caller(&p, &headers, None)?;
    let list = h.store.list_passkeys().map_err(internal)?;
    Ok(Json(json!({"passkeys": list})).into_response())
}

pub(crate) async fn passkey_revoke(
    State(h): State<Arc<Hub>>,
    Extension(p): Extension<Peer>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    h.ui_caller(&p, &headers, None)?;
    let id = NodeId::new(&id).map_err(|_| StatusCode::NOT_FOUND)?;
    match h.revoke_passkey(&id, now_ms()) {
        Ok(()) => Ok(StatusCode::NO_CONTENT),
        Err(AuthError::Invalid) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

// ---- static files ----

const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; \
img-src 'self' data:; connect-src 'self' wss:; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

pub(crate) async fn security_headers(mut r: Response) -> Response {
    let h = r.headers_mut();
    for (k, v) in [
        ("content-security-policy", CSP),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("x-frame-options", "DENY"),
        ("strict-transport-security", "max-age=31536000"),
    ] {
        h.insert(k, HeaderValue::from_static(v));
    }
    r
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|x| x.1) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("json" | "webmanifest") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("webp") => "image/webp",
        Some("map") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Reads `path` only if it resolves (symlinks included) to a file inside the canonical `root`.
async fn read_inside(root: &std::path::Path, path: &std::path::Path) -> Option<Vec<u8>> {
    let real = tokio::fs::canonicalize(path).await.ok()?;
    if !real.starts_with(root) {
        return None;
    }
    tokio::fs::read(real).await.ok()
}

fn etag(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("\"{}\"", &crate::ca::hex(&Sha256::digest(bytes))[..32])
}

fn matches_etag(headers: &HeaderMap, tag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|t| t == "*" || t.strip_prefix("W/").unwrap_or(t) == tag)
}

/// Serves `ULAK_WEB_DIR` (GET, HEAD, ETag/304). The path is never percent-decoded, `..`, `\` and
/// NUL segments are refused, and a symlink that leaves the directory is a 404.
pub(crate) async fn static_file(
    State(h): State<Arc<Hub>>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let Some(dir) = h
        .cfg
        .web_dir
        .as_ref()
        .filter(|_| method == Method::GET || method == Method::HEAD)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(root) = tokio::fs::canonicalize(dir).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let segs: Vec<&str> = uri.path().split('/').filter(|s| !s.is_empty()).collect();
    if segs
        .iter()
        .any(|s| *s == "." || *s == ".." || s.contains(['\\', '\0']))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut file = dir.clone();
    file.extend(&segs);
    let mut name = segs.last().copied().unwrap_or("index.html");
    if segs.is_empty() {
        file.push(name);
    }
    let mut bytes = read_inside(&root, &file).await;
    // Single-page app: extension-less routes get index.html; missing assets stay 404.
    if bytes.is_none() && !name.contains('.') {
        name = "index.html";
        bytes = read_inside(&root, &dir.join(name)).await;
    }
    let Some(b) = bytes else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let tag = etag(&b);
    let common = [
        (header::ETAG, tag.clone()),
        (header::CACHE_CONTROL, "no-cache".to_owned()),
    ];
    if matches_etag(&headers, &tag) {
        return (StatusCode::NOT_MODIFIED, common).into_response();
    }
    let ctype = (header::CONTENT_TYPE, content_type(name).to_owned());
    if method == Method::HEAD {
        let len = (header::CONTENT_LENGTH, b.len().to_string());
        return (common, [ctype, len]).into_response();
    }
    (common, [ctype], b).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenges_expire_are_single_use_bounded_and_swept() {
        let c = Challenges::<u8>::default();
        let id = c.put(1, 1000).unwrap();
        assert_eq!(c.take(&id, 1000 + CHALLENGE_TTL_MS), Some(1));
        assert_eq!(c.take(&id, 1000), None, "single use");
        let id = c.put(2, 1000).unwrap();
        assert_eq!(c.take(&id, 1001 + CHALLENGE_TTL_MS), None, "expired");
        assert_eq!(c.take(&id, 1000), None, "an expired take still consumes");
        c.put(3, 0).unwrap();
        c.put(4, 0).unwrap();
        c.sweep(CHALLENGE_TTL_MS + 1);
        assert_eq!(c.map().len(), 0);
        for _ in 0..MAX_CHALLENGES {
            c.put(0, 10).unwrap();
        }
        assert!(c.put(0, 10).is_none(), "bounded");
        // Expired entries make room.
        assert!(c.put(0, 10 + CHALLENGE_TTL_MS + 1).is_some());
    }
}

#[cfg(test)]
mod content_type_tests {
    use super::content_type;

    #[test]
    fn fonts_and_wasm_have_their_own_types() {
        assert_eq!(content_type("Inter.woff2"), "font/woff2");
        assert_eq!(content_type("ulak_bg.wasm"), "application/wasm");
        assert_eq!(content_type("manifest.json"), "application/json");
        assert_eq!(content_type("blob.bin"), "application/octet-stream");
    }
}
