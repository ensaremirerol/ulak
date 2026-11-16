use proptest::prelude::*;
use ulak_proto::{AgentId, NodeId, Otp, ProtoError, QuestionId};

#[test]
fn accepts_lowercase_slug() {
    assert_eq!(NodeId::new("gaming-pc-1").unwrap().as_str(), "gaming-pc-1");
}

#[test]
fn rejects_bad_ids() {
    for bad in [
        "",
        "Upper",
        "has space",
        "semi;colon",
        "dot.dot",
        "a/b",
        &"x".repeat(65),
    ] {
        assert!(
            matches!(AgentId::new(bad), Err(ProtoError::InvalidId { .. })),
            "{bad:?} should be rejected"
        );
    }
}

#[test]
fn tmux_session_name() {
    assert_eq!(
        AgentId::new("reviewer-2").unwrap().tmux_session(),
        "ulak-reviewer-2"
    );
}

#[test]
fn id_kinds_are_distinct_types_with_same_rules() {
    assert!(QuestionId::new("q-1").is_ok());
    assert!(QuestionId::new("Q 1").is_err());
}

#[test]
fn otp_is_eight_uppercase_alphanumerics() {
    assert!(Otp::new("K7Q2M9XA").is_ok());
    for bad in ["", "K7Q2M9X", "K7Q2M9XAB", "k7q2m9xa", "K7Q2-9XA"] {
        assert!(
            matches!(Otp::new(bad), Err(ProtoError::InvalidOtp)),
            "{bad:?}"
        );
    }
}

#[test]
fn otp_is_not_leaked_by_debug() {
    let dbg = format!("{:?}", Otp::new("K7Q2M9XA").unwrap());
    assert!(!dbg.contains("K7Q2M9XA"));
}

proptest! {
    #[test]
    fn accepted_ids_are_safe_for_tmux(s in ".{0,80}") {
        if let Ok(id) = AgentId::new(&s) {
            let name = id.tmux_session();
            prop_assert!(name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
            prop_assert!(name.len() <= 69);
        }
    }
}
