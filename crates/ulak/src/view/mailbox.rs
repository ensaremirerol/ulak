//! Mailbox inspector: read-only message log with a thread filter.
use crate::{app::App, model::Intent};
use rune::{col, row, text, El, ScrollSpec};
use rune_kit::{Button, ButtonVariant, Kit};
use ulak_proto::Message;

/// `HH:MM:SS` (UTC) of a unix-ms timestamp.
pub fn clock(ms: u64) -> String {
    let s = ms / 1000 % 86_400;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// First line, at most 80 characters.
pub fn preview(p: &str) -> String {
    let line = p.lines().next().unwrap_or("");
    let mut out: String = line.chars().take(80).collect();
    if out.len() < line.len() || p.lines().count() > 1 {
        out.push_str("...");
    }
    out
}

/// Messages that pass the thread filter.
pub fn visible(s: &App) -> Vec<&Message> {
    s.ui.mailbox
        .iter()
        .filter(|m| s.thread_filter.as_ref().is_none_or(|t| m.thread() == t))
        .collect()
}

fn msg_row(k: &Kit, m: &Message) -> El {
    let t = m.thread().clone();
    let id = m.id().as_str();
    row()
        .key(format!("msg-{id}"))
        .gap(12.0)
        .center_y()
        .px(10.0)
        .py(6.0)
        .w_full()
        .radius(6.0)
        .fill(k.c.layer_01)
        .child(
            text(clock(m.at_ms()))
                .w(64.0)
                .font(k.ty.small)
                .color(k.c.text_muted),
        )
        .child(
            text(format!("{} -> {}", m.from().as_str(), m.to().as_str()))
                .w(180.0)
                .font(k.ty.ui_medium)
                .color(k.c.text_base),
        )
        .child(
            row().w(130.0).child(
                Button::new(t.as_str())
                    .variant(ButtonVariant::Ghost)
                    .on_press(move |s: &mut App| s.thread_filter = Some(t.clone()))
                    .build(k)
                    .key(format!("thread-{id}")),
            ),
        )
        .child(
            text(format!("{:?}", m.status).to_lowercase())
                .w(70.0)
                .font(k.ty.small)
                .color(k.c.text_muted),
        )
        .child(
            text(preview(m.payload()))
                .grow(1.0)
                .font(k.ty.ui)
                .color(k.c.text_base),
        )
}

pub fn view(s: &App, k: &Kit) -> El {
    let msgs = visible(s);
    let mut head = row().gap(8.0).center_y().child(
        text(format!(
            "Mailbox ({} of {})",
            msgs.len(),
            s.ui.mailbox.len()
        ))
        .key("mailbox-count")
        .font(k.ty.body_medium)
        .color(k.c.text_strong),
    );
    if let Some(t) = &s.thread_filter {
        head = head
            .child(
                text(format!("thread: {}", t.as_str()))
                    .key("mailbox-filter")
                    .font(k.ty.ui)
                    .color(k.c.text_accent),
            )
            .child(
                Button::new("Clear filter")
                    .variant(ButtonVariant::Outline)
                    .on_press(|s: &mut App| s.thread_filter = None)
                    .build(k)
                    .key("clear-filter"),
            );
    }
    head = head.child(row().grow(1.0)).child(
        Button::new("Refresh")
            .variant(ButtonVariant::Outline)
            .on_press(|s: &mut App| s.dispatch(Intent::RefreshMailbox(None)))
            .build(k)
            .key("mailbox-refresh"),
    );
    let mut list = col().gap(4.0).w_full();
    if msgs.is_empty() {
        list = list.child(
            text("No messages yet. Agents write here when they use the mailbox tools.")
                .key("mailbox-empty")
                .font(k.ty.ui)
                .color(k.c.text_faint),
        );
    }
    for m in msgs {
        list = list.child(msg_row(k, m));
    }
    col()
        .gap(10.0)
        .padding(16.0)
        .w_full()
        .child(head)
        .child(list)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
}
