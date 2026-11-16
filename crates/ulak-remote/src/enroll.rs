use crate::{Ca, DeviceKind, Devices, EnrollError, Store};
use rcgen::SubjectPublicKeyInfo;
use std::{collections::HashMap, sync::Arc, sync::Mutex};
use ulak_proto::{EnrollRequest, EnrollResponse, NodeId, Otp};

const OTP_TTL_MS: u64 = 300_000;
const MAX_FAILURES: u32 = 3;
/// 32 symbols without I and O, so `byte % 32` is unbiased.
const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// What the UI shows after "Add device".
#[derive(Debug, Clone)]
pub struct OtpOffer {
    pub otp: Otp,
    pub hub_fingerprint: String,
    pub expires_ms: u64,
}

#[derive(Debug)]
pub struct Enrolled {
    pub device: NodeId,
    pub response: EnrollResponse,
}

#[derive(Default)]
struct State {
    offers: HashMap<String, (DeviceKind, u64)>,
    failures: u32,
    locked: bool,
}

/// Issues one-time codes and turns a valid code plus a public key into a client certificate.
// ponytail: failures are counted globally (a wrong code cannot be tied to an offer);
// per-IP limits belong in the HTTP layer.
pub struct Enroller {
    ca: Ca,
    store: Arc<Store>,
    state: Mutex<State>,
    cert_days: i64,
}

impl Enroller {
    pub fn new(ca: Ca, store: Arc<Store>) -> Self {
        Self {
            ca,
            store,
            state: Mutex::default(),
            cert_days: 90,
        }
    }

    /// Lifetime of issued client certificates (default 90 days).
    pub fn with_cert_days(mut self, days: i64) -> Self {
        self.cert_days = days;
        self
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("enroller lock is never held across a panic")
    }

    /// Operator action ("Add device"). Also clears a lockout.
    pub fn begin(&self, kind: DeviceKind, now_ms: u64) -> OtpOffer {
        let mut raw = [0u8; 8];
        rand::fill(&mut raw[..]);
        let code: String = raw
            .iter()
            .map(|b| ALPHABET[(b % 32) as usize] as char)
            .collect();
        let otp = Otp::new(&code).expect("generated code uses the OTP alphabet");
        let expires_ms = now_ms + OTP_TTL_MS;
        let mut st = self.state();
        st.locked = false;
        st.failures = 0;
        st.offers.insert(code, (kind, expires_ms));
        OtpOffer {
            otp,
            hub_fingerprint: self.ca.fingerprint().to_owned(),
            expires_ms,
        }
    }

    fn take(
        &self,
        otp: &Otp,
        now_ms: u64,
        allow: fn(DeviceKind) -> bool,
    ) -> Result<DeviceKind, EnrollError> {
        let mut st = self.state();
        if st.locked {
            return Err(EnrollError::Locked);
        }
        match st.offers.get(otp.expose()).copied() {
            Some((kind, exp)) if now_ms <= exp && allow(kind) => {
                st.offers.remove(otp.expose());
                st.failures = 0;
                Ok(kind)
            }
            _ => {
                st.failures += 1;
                if st.failures >= MAX_FAILURES {
                    st.locked = true;
                    st.offers.clear();
                }
                Err(EnrollError::BadOtp)
            }
        }
    }

    /// Spends a code of any kind (used for browser passkey registration).
    pub fn consume(&self, otp: &Otp, now_ms: u64) -> Result<DeviceKind, EnrollError> {
        self.take(otp, now_ms, |_| true)
    }

    /// Node or UI device: spends the code and signs the device's public key.
    pub fn redeem(&self, req: &EnrollRequest, now_ms: u64) -> Result<Enrolled, EnrollError> {
        let key = SubjectPublicKeyInfo::from_pem(req.public_key())
            .map_err(|_| EnrollError::InvalidKey)?;
        let kind = self.take(req.otp(), now_ms, |k| k != DeviceKind::Browser)?;
        let device = self
            .store
            .add(req.hostname(), kind, req.public_key(), now_ms)?;
        let cert = self.ca.sign(&device, &key, now_ms, self.cert_days)?;
        Ok(Enrolled {
            response: EnrollResponse::new(cert, self.ca.cert_pem().to_owned()),
            device,
        })
    }

    /// Fresh certificate for an existing device (same id, so same kind) and its new public key.
    /// The caller has already authenticated the device and checked the renewal window.
    pub fn renew(
        &self,
        id: &NodeId,
        public_key: &str,
        now_ms: u64,
    ) -> Result<EnrollResponse, EnrollError> {
        let key =
            SubjectPublicKeyInfo::from_pem(public_key).map_err(|_| EnrollError::InvalidKey)?;
        let cert = self.ca.sign(id, &key, now_ms, self.cert_days)?;
        self.store.mark_renewed(id, public_key, now_ms)?;
        Ok(EnrollResponse::new(cert, self.ca.cert_pem().to_owned()))
    }
}
