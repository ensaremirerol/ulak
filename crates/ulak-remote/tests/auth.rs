use std::sync::Arc;
use ulak_proto::*;
use ulak_remote::*;

const DAY: u64 = 86_400_000;

struct Fx {
    hub: Enroller,
    store: Arc<Store>,
    ca_pem: String,
    ca_fp: String,
}

fn fx() -> Fx {
    let dir = std::env::temp_dir().join(format!("ulak-auth-{}", uuid::Uuid::now_v7()));
    let ca = Ca::load_or_create(&dir).unwrap();
    let (ca_pem, ca_fp) = (ca.cert_pem().to_owned(), ca.fingerprint().to_owned());
    let store = Arc::new(Store::open_in_memory().unwrap());
    Fx {
        hub: Enroller::new(ca, store.clone()),
        store,
        ca_pem,
        ca_fp,
    }
}

fn req(otp: &Otp, host: &str) -> EnrollRequest {
    let key = rcgen::KeyPair::generate().unwrap().public_key_pem();
    EnrollRequest::new(otp.clone(), key, host.into()).unwrap()
}

fn wrong() -> Otp {
    Otp::new("AAAAAAAA").unwrap()
}

#[test]
fn ca_persists_with_private_key() {
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!("ulak-ca-{}", uuid::Uuid::now_v7()));
    let a = Ca::load_or_create(&dir).unwrap();
    let b = Ca::load_or_create(&dir).unwrap();
    assert_eq!(a.fingerprint(), b.fingerprint());
    assert_eq!(a.fingerprint().len(), 64);
    let mode = std::fs::metadata(dir.join("ca.key"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "ca.key must be private");
}

#[test]
fn enroll_issues_a_cert_signed_by_the_hub_ca() {
    let f = fx();
    let offer = f.hub.begin(DeviceKind::Node, 0);
    assert_eq!(offer.expires_ms, 300_000);
    assert_eq!(offer.hub_fingerprint, f.ca_fp);

    let done = f.hub.redeem(&req(&offer.otp, "Gaming PC!"), 1_000).unwrap();
    assert_eq!(done.device.as_str(), "gaming-pc");
    assert_eq!(done.response.ca_cert, f.ca_pem);

    let (_, ca_pem) = x509_parser::pem::parse_x509_pem(f.ca_pem.as_bytes()).unwrap();
    let ca = ca_pem.parse_x509().unwrap();
    let (_, pem) = x509_parser::pem::parse_x509_pem(done.response.cert.as_bytes()).unwrap();
    let cert = pem.parse_x509().unwrap();
    cert.verify_signature(Some(ca.public_key()))
        .expect("signed by the hub CA");
    assert_eq!(
        cert.subject()
            .iter_common_name()
            .next()
            .unwrap()
            .as_str()
            .unwrap(),
        "gaming-pc"
    );
    let life = cert.validity().not_after.timestamp() - cert.validity().not_before.timestamp();
    assert_eq!(
        life,
        91 * 86_400,
        "90 days, backdated one day for clock skew"
    );
}

#[test]
fn otp_is_single_use() {
    let f = fx();
    let offer = f.hub.begin(DeviceKind::Node, 0);
    f.hub.redeem(&req(&offer.otp, "a"), 1).unwrap();
    assert!(matches!(
        f.hub.redeem(&req(&offer.otp, "b"), 2),
        Err(EnrollError::BadOtp)
    ));
}

#[test]
fn otp_expires_after_five_minutes() {
    let f = fx();
    let offer = f.hub.begin(DeviceKind::Node, 0);
    assert!(matches!(
        f.hub.redeem(&req(&offer.otp, "a"), 300_001),
        Err(EnrollError::BadOtp)
    ));
    let offer = f.hub.begin(DeviceKind::Node, 0);
    assert!(f.hub.redeem(&req(&offer.otp, "a"), 300_000).is_ok());
}

#[test]
fn three_wrong_codes_lock_enrollment_until_a_new_offer() {
    let f = fx();
    let offer = f.hub.begin(DeviceKind::Node, 0);
    for _ in 0..3 {
        assert!(matches!(
            f.hub.redeem(&req(&wrong(), "x"), 1),
            Err(EnrollError::BadOtp)
        ));
    }
    assert!(matches!(
        f.hub.redeem(&req(&offer.otp, "x"), 2),
        Err(EnrollError::Locked)
    ));
    let fresh = f.hub.begin(DeviceKind::Node, 3);
    assert!(f.hub.redeem(&req(&fresh.otp, "x"), 4).is_ok());
}

#[test]
fn success_resets_the_failure_count() {
    let f = fx();
    let offer = f.hub.begin(DeviceKind::Node, 0);
    for _ in 0..2 {
        let _ = f.hub.redeem(&req(&wrong(), "x"), 1);
    }
    f.hub.redeem(&req(&offer.otp, "x"), 2).unwrap();
    let offer = f.hub.begin(DeviceKind::Node, 3);
    for _ in 0..2 {
        let _ = f.hub.redeem(&req(&wrong(), "x"), 4);
    }
    assert!(f.hub.redeem(&req(&offer.otp, "y"), 5).is_ok());
}

#[test]
fn bad_public_key_is_rejected_without_burning_the_otp() {
    let f = fx();
    let offer = f.hub.begin(DeviceKind::Node, 0);
    let bad = EnrollRequest::new(offer.otp.clone(), "garbage".into(), "a".into()).unwrap();
    assert!(matches!(
        f.hub.redeem(&bad, 1),
        Err(EnrollError::InvalidKey)
    ));
    assert!(f.hub.redeem(&req(&offer.otp, "a"), 2).is_ok());
}

#[test]
fn browser_codes_are_consumed_separately() {
    let f = fx();
    let offer = f.hub.begin(DeviceKind::Browser, 0);
    assert!(matches!(
        f.hub.redeem(&req(&offer.otp, "phone"), 1),
        Err(EnrollError::BrowserCode)
    ));
    // Refused unspent, and a mixup is not a failed guess.
    assert_eq!(f.hub.consume(&offer.otp, 1).unwrap(), DeviceKind::Browser);
    assert!(matches!(
        f.hub.consume(&offer.otp, 2),
        Err(EnrollError::BadOtp)
    ));
}

#[test]
fn devices_are_listed_named_uniquely_and_revocable() {
    let f = fx();
    let mut ids = vec![];
    for host in ["box", "box"] {
        let o = f.hub.begin(DeviceKind::Node, 0);
        ids.push(f.hub.redeem(&req(&o.otp, host), 1).unwrap().device);
    }
    assert_eq!(
        ids.iter().map(|i| i.as_str()).collect::<Vec<_>>(),
        ["box", "box-2"]
    );
    assert_eq!(f.store.list().unwrap().len(), 2);
    assert!(f.store.is_active(&ids[0]).unwrap());
    f.store.revoke(&ids[0], 9).unwrap();
    assert!(!f.store.is_active(&ids[0]).unwrap());
    assert!(f.store.is_active(&ids[1]).unwrap());
    let ghost = NodeId::new("ghost").unwrap();
    assert!(matches!(
        f.store.revoke(&ghost, 9),
        Err(DeviceError::NotFound(_))
    ));
    assert!(!f.store.is_active(&ghost).unwrap());
}

#[test]
fn agent_tokens_verify_expire_and_revoke() {
    let s = Store::open_in_memory().unwrap();
    let a = AgentId::new("a-1").unwrap();
    let t1 = s.issue(&a, DAY, 0).unwrap();
    let t2 = s.issue(&a, DAY, 0).unwrap();
    assert_ne!(t1.expose(), t2.expose());
    assert_eq!(s.verify(&t1, DAY).unwrap(), a);
    assert!(matches!(s.verify(&t1, DAY + 1), Err(AuthError::Invalid)));
    assert!(matches!(
        s.verify(&AgentToken::new("f".repeat(64)).unwrap(), 0),
        Err(AuthError::Invalid)
    ));
    s.revoke_agent(&a).unwrap();
    assert!(matches!(s.verify(&t2, 1), Err(AuthError::Invalid)));
}

#[test]
fn device_code_at_browser_registration_is_refused_unspent() {
    let f = fx();
    let offer = f.hub.begin(DeviceKind::Ui, 0);
    for t in 1..6 {
        assert!(matches!(
            f.hub.consume(&offer.otp, t),
            Err(EnrollError::DeviceCode)
        ));
    }
    // Ui and node codes are interchangeable for devices.
    assert!(f.hub.redeem(&req(&offer.otp, "phone"), 7).is_ok());
}
