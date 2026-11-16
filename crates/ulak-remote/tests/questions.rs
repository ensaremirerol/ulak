use ulak_proto::{AgentId, Answer, QuestionId};
use ulak_remote::{QuestionError, Questions, Store};

fn a(s: &str) -> AgentId {
    AgentId::new(s).unwrap()
}
fn store() -> Store {
    Store::open_in_memory().unwrap()
}

#[test]
fn open_answer_collect() {
    let s = store();
    let q = s.open(&a("b-1"), "Approve migration?".into(), 0).unwrap();
    assert_eq!(s.pending().unwrap(), vec![q.clone()]);
    assert_eq!(s.collect(q.id(), 5).unwrap(), None, "not answered yet");

    s.answer(Answer::new(q.id().clone(), "yes".into()).unwrap(), 10)
        .unwrap();
    assert!(s.pending().unwrap().is_empty());
    let got = s.collect(q.id(), 20).unwrap().unwrap();
    assert_eq!(got.text(), "yes");
    assert_eq!(
        s.collect(q.id(), 30).unwrap().unwrap().text(),
        "yes",
        "repeatable"
    );
}

#[test]
fn answer_errors() {
    let s = store();
    let q = s.open(&a("b-1"), "?".into(), 0).unwrap();
    let ans = || Answer::new(q.id().clone(), "ok".into()).unwrap();
    s.answer(ans(), 1).unwrap();
    assert!(matches!(
        s.answer(ans(), 2),
        Err(QuestionError::AlreadyAnswered(_))
    ));
    let ghost = Answer::new(QuestionId::new("ghost").unwrap(), "x".into()).unwrap();
    assert!(matches!(
        s.answer(ghost, 3),
        Err(QuestionError::NotFound(_))
    ));
}

#[test]
fn nudges_repeat_until_collected_or_max() {
    let s = store();
    let q = s.open(&a("b-1"), "?".into(), 0).unwrap();
    assert!(
        s.due_nudges(0, 120_000, 3).unwrap().is_empty(),
        "unanswered: nothing to nudge"
    );

    s.answer(Answer::new(q.id().clone(), "yes".into()).unwrap(), 1_000)
        .unwrap();
    let due = s.due_nudges(1_000, 120_000, 3).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].from(), &a("b-1"));
    s.record_nudge(q.id(), 1_000).unwrap();

    assert!(
        s.due_nudges(60_000, 120_000, 3).unwrap().is_empty(),
        "too soon"
    );
    assert_eq!(s.due_nudges(121_000, 120_000, 3).unwrap().len(), 1);
    s.record_nudge(q.id(), 121_000).unwrap();
    s.record_nudge(q.id(), 241_000).unwrap();
    assert!(
        s.due_nudges(999_999, 120_000, 3).unwrap().is_empty(),
        "max nudges reached"
    );
}

#[test]
fn collecting_stops_nudges() {
    let s = store();
    let q = s.open(&a("b-1"), "?".into(), 0).unwrap();
    s.answer(Answer::new(q.id().clone(), "yes".into()).unwrap(), 1)
        .unwrap();
    s.collect(q.id(), 2).unwrap();
    assert!(s.due_nudges(1_000_000, 1, 3).unwrap().is_empty());
}
