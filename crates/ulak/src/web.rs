//! Browser build (wasm32): entry point, the browser-WebSocket `Transport`, and the thin
//! WebAuthn / fetch glue. All decisions live in `web_login` (pure, natively tested).
//! No mTLS here: the hub authenticates the socket with the `ulak_session` cookie (same
//! origin), or `?token=` when the login response gave us a token.
use crate::{
    app::{poll, App},
    model::Event,
    net::{run_session, EventTx, Exit, NetError, NetHandle, Transport},
    scenes,
    view::{on_key, view},
    web_login::{
        login_json, post, prepare_options, register_json, Ceremony, LoginMsg, Prepared,
        RawCredential, Seg,
    },
};
use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use rune::{AppSpec, Waker};
use serde_json::{json, Value};
use std::{cell::RefCell, rc::Rc, sync::mpsc};
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    Notify,
};
use ulak_proto::{decode, encode, UiToHub};
use wasm_bindgen::{closure::Closure, prelude::wasm_bindgen, JsCast, JsValue};
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    CredentialCreationOptions, CredentialRequestOptions, MessageEvent, Request, RequestInit,
    Response, WebSocket,
};

thread_local! {
    static WAKE: RefCell<Option<Waker>> = const { RefCell::new(None) };
    static TOKEN: RefCell<Option<String>> = const { RefCell::new(None) };
    static KICK: Rc<Notify> = Rc::new(Notify::new());
}

fn wake() {
    WAKE.with(|w| {
        if let Some(f) = &*w.borrow() {
            f();
        }
    });
}

/// Tell the login screen something and wake the UI to show it.
fn say(m: LoginMsg) {
    post(m);
    wake();
}

fn js_err(e: JsValue) -> String {
    e.as_string()
        .or_else(|| {
            Reflect::get(&e, &"message".into())
                .ok()
                .and_then(|m| m.as_string())
        })
        .unwrap_or_else(|| format!("{e:?}"))
}

#[wasm_bindgen(start)]
pub fn start() {
    register_service_worker();
    // Dev convenience (plain-http origins cannot keep the Secure cookie): `/?token=<session>`.
    let q = web_sys::window().and_then(|w| w.location().search().ok());
    let tok = q.as_deref().and_then(|q| {
        q.trim_start_matches('?')
            .split('&')
            .find_map(|kv| kv.strip_prefix("token="))
    });
    TOKEN.with(|t| *t.borrow_mut() = tok.map(str::to_owned));
    rune::run(AppSpec::<App> {
        scenes: &[("mux", scenes::mux), ("busy", scenes::busy)],
        on_key: Some(on_key),
        families: ("Inter", "ui-monospace"),
        setup: Some(setup),
        poll: Some(poll),
        ..AppSpec::new("ulak", (1000.0, 650.0), App::default, view)
    });
}

fn register_service_worker() {
    let Some(w) = web_sys::window() else { return };
    let sw = Reflect::get(&w.navigator(), &"serviceWorker".into()).unwrap_or(JsValue::UNDEFINED);
    if sw.is_undefined() {
        return;
    }
    if let Ok(f) = Reflect::get(&sw, &"register".into()).and_then(|f| f.dyn_into::<Function>()) {
        let _ = f.call1(&sw, &"/sw.js".into());
    }
}

/// `AppSpec::setup` on the web: keep the waker and start the socket loop.
pub fn setup(app: &mut App, w: Waker) {
    WAKE.with(|c| *c.borrow_mut() = Some(w.clone()));
    let (etx, erx) = mpsc::channel();
    let (otx, orx) = unbounded_channel();
    spawn_local(net_loop(orx, EventTx::new(etx, w)));
    app.net = Some(NetHandle::new(erx, otx));
}

// ---- transport -------------------------------------------------------------------------

enum Inbound {
    Open,
    Text(String),
    Closed(String),
}

/// A connected browser WebSocket. Dropping it closes the socket.
pub struct WebTransport {
    ws: WebSocket,
    rx: UnboundedReceiver<Inbound>,
    _keep: Vec<Closure<dyn FnMut(JsValue)>>,
}

impl WebTransport {
    /// Opens the socket; `Err` when it never opened (refused, 401, network).
    pub async fn connect(url: &str) -> Result<Self, NetError> {
        let ws = WebSocket::new(url).map_err(|e| NetError::Web(js_err(e)))?;
        let (tx, mut rx) = unbounded_channel();
        let on = |tx: UnboundedSender<Inbound>, f: fn(JsValue) -> Option<Inbound>| {
            Closure::<dyn FnMut(JsValue)>::new(move |e| {
                if let Some(m) = f(e) {
                    let _ = tx.send(m);
                }
            })
        };
        let open = on(tx.clone(), |_| Some(Inbound::Open));
        let msg = on(tx.clone(), |e| {
            e.dyn_into::<MessageEvent>()
                .ok()
                .and_then(|m| m.data().as_string())
                .map(Inbound::Text)
        });
        let close = on(tx.clone(), |e| {
            let why = Reflect::get(&e, &"reason".into())
                .ok()
                .and_then(|r| r.as_string())
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| "socket closed".into());
            Some(Inbound::Closed(why))
        });
        ws.set_onopen(Some(open.as_ref().unchecked_ref()));
        ws.set_onmessage(Some(msg.as_ref().unchecked_ref()));
        ws.set_onclose(Some(close.as_ref().unchecked_ref()));
        let keep = vec![open, msg, close];
        // Dropping `t` on the error paths closes the socket.
        let mut t = Self {
            ws,
            rx: unbounded_channel().1,
            _keep: keep,
        };
        match rx.recv().await {
            Some(Inbound::Open) => {
                t.rx = rx;
                Ok(t)
            }
            Some(Inbound::Closed(why)) => Err(NetError::Web(why)),
            _ => Err(NetError::Web("socket failed".into())),
        }
    }
}

impl Drop for WebTransport {
    fn drop(&mut self) {
        self.ws.set_onclose(None);
        self.ws.set_onmessage(None);
        let _ = self.ws.close();
    }
}

impl Transport for WebTransport {
    async fn send(&mut self, frame: &UiToHub) -> Result<(), NetError> {
        self.ws
            .send_with_str(&encode(frame)?)
            .map_err(|e| NetError::Web(js_err(e)))
    }
    async fn recv(&mut self) -> Result<Option<ulak_proto::HubToUi>, NetError> {
        loop {
            match self.rx.recv().await {
                Some(Inbound::Text(t)) => return Ok(Some(decode(&t)?)),
                Some(Inbound::Closed(_)) | None => return Ok(None),
                Some(Inbound::Open) => {}
            }
        }
    }
}

fn ws_url() -> Result<String, NetError> {
    let loc = web_sys::window()
        .ok_or_else(|| NetError::Web("no window".into()))?
        .location();
    let scheme = if loc.protocol().unwrap_or_default() == "https:" {
        "wss"
    } else {
        "ws"
    };
    let host = loc.host().map_err(|e| NetError::Web(js_err(e)))?;
    let token = TOKEN.with(|t| t.borrow().clone());
    Ok(format!(
        "{scheme}://{host}/ws/ui{}",
        token.map(|t| format!("?token={t}")).unwrap_or_default()
    ))
}

async fn sleep(ms: i32) {
    let p = Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(p).await;
}

/// Connect, run, report, back off (1 s doubling to 30 s), repeat; a login `kick` skips the wait.
async fn net_loop(mut out: UnboundedReceiver<UiToHub>, events: EventTx) {
    let kick = KICK.with(Rc::clone);
    let mut delay = 1000;
    loop {
        if !events.send(Event::Connecting) {
            return;
        }
        let url = ws_url();
        match WebTransport::connect(url.as_deref().unwrap_or("")).await {
            Ok(mut t) => {
                while out.try_recv().is_ok() {}
                if !events.send(Event::Connected) {
                    return;
                }
                say(LoginMsg::Authed);
                delay = 1000;
                let r = run_session(&mut t, &mut out, &events).await;
                if matches!(r, Ok(Exit::UiGone)) {
                    return;
                }
                let why = r
                    .err()
                    .map_or_else(|| "closed by hub".into(), |e| e.to_string());
                if !events.send(Event::Disconnected(why)) {
                    return;
                }
            }
            Err(e) => {
                if !events.send(Event::Disconnected(e.to_string())) {
                    return;
                }
                // A browser socket cannot see the HTTP status; ask the API (401 = no session).
                if status("/api/devices").await == Some(401) {
                    say(LoginMsg::Needed);
                }
            }
        }
        tokio::select! {
            () = sleep(delay) => {}
            () = kick.notified() => {}
        }
        delay = (delay * 2).min(30_000);
    }
}

// ---- fetch -----------------------------------------------------------------------------

async fn fetch(path: &str, body: Option<&Value>) -> Result<Response, String> {
    let init = RequestInit::new();
    let h = Object::new();
    if let Some(b) = body {
        init.set_method("POST");
        init.set_body(&JsValue::from_str(&b.to_string()));
        Reflect::set(&h, &"content-type".into(), &"application/json".into()).map_err(js_err)?;
    }
    if let Some(t) = TOKEN.with(|t| t.borrow().clone()) {
        Reflect::set(&h, &"authorization".into(), &format!("Bearer {t}").into()).map_err(js_err)?;
    }
    init.set_headers(&h);
    let req = Request::new_with_str_and_init(path, &init).map_err(js_err)?;
    let w = web_sys::window().ok_or("no window")?;
    let r = JsFuture::from(w.fetch_with_request(&req))
        .await
        .map_err(js_err)?;
    r.dyn_into::<Response>().map_err(js_err)
}

async fn status(path: &str) -> Option<u16> {
    fetch(path, None).await.ok().map(|r| r.status())
}

async fn post_json(path: &str, body: &Value) -> Result<Value, String> {
    let r = fetch(path, Some(body)).await?;
    let ok = r.ok();
    let code = r.status();
    let text = JsFuture::from(r.text().map_err(js_err)?)
        .await
        .map_err(js_err)?
        .as_string()
        .unwrap_or_default();
    if ok {
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    } else {
        Err(match code {
            429 => "too many attempts, wait a bit".into(),
            403 => "refused: wrong, expired or already used code (or no passkey registered)".into(),
            401 => "passkey was not accepted".into(),
            c => format!(
                "hub answered {c}: {}",
                text.chars().take(120).collect::<String>()
            ),
        })
    }
}

// ---- WebAuthn --------------------------------------------------------------------------

/// `JSON.parse` the prepared options and put the bytes back as `Uint8Array`s.
fn to_js(p: &Prepared) -> Result<JsValue, String> {
    let root = js_sys::JSON::parse(&p.json.to_string()).map_err(js_err)?;
    for (path, bytes) in &p.binary {
        let (last, parents) = path.split_last().ok_or("empty path")?;
        let mut at = root.clone();
        for seg in parents {
            at = Reflect::get(&at, &seg_key(seg)).map_err(js_err)?;
        }
        Reflect::set(&at, &seg_key(last), &Uint8Array::from(bytes.as_slice())).map_err(js_err)?;
    }
    Ok(root)
}

fn seg_key(s: &Seg) -> JsValue {
    match s {
        Seg::Key(k) => k.as_str().into(),
        Seg::Idx(i) => (*i).into(),
    }
}

fn bytes_of(o: &JsValue, key: &str) -> Result<Vec<u8>, String> {
    let v = Reflect::get(o, &key.into()).map_err(js_err)?;
    if v.is_null() || v.is_undefined() {
        return Ok(Vec::new());
    }
    Ok(Uint8Array::new(&v).to_vec())
}

fn raw_credential(c: &JsValue) -> Result<RawCredential, String> {
    let resp = Reflect::get(c, &"response".into()).map_err(js_err)?;
    let user_handle = bytes_of(&resp, "userHandle")?;
    let transports = Reflect::get(&resp, &"getTransports".into())
        .ok()
        .and_then(|f| f.dyn_into::<Function>().ok())
        .and_then(|f| f.call0(&resp).ok())
        .map(|a| {
            Array::from(&a)
                .iter()
                .filter_map(|t| t.as_string())
                .collect()
        })
        .unwrap_or_default();
    Ok(RawCredential {
        id: Reflect::get(c, &"id".into())
            .map_err(js_err)?
            .as_string()
            .ok_or("credential has no id")?,
        raw_id: bytes_of(c, "rawId")?,
        client_data_json: bytes_of(&resp, "clientDataJSON")?,
        attestation_object: bytes_of(&resp, "attestationObject")?,
        transports,
        authenticator_data: bytes_of(&resp, "authenticatorData")?,
        signature: bytes_of(&resp, "signature")?,
        user_handle: (!user_handle.is_empty()).then_some(user_handle),
    })
}

async fn credential(options: &Value, which: Ceremony) -> Result<RawCredential, String> {
    let prepared = prepare_options(options, which)?;
    let pk: Object = Reflect::get(&to_js(&prepared)?, &"publicKey".into())
        .map_err(js_err)?
        .unchecked_into();
    let creds = web_sys::window()
        .ok_or("no window")?
        .navigator()
        .credentials();
    let promise = match which {
        Ceremony::Register => {
            let o = Object::new();
            Reflect::set(&o, &"publicKey".into(), &pk).map_err(js_err)?;
            creds.create_with_options(o.unchecked_ref::<CredentialCreationOptions>())
        }
        Ceremony::Login => {
            let o = Object::new();
            Reflect::set(&o, &"publicKey".into(), &pk).map_err(js_err)?;
            creds.get_with_options(o.unchecked_ref::<CredentialRequestOptions>())
        }
    }
    .map_err(js_err)?;
    let c = JsFuture::from(promise).await.map_err(js_err)?;
    if c.is_null() {
        return Err("no credential returned".into());
    }
    raw_credential(&c)
}

async fn do_login() -> Result<(), String> {
    let start = post_json("/auth/login/start", &json!({})).await?;
    let cred = credential(&start["options"], Ceremony::Login).await?;
    let fin = post_json(
        "/auth/login/finish",
        &json!({"challenge_id": start["challenge_id"], "credential": login_json(&cred)}),
    )
    .await?;
    // The cookie is set by the hub; keep the token in memory for the `?token=` fallback.
    TOKEN.with(|t| *t.borrow_mut() = fin["token"].as_str().map(str::to_owned));
    Ok(())
}

async fn do_register(otp: String, name: String) -> Result<(), String> {
    let name = if name.is_empty() {
        "this browser".into()
    } else {
        name
    };
    let start = post_json("/auth/register/start", &json!({"otp": otp, "name": name})).await?;
    let cred = credential(&start["options"], Ceremony::Register).await?;
    post_json(
        "/auth/register/finish",
        &json!({"challenge_id": start["challenge_id"], "credential": register_json(&cred)}),
    )
    .await?;
    do_login().await
}

fn finish(r: Result<(), String>) {
    match r {
        Ok(()) => KICK.with(|k| k.notify_one()),
        Err(e) => say(LoginMsg::Failed(e)),
    }
}

/// Passkey sign-in; the result arrives as `LoginMsg`s and the socket loop reconnects.
pub fn sign_in() {
    say(LoginMsg::Working);
    spawn_local(async { finish(do_login().await) });
}

pub fn register(otp: String, name: String) {
    say(LoginMsg::Working);
    spawn_local(async { finish(do_register(otp, name).await) });
}
