//! Question inbox: pending questions with an answer field each. Enter sends the answer; the card
//! disappears when the hub sends `QuestionClosed`.
use crate::{
    app::App,
    model::{kind_title, Intent},
};
use rune::{col, row, text, El, ScrollSpec};
use rune_kit::{Button, ButtonVariant, Kit, TextInput};
use ulak_proto::{Blocker, BlockerKind, Question, QuestionId};

pub fn age(ms: u64) -> String {
    match ms / 1000 {
        s if s < 60 => format!("{s}s ago"),
        s if s < 3600 => format!("{}m ago", s / 60),
        s => format!("{}h ago", s / 3600),
    }
}

fn answer_edit<'a>(s: &'a mut App, id: &QuestionId) -> &'a mut rune::TextEdit {
    s.answers.entry(id.clone()).or_default()
}

fn card(s: &App, k: &Kit, q: &Question) -> El {
    let id = q.id().clone();
    let (e, sub) = (id.clone(), id.clone());
    let edit = s.answers.get(&id).cloned().unwrap_or_default();
    let input = TextInput::new(&edit)
        .placeholder("Type an answer, Enter to send")
        .on_edit(move |s: &mut App| answer_edit(s, &e))
        .on_submit(move |s: &mut App, _| {
            let text = answer_edit(s, &sub).text.trim().to_string();
            if text.is_empty() {
                return;
            }
            s.dispatch(Intent::AnswerQuestion {
                question: sub.clone(),
                text,
            });
            answer_edit(s, &sub).clear();
        })
        .build(k)
        .key(format!("answer-{}", id.as_str()));
    col()
        .key(format!("question-{}", id.as_str()))
        .gap(8.0)
        .padding(12.0)
        .w_full()
        .radius(8.0)
        .fill(k.c.layer_01)
        .border(1.0, k.c.border_base)
        .child(
            row()
                .gap(8.0)
                .items_center()
                .child(
                    text(q.from().as_str())
                        .font(k.ty.ui_medium)
                        .color(k.c.text_accent),
                )
                .child(
                    text(age(s.ui.question_age_ms(q)))
                        .key(format!("age-{}", id.as_str()))
                        .font(k.ty.small)
                        .color(k.c.text_muted),
                ),
        )
        .child(text(q.text()).wrap().font(k.ty.body).color(k.c.text_base))
        .child(input)
}

/// Permanent card for a login/install blocker: no age timeout, only the user resolves it.
fn blocker_card(s: &App, k: &Kit, b: &Blocker) -> El {
    let id = b.id().as_str();
    let (a, r) = (b.agent().cloned(), b.id().clone());
    let armed = s.resolve_armed.as_ref() == Some(b.id());
    let mut actions = row().gap(8.0).items_center();
    if let (BlockerKind::Login, Some(agent)) = (b.kind(), a) {
        actions = actions.child(
            Button::new("Open agent")
                .variant(ButtonVariant::Outline)
                .on_press(move |s: &mut App| s.dispatch(Intent::SelectAgent(agent.clone())))
                .build(k)
                .key(format!("blocker-open-{id}")),
        );
    }
    actions = actions.child(
        Button::new(if armed {
            "Click again to confirm"
        } else {
            "Mark resolved"
        })
        .variant(if armed {
            ButtonVariant::Danger
        } else {
            ButtonVariant::Outline
        })
        .on_press(move |s: &mut App| s.resolve_click(&r))
        .build(k)
        .key(format!("blocker-resolve-{id}")),
    );
    col()
        .key(format!("blocker-{id}"))
        .gap(8.0)
        .padding(12.0)
        .w_full()
        .radius(8.0)
        .fill(k.c.warning.bg)
        .border(1.0, k.c.warning.border)
        .child(
            text(kind_title(b.kind()))
                .font(k.ty.ui_medium)
                .color(k.c.warning.fg),
        )
        .child(
            text(format!("{} / {}", b.node().as_str(), b.profile().as_str()))
                .font(k.ty.small)
                .color(k.c.text_muted),
        )
        .child(
            col()
                .padding(8.0)
                .radius(6.0)
                .w_full()
                .fill(k.c.bg_base)
                .max_h(120.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(text(b.detail()).wrap().font(k.ty.mono).color(k.c.text_base)),
        )
        .child(actions)
}

pub fn view(s: &App, k: &Kit) -> El {
    let mut c = col().gap(10.0).padding(16.0).w_full();
    for b in &s.ui.blockers {
        c = c.child(blocker_card(s, k, b));
    }
    if s.ui.questions.is_empty() && s.ui.blockers.is_empty() {
        c = c.child(
            text("No pending questions.")
                .font(k.ty.ui)
                .color(k.c.text_faint),
        );
    }
    for q in &s.ui.questions {
        c = c.child(card(s, k, q));
    }
    c.scroll(ScrollSpec {
        stick_bottom: false,
    })
    .grow(1.0)
}
