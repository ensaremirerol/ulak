//! Mux panel: agents grouped by node on the left, the selected agent's terminal on the right.
//!
//! The screen is a fixed `term::Screen` (default 120x40; no resize negotiation yet). The pane
//! renders rows up to the last non-blank row or the cursor and scrolls; the footer says the
//! size. Text uses `Family::Mono` (`AppSpec::families` maps it to the system mono font).
use crate::{app::App, model::Intent, term};
use rune::{col, color::hex, el, row, text, Color, El, ScrollSpec};
use rune_kit::{Button, ButtonVariant, Kit};
use ulak_proto::{AgentId, AgentStatus};

const ANSI: [&str; 16] = [
    "#1d1f21", "#cc6666", "#b5bd68", "#f0c674", "#81a2be", "#b294bb", "#8abeb7", "#c5c8c6",
    "#666666", "#d54e53", "#b9ca4a", "#e7c547", "#7aa6da", "#c397d8", "#70c0b1", "#eaeaea",
];

fn rgb(r: u8, g: u8, b: u8) -> Color {
    hex(&format!("#{r:02x}{g:02x}{b:02x}")).expect("six hex digits")
}

/// xterm 256-color palette.
pub fn term_color(c: term::Color) -> Color {
    match c {
        term::Color::Rgb(r, g, b) => rgb(r, g, b),
        term::Color::Indexed(i) if i < 16 => hex(ANSI[i as usize]).expect("static palette"),
        term::Color::Indexed(i) if i >= 232 => {
            let v = 8 + (i - 232) * 10;
            rgb(v, v, v)
        }
        term::Color::Indexed(i) => {
            let n = i - 16;
            let lvl = |x: u8| if x == 0 { 0 } else { 55 + 40 * x };
            rgb(lvl(n / 36), lvl(n / 6 % 6), lvl(n % 6))
        }
    }
}

fn status_dot(k: &Kit, st: Option<&AgentStatus>) -> (Color, &'static str) {
    match st {
        None | Some(AgentStatus::Starting) => (k.c.warning.icon, "starting"),
        Some(AgentStatus::Running) => (k.c.success.icon, "running"),
        Some(AgentStatus::Exited(_)) => (k.c.text_faint, "exited"),
        Some(AgentStatus::Failed) => (k.c.danger.icon, "failed"),
    }
}

fn agent_row(s: &App, k: &Kit, id: &AgentId) -> El {
    let (dot, label) = status_dot(k, s.ui.statuses.get(id));
    let sel = s.ui.selected.as_ref() == Some(id);
    let (a, b) = (id.clone(), id.clone());
    row()
        .gap(4.0)
        .center_y()
        .child(
            row()
                .key(format!("agent-{}", id.as_str()))
                .label(format!("{} ({label})", id.as_str()))
                .grow(1.0)
                .gap(8.0)
                .center_y()
                .px(8.0)
                .h(28.0)
                .radius(6.0)
                .fill(if sel { k.c.layer_03 } else { k.c.layer_01 })
                .on_press(move |s: &mut App| s.dispatch(Intent::SelectAgent(a.clone())))
                .child(
                    el().key(format!("dot-{}", id.as_str()))
                        .w(8.0)
                        .h(8.0)
                        .radius(4.0)
                        .fill(dot),
                )
                .child(text(id.as_str()).font(k.ty.ui).color(k.c.text_base)),
        )
        .child(
            Button::new("Kill")
                .variant(ButtonVariant::Danger)
                .on_press(move |s: &mut App| s.dispatch(Intent::Kill(b.clone())))
                .build(k)
                .key(format!("kill-{}", id.as_str())),
        )
}

fn sidebar(s: &App, k: &Kit) -> El {
    let mut c = col().w(230.0).gap(6.0).padding(10.0).fill(k.c.bg_deep);
    for n in &s.ui.nodes {
        c = c.child(
            text(n.hostname.clone())
                .font(k.ty.small_medium)
                .color(k.c.text_muted)
                .key(format!("group-{}", n.id.as_str())),
        );
        for a in &n.agents {
            c = c.child(agent_row(s, k, a));
        }
    }
    c.scroll(ScrollSpec {
        stick_bottom: false,
    })
}

fn screen_rows(k: &Kit, scr: &term::Screen) -> El {
    let (_, rows) = scr.size();
    let (cur_col, cur_row) = scr.cursor();
    let last = (0..rows)
        .rev()
        .find(|&r| !scr.line(r).trim().is_empty())
        .unwrap_or(0)
        .max(cur_row);
    let mut c = col();
    for r in 0..=last.min(rows.saturating_sub(1)) {
        let mut line = row().key(format!("term-row-{r}"));
        let runs = scr.runs_with_cursor(r, (r == cur_row).then_some(cur_col));
        if runs.is_empty() {
            line = line.child(text(" ").font(k.ty.mono));
        }
        for run in runs {
            let mut font = k.ty.mono;
            if run.style.bold {
                font.weight = 700.0;
            }
            let fg = run.style.fg.map_or(k.c.text_base, term_color);
            let mut t = text(run.text).font(font).color(fg);
            if let Some(bg) = run.style.bg {
                t = t.fill(term_color(bg));
            }
            if run.cursor {
                t = t.color(k.c.bg_base).fill(k.c.text_base);
            }
            line = line.child(t);
        }
        c = c.child(line);
    }
    c
}

fn pane(s: &App, k: &Kit) -> El {
    let body = match (&s.ui.selected, s.ui.selected_screen()) {
        (Some(_), Some(scr)) => {
            let (c, r) = scr.size();
            col()
                .grow(1.0)
                .w_full()
                .child(
                    screen_rows(k, scr)
                        .key("term")
                        .padding(8.0)
                        .scroll(ScrollSpec { stick_bottom: true })
                        .grow(1.0)
                        .w_full()
                        .fill(k.c.bg_base),
                )
                .child(
                    text(format!("{c}x{r} terminal (size follows the daemon's tmux pane); rows below the last line are hidden"))
                        .font(k.ty.small)
                        .color(k.c.text_faint)
                        .px(8.0),
                )
        }
        (Some(_), None) => hint(k, "Attached; waiting for output..."),
        _ => hint(k, "Select an agent on the left to attach."),
    };
    body.grow(1.0)
}

fn hint(k: &Kit, msg: &str) -> El {
    col()
        .center()
        .grow(1.0)
        .child(text(msg).font(k.ty.ui).color(k.c.text_faint))
}

pub fn view(s: &App, k: &Kit) -> El {
    row()
        .w_full()
        .grow(1.0)
        .child(sidebar(s, k))
        .child(pane(s, k))
}
