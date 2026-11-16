//! Question inbox: pending questions with an answer field each. Enter sends the answer; the card
//! disappears when the hub sends `QuestionClosed`.
use crate::{app::App, model::Intent};
use rune::{col, row, text, El, ScrollSpec};
use rune_kit::{Kit, TextInput};
use ulak_proto::{Question, QuestionId};

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

pub fn view(s: &App, k: &Kit) -> El {
    let mut c = col().gap(10.0).padding(16.0).w_full();
    if s.ui.questions.is_empty() {
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
