//! Browser sign-in: the login screen state and the pure JSON <-> WebAuthn conversions.
//! Compiled on every target so the logic has native tests; `web.rs` (wasm only) does the
//! `fetch` and `navigator.credentials` calls and posts `LoginMsg`s back through `post`.
//!
//! The hub speaks webauthn-rs JSON: options come as `{"publicKey":{...}}` with base64url
//! strings where the browser API wants `ArrayBuffer`s, and the credential goes back with
//! base64url strings. `prepare_options` pulls the binary fields out (the glue puts them back
//! as `Uint8Array`s at the returned paths); `register_json` / `login_json` build the replies.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rune::TextEdit;
use serde_json::{json, Map, Value};
use std::cell::RefCell;

pub fn b64u_encode(b: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(b)
}

/// Decodes base64url, with or without `=` padding.
pub fn b64u_decode(s: &str) -> Result<Vec<u8>, String> {
    URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .map_err(|e| format!("bad base64url: {e}"))
}

/// One step into a JSON/JS value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    Key(String),
    Idx(u32),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ceremony {
    Register,
    Login,
}

/// Options with the binary fields replaced by `null`, plus those fields' bytes by path.
#[derive(Debug, Clone, PartialEq)]
pub struct Prepared {
    /// What to `JSON.parse` and then patch (`{"publicKey":{...}}`).
    pub json: Value,
    pub binary: Vec<(Vec<Seg>, Vec<u8>)>,
}

fn take_bin(
    v: &mut Value,
    path: Vec<Seg>,
    out: &mut Vec<(Vec<Seg>, Vec<u8>)>,
) -> Result<(), String> {
    let Value::String(s) = v.take() else {
        return Err(format!("expected a base64url string at {path:?}"));
    };
    out.push((path, b64u_decode(&s)?));
    Ok(())
}

/// Splits the hub's `options` (a webauthn-rs Creation/RequestChallengeResponse) for the browser.
pub fn prepare_options(options: &Value, which: Ceremony) -> Result<Prepared, String> {
    let mut json = options.clone();
    let pk = json
        .get_mut("publicKey")
        .ok_or("options has no publicKey")?;
    let key = |k: &str| Seg::Key(k.into());
    let mut binary = Vec::new();
    let challenge = pk.get_mut("challenge").ok_or("options has no challenge")?;
    take_bin(
        challenge,
        vec![key("publicKey"), key("challenge")],
        &mut binary,
    )?;
    if which == Ceremony::Register {
        let id = pk
            .get_mut("user")
            .and_then(|u| u.get_mut("id"))
            .ok_or("options has no user.id")?;
        take_bin(
            id,
            vec![key("publicKey"), key("user"), key("id")],
            &mut binary,
        )?;
    }
    let list = match which {
        Ceremony::Register => "excludeCredentials",
        Ceremony::Login => "allowCredentials",
    };
    if let Some(Value::Array(items)) = pk.get_mut(list) {
        for (i, item) in items.iter_mut().enumerate() {
            let id = item
                .get_mut("id")
                .ok_or("credential descriptor has no id")?;
            let path = vec![key("publicKey"), key(list), Seg::Idx(i as u32), key("id")];
            take_bin(id, path, &mut binary)?;
        }
    }
    Ok(Prepared { json, binary })
}

/// What the glue reads off a `PublicKeyCredential`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawCredential {
    pub id: String,
    pub raw_id: Vec<u8>,
    pub client_data_json: Vec<u8>,
    /// Registration only.
    pub attestation_object: Vec<u8>,
    pub transports: Vec<String>,
    /// Login only.
    pub authenticator_data: Vec<u8>,
    pub signature: Vec<u8>,
    pub user_handle: Option<Vec<u8>>,
}

/// `credential` for `POST /auth/register/finish`.
pub fn register_json(c: &RawCredential) -> Value {
    let mut response = Map::new();
    response.insert(
        "attestationObject".into(),
        json!(b64u_encode(&c.attestation_object)),
    );
    response.insert(
        "clientDataJSON".into(),
        json!(b64u_encode(&c.client_data_json)),
    );
    if !c.transports.is_empty() {
        response.insert("transports".into(), json!(c.transports));
    }
    json!({"id": c.id, "rawId": b64u_encode(&c.raw_id), "type": "public-key", "response": response, "extensions": {}})
}

/// `credential` for `POST /auth/login/finish`.
pub fn login_json(c: &RawCredential) -> Value {
    let mut response = Map::new();
    response.insert(
        "authenticatorData".into(),
        json!(b64u_encode(&c.authenticator_data)),
    );
    response.insert(
        "clientDataJSON".into(),
        json!(b64u_encode(&c.client_data_json)),
    );
    response.insert("signature".into(), json!(b64u_encode(&c.signature)));
    if let Some(h) = &c.user_handle {
        response.insert("userHandle".into(), json!(b64u_encode(h)));
    }
    json!({"id": c.id, "rawId": b64u_encode(&c.raw_id), "type": "public-key", "response": response, "extensions": {}})
}

/// OTPs are shown as `XXXX XXXX`; the hub wants the bare code.
pub fn clean_otp(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_uppercase()
}

/// Browser-side results the glue reports to the login screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginMsg {
    /// The hub refused the socket / API with 401: show the login screen.
    Needed,
    /// A ceremony or a ceremony step is running.
    Working,
    /// Signed in (or the socket connected): hide the login screen.
    Authed,
    Failed(String),
}

thread_local! {
    static INBOX: RefCell<Vec<LoginMsg>> = const { RefCell::new(Vec::new()) };
}

/// Queue a message for the UI (the browser is single threaded, so a thread-local is enough).
pub fn post(m: LoginMsg) {
    INBOX.with(|i| i.borrow_mut().push(m));
}

/// "Sign in with passkey" (browser build: runs the WebAuthn ceremony; native: explains).
pub fn start_login() {
    #[cfg(target_arch = "wasm32")]
    crate::web::sign_in();
    #[cfg(not(target_arch = "wasm32"))]
    post(LoginMsg::Failed("passkeys need the browser build".into()));
}

/// "Register this device": spend the OTP, create a passkey, then sign in with it.
pub fn start_register(otp: &str, name: &str) {
    let otp = clean_otp(otp);
    if otp.is_empty() {
        return post(LoginMsg::Failed("enter the one-time code first".into()));
    }
    #[cfg(target_arch = "wasm32")]
    crate::web::register(otp, name.trim().to_owned());
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = name;
        post(LoginMsg::Failed("passkeys need the browser build".into()));
    }
}

/// Login screen state (view state of the browser build; `visible` is never set natively).
#[derive(Default)]
pub struct Login {
    pub visible: bool,
    pub busy: bool,
    pub otp: TextEdit,
    pub name: TextEdit,
    pub message: Option<String>,
}

impl Login {
    pub fn apply(&mut self, m: LoginMsg) {
        match m {
            LoginMsg::Needed => self.visible = true,
            LoginMsg::Working => {
                self.busy = true;
                self.message = None;
            }
            LoginMsg::Authed => {
                self.visible = false;
                self.busy = false;
                self.message = None;
                self.otp.clear();
            }
            LoginMsg::Failed(e) => {
                self.busy = false;
                self.visible = true;
                self.message = Some(e);
            }
        }
    }

    /// Apply queued messages; true when any.
    pub fn drain(&mut self) -> bool {
        let msgs = INBOX.with(|i| std::mem::take(&mut *i.borrow_mut()));
        let any = !msgs.is_empty();
        for m in msgs {
            self.apply(m);
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use webauthn_rs_proto::{
        CreationChallengeResponse, PublicKeyCredential, RegisterPublicKeyCredential,
        RequestChallengeResponse,
    };

    const CREATION: &str = r#"{"publicKey":{"rp":{"name":"ulak","id":"hub.example"},
      "user":{"id":"AQIDBA","name":"operator","displayName":"operator"},
      "challenge":"q83vEjRW","pubKeyCredParams":[{"type":"public-key","alg":-7}],
      "timeout":60000,"excludeCredentials":[{"type":"public-key","id":"CQgH"},{"type":"public-key","id":"AAEC"}],
      "attestation":"none","authenticatorSelection":{"residentKey":"preferred","requireResidentKey":false,"userVerification":"preferred"}}}"#;
    const REQUEST: &str = r#"{"publicKey":{"challenge":"q83vEjRW","timeout":60000,"rpId":"hub.example",
      "allowCredentials":[{"type":"public-key","id":"CQgH"}],"userVerification":"preferred"}}"#;

    #[test]
    fn base64url_round_trips_and_accepts_padding() {
        let data = [0u8, 255, 254, 1, 2, 250];
        let s = b64u_encode(&data);
        assert!(!s.contains(['+', '/', '=']));
        assert_eq!(b64u_decode(&s).unwrap(), data);
        assert_eq!(b64u_decode("AQI=").unwrap(), [1, 2]);
        assert!(b64u_decode("a+b").is_err());
    }

    #[test]
    fn creation_options_are_split_into_paths_and_bytes() {
        // The sample is valid hub JSON.
        serde_json::from_str::<CreationChallengeResponse>(CREATION).unwrap();
        let p =
            prepare_options(&serde_json::from_str(CREATION).unwrap(), Ceremony::Register).unwrap();
        let k = |s: &str| Seg::Key(s.into());
        assert_eq!(p.binary.len(), 4);
        assert_eq!(
            p.binary[1],
            (vec![k("publicKey"), k("user"), k("id")], vec![1, 2, 3, 4])
        );
        assert_eq!(
            p.binary[3].0,
            vec![
                k("publicKey"),
                k("excludeCredentials"),
                Seg::Idx(1),
                k("id")
            ]
        );
        assert_eq!(p.binary[3].1, vec![0, 1, 2]);
        assert_eq!(p.json["publicKey"]["challenge"], Value::Null);
        assert_eq!(p.json["publicKey"]["rp"]["id"], "hub.example");
    }

    #[test]
    fn request_options_use_allow_credentials_and_reject_junk() {
        serde_json::from_str::<RequestChallengeResponse>(REQUEST).unwrap();
        let p = prepare_options(&serde_json::from_str(REQUEST).unwrap(), Ceremony::Login).unwrap();
        assert_eq!(p.binary.len(), 2);
        assert!(prepare_options(&json!({}), Ceremony::Login).is_err());
        assert!(prepare_options(&json!({"publicKey":{"challenge":5}}), Ceremony::Login).is_err());
        // Login options have no user.id.
        assert!(
            prepare_options(&serde_json::from_str(REQUEST).unwrap(), Ceremony::Register).is_err()
        );
    }

    #[test]
    fn credentials_serialize_to_what_webauthn_rs_accepts() {
        let reg = RawCredential {
            id: "AAEC".into(),
            raw_id: vec![0, 1, 2],
            client_data_json: b"{}".to_vec(),
            attestation_object: vec![9, 9],
            transports: vec!["internal".into()],
            ..Default::default()
        };
        serde_json::from_value::<RegisterPublicKeyCredential>(register_json(&reg)).unwrap();
        let login = RawCredential {
            authenticator_data: vec![1; 37],
            signature: vec![7, 7],
            user_handle: Some(vec![1, 2, 3, 4]),
            ..reg
        };
        serde_json::from_value::<PublicKeyCredential>(login_json(&login)).unwrap();
        let no_handle = RawCredential {
            user_handle: None,
            ..login
        };
        let v = login_json(&no_handle);
        assert!(v["response"].get("userHandle").is_none());
        serde_json::from_value::<PublicKeyCredential>(v).unwrap();
    }

    #[test]
    fn otp_is_normalised_and_login_state_follows_messages() {
        assert_eq!(clean_otp(" abcd 2345\n"), "ABCD2345");
        let mut l = Login::default();
        assert!(!l.visible);
        post(LoginMsg::Needed);
        post(LoginMsg::Working);
        assert!(l.drain());
        assert!(l.visible && l.busy);
        assert!(!l.drain());
        l.apply(LoginMsg::Failed("nope".into()));
        assert!(l.visible && !l.busy && l.message.as_deref() == Some("nope"));
        l.apply(LoginMsg::Authed);
        assert!(!l.visible && l.message.is_none());
    }
}
