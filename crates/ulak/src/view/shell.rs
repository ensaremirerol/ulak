//! Window chrome: header (tabs, connection pill), node bar, the active panel, error toast.
//! To add a panel: add a `yourmod::view(s, &k)` arm in `view` below.
use super::{devices, mailbox, mux, nodebar, questions, spawn};
use crate::{
    app::App,
    model::{ConnStatus, Panel},
};
use rune::{col, row, text, Cx, El};
use rune_kit::{Badge, BadgeVariant, Button, ButtonVariant, Kit};

/// Tab order; Ctrl+1..5 follow it. `Panel::Terminal` is the Mux panel.
pub const TABS: [(Panel, &str); 5] = [
    (Panel::Terminal, "Mux"),
    (Panel::Questions, "Questions"),
    (Panel::Spawn, "Spawn"),
    (Panel::Mailbox, "Mailbox"),
    (Panel::Devices, "Devices"),
];

fn tab(s: &App, k: &Kit, i: usize) -> El {
    let (p, name) = TABS[i];
    let active = s.ui.panel == p || (p == Panel::Terminal && s.ui.panel == Panel::Fleet);
    let b = Button::new(name)
        .variant(if active {
            ButtonVariant::Contrast
        } else {
            ButtonVariant::Ghost
        })
        .on_press(move |s: &mut App| s.open_panel(p))
        .build(k)
        .key(format!("tab-{}", name.to_lowercase()));
    let mut r = row().gap(4.0).center_y().child(b);
    let open = s.ui.questions.len() + s.ui.blockers.len();
    if p == Panel::Questions && open > 0 {
        r = r.child(
            Badge::new(open.to_string())
                .variant(BadgeVariant::Accent)
                .build(k)
                .key("questions-badge"),
        );
    }
    r
}

fn conn_pill(s: &App, k: &Kit) -> El {
    let (label, c) = match s.ui.conn {
        ConnStatus::Connected => ("Connected", &k.c.success),
        _ => ("Reconnecting", &k.c.warning),
    };
    nodebar::pill(k, label.into(), c)
        .key("conn-pill")
        .label(label)
}

fn header(s: &App, k: &Kit) -> El {
    let mut r = row()
        .w_full()
        .gap(6.0)
        .px(12.0)
        .h(44.0)
        .center_y()
        .fill(k.c.bg_deep)
        .child(text("ulak").font(k.ty.body_medium).color(k.c.text_strong));
    for i in 0..TABS.len() {
        r = r.child(tab(s, k, i));
    }
    r.child(row().grow(1.0)).child(conn_pill(s, k))
}

fn toast(k: &Kit, msg: &str) -> El {
    row()
        .key("toast")
        .w_full()
        .gap(8.0)
        .px(12.0)
        .py(6.0)
        .center_y()
        .fill(k.c.danger.bg)
        .child(
            text(msg.to_string())
                .wrap()
                .grow(1.0)
                .font(k.ty.ui)
                .color(k.c.danger.fg),
        )
        .child(
            Button::new("Dismiss")
                .variant(ButtonVariant::Ghost)
                .on_press(|s: &mut App| s.ui.last_error = None)
                .build(k)
                .key("toast-dismiss"),
        )
}

/// "ulak 0.1.0 (abc) / hub 0.1.0 (def)".
pub fn version_line(s: &App) -> String {
    let fmt = |v: &ulak_proto::VersionInfo| format!("{} ({})", v.version(), v.git());
    let hub = s.ui.hub_version.as_ref().map_or("?".into(), fmt);
    format!("ulak {} / hub {hub}", fmt(&s.ui.own()))
}

fn footer(s: &App, k: &Kit) -> El {
    let mut r = row()
        .w_full()
        .gap(8.0)
        .px(12.0)
        .h(22.0)
        .center_y()
        .fill(k.c.bg_deep)
        .child(
            text(version_line(s))
                .key("version-line")
                .font(k.ty.small)
                .color(k.c.text_muted),
        );
    if !s.identity.is_empty() {
        r = r.child(
            text(s.identity.clone())
                .key("identity-line")
                .font(k.ty.small)
                .color(k.c.text_muted),
        );
    }
    if let Some(f) = s.ui.own_flag() {
        r = r.child(nodebar::pill(k, f.label().into(), &k.c.warning).key("version-flag"));
    }
    r
}

pub fn view(s: &App, _: &mut Cx<App>) -> El {
    let k = Kit::neutral(true);
    if s.login.visible {
        return crate::web_view::view(s, &k);
    }
    let body = match s.ui.panel {
        Panel::Terminal | Panel::Fleet => mux::view(s, &k),
        Panel::Questions => questions::view(s, &k),
        Panel::Spawn => spawn::view(s, &k),
        Panel::Mailbox => mailbox::view(s, &k),
        Panel::Devices => devices::view(s, &k),
    };
    let mut c = col()
        .w_full()
        .h_full()
        .fill(k.c.bg_base)
        .child(header(s, &k))
        .child(nodebar::view(s, &k))
        .child(body)
        .child(footer(s, &k));
    // The spawn dialog shows hub errors inline.
    if let (Some(m), false) = (&s.ui.last_error, s.ui.panel == Panel::Spawn) {
        c = c.child(toast(&k, m));
    }
    c
}
