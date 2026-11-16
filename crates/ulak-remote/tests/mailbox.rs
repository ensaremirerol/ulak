use ulak_proto::{AgentId, MessageStatus};
use ulak_remote::{Mailbox, MailboxError, Store};

fn a(s: &str) -> AgentId {
    AgentId::new(s).unwrap()
}
fn store() -> Store {
    Store::open_in_memory().unwrap()
}

#[test]
fn send_then_inbox_delivers_once() {
    let s = store();
    s.send(&a("a-1"), &a("b-1"), None, "hello".into(), 10)
        .unwrap();
    let got = s.inbox(&a("b-1"), 10).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].payload(), "hello");
    assert_eq!(got[0].status, MessageStatus::Delivered);
    assert!(
        s.inbox(&a("b-1"), 10).unwrap().is_empty(),
        "already delivered"
    );
}

#[test]
fn inbox_is_per_recipient_oldest_first_and_limited() {
    let s = store();
    for (i, body) in ["one", "two", "three"].iter().enumerate() {
        s.send(&a("a-1"), &a("b-1"), None, (*body).into(), i as u64)
            .unwrap();
    }
    s.send(&a("a-1"), &a("c-1"), None, "other".into(), 5)
        .unwrap();
    let got = s.inbox(&a("b-1"), 2).unwrap();
    assert_eq!(
        got.iter().map(|m| m.payload()).collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(s.inbox(&a("b-1"), 10).unwrap()[0].payload(), "three");
}

#[test]
fn threads_are_created_and_reused() {
    let s = store();
    let first = s.send(&a("a-1"), &a("b-1"), None, "q".into(), 1).unwrap();
    let reply = s
        .send(
            &a("b-1"),
            &a("a-1"),
            Some(first.thread().clone()),
            "r".into(),
            2,
        )
        .unwrap();
    s.send(&a("a-1"), &a("b-1"), None, "unrelated".into(), 3)
        .unwrap();
    assert_eq!(first.thread(), reply.thread());
    let log = s.log(Some(first.thread()), 10).unwrap();
    assert_eq!(
        log.iter().map(|m| m.payload()).collect::<Vec<_>>(),
        ["q", "r"]
    );
    assert_eq!(s.log(None, 10).unwrap().len(), 3);
}

#[test]
fn empty_body_rejected() {
    assert!(matches!(
        store().send(&a("a-1"), &a("b-1"), None, "".into(), 1),
        Err(MailboxError::Invalid(_))
    ));
}

#[test]
fn mark_read() {
    let s = store();
    let m = s.send(&a("a-1"), &a("b-1"), None, "x".into(), 1).unwrap();
    s.mark_read(m.id()).unwrap();
    assert_eq!(s.log(None, 10).unwrap()[0].status, MessageStatus::Read);
    let missing = ulak_proto::MessageId::new("nope").unwrap();
    assert!(matches!(
        s.mark_read(&missing),
        Err(MailboxError::NotFound(_))
    ));
}

#[test]
fn survives_reopen_on_disk() {
    let dir = std::env::temp_dir().join(format!("ulak-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("hub.db");
    Store::open(&path)
        .unwrap()
        .send(&a("a-1"), &a("b-1"), None, "kept".into(), 1)
        .unwrap();
    assert_eq!(
        Store::open(&path).unwrap().inbox(&a("b-1"), 10).unwrap()[0].payload(),
        "kept"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
