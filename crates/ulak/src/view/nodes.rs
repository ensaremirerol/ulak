//! Nodes table (inside the Devices panel): version, platform, update status, Check/Update buttons.
use super::{
    mailbox::clock,
    nodebar::{pill, state_label},
};
use crate::{
    app::App,
    model::{available, Intent},
};
use rune::{col, row, text, El};
use rune_kit::{Button, ButtonVariant, Kit, StateColors};
use ulak_proto::{NodeInfo, NodeState, UpdateState};

const SHORT: usize = 30;

/// Pill text of an update state. A failure message is cut to `SHORT` chars (the view shows the full text on a second line).
pub fn status_label(u: &UpdateState) -> String {
    match u {
        UpdateState::UpToDate { checked_ms } => {
            format!("Up to date, checked {}", clock(*checked_ms))
        }
        UpdateState::Available { version } => format!("Available {version}"),
        UpdateState::Checking => "Checking".into(),
        UpdateState::Downloading => "Downloading".into(),
        UpdateState::Verifying => "Verifying".into(),
        UpdateState::Installing => "Installing".into(),
        UpdateState::Restarting => "Restarting".into(),
        UpdateState::Failed { message } => {
            let cut: String = message.chars().take(SHORT).collect();
            if cut.len() < message.len() {
                format!("Failed: {cut}...")
            } else {
                format!("Failed: {cut}")
            }
        }
    }
}

fn colors<'a>(k: &'a Kit, u: &UpdateState) -> &'a StateColors {
    match u {
        UpdateState::UpToDate { .. } => &k.c.success,
        UpdateState::Failed { .. } => &k.c.danger,
        _ => &k.c.info,
    }
}

fn cell(k: &Kit, w: f32, t: String) -> El {
    text(t).w(w).font(k.ty.ui).color(k.c.text_base)
}

fn node_row(s: &App, k: &Kit, n: &NodeInfo) -> El {
    let id = n.id.as_str();
    let online = n.state != NodeState::Offline;
    let up = s.ui.updates.get(&n.id);
    let (ver, plat) = match s.ui.node_versions.get(&n.id) {
        Some(v) => (
            format!("{} ({})", v.version(), v.git()),
            format!("{}/{}", v.os(), v.arch()),
        ),
        None => ("unknown".into(), "-".into()),
    };
    let mut vcol = col().w(160.0).gap(2.0).child(
        text(ver)
            .key(format!("nver-{id}"))
            .font(k.ty.ui)
            .color(k.c.text_base),
    );
    vcol = vcol.child(text(plat).font(k.ty.small).color(k.c.text_muted));
    if let Some(f) = s.ui.node_flag(&n.id) {
        vcol = vcol.child(
            text(f.label())
                .key(format!("nflag-{id}"))
                .font(k.ty.small)
                .color(k.c.warning.fg),
        );
    }
    let status = match up {
        Some(u) => pill(k, status_label(u), colors(k, u)).key(format!("nstatus-{id}")),
        None => text("-").font(k.ty.small).color(k.c.text_faint),
    };
    let target = n.id.clone();
    let check = Button::new("Check")
        .variant(ButtonVariant::Outline)
        .disabled(!online)
        .on_press(move |s: &mut App| {
            s.dispatch(Intent::CheckUpdate {
                node: Some(target.clone()),
            })
        })
        .build(k)
        .key(format!("check-{id}"));
    let armed = s.update_armed.as_ref() == Some(&Some(n.id.clone()));
    let target = n.id.clone();
    let update = Button::new(if armed {
        "Click again to update"
    } else {
        "Update"
    })
    .variant(ButtonVariant::Danger)
    .disabled(!(online && up.and_then(available).is_some()))
    .on_press(move |s: &mut App| s.update_click(Some(target.clone())))
    .build(k)
    .key(format!("update-{id}"));
    let mut top = row()
        .gap(12.0)
        .center_y()
        .w_full()
        .child(cell(k, 130.0, n.hostname.clone()))
        .child(row().w(190.0).child(pill(
            k,
            state_label(&n.state),
            match n.state {
                NodeState::Idle => &k.c.success,
                NodeState::Busy(_) => &k.c.warning,
                NodeState::Offline => &k.c.danger,
            },
        )))
        .child(vcol)
        .child(row().grow(1.0).child(status))
        .child(check)
        .child(update);
    if armed {
        top = top.child(
            Button::new("Cancel")
                .variant(ButtonVariant::Ghost)
                .on_press(|s: &mut App| s.update_armed = None)
                .build(k)
                .key(format!("update-cancel-{id}")),
        );
    }
    let mut c = col()
        .key(format!("nrow-{id}"))
        .gap(4.0)
        .px(10.0)
        .py(6.0)
        .w_full()
        .radius(6.0)
        .fill(k.c.layer_01)
        .child(top);
    if let Some(UpdateState::Failed { message }) = up {
        c = c.child(
            text(message.clone())
                .key(format!("nfail-{id}"))
                .wrap()
                .font(k.ty.mono)
                .color(k.c.danger.fg),
        );
    }
    c
}

fn all_button(s: &App, k: &Kit) -> El {
    let any = s.ui.nodes.iter().any(|n| n.state != NodeState::Offline);
    let armed = s.update_armed == Some(None);
    let mut r = row()
        .gap(8.0)
        .center_y()
        .child(
            Button::new("Check all")
                .variant(ButtonVariant::Outline)
                .disabled(!any)
                .on_press(|s: &mut App| s.dispatch(Intent::CheckUpdate { node: None }))
                .build(k)
                .key("check-all"),
        )
        .child(
            Button::new(if armed {
                "Click again to update all"
            } else {
                "Update all"
            })
            .variant(ButtonVariant::Danger)
            .disabled(!s.ui.updates_available())
            .on_press(|s: &mut App| s.update_click(None))
            .build(k)
            .key("update-all"),
        );
    if armed {
        r = r.child(
            Button::new("Cancel")
                .variant(ButtonVariant::Ghost)
                .on_press(|s: &mut App| s.update_armed = None)
                .build(k)
                .key("update-all-cancel"),
        );
    }
    r
}

pub fn view(s: &App, k: &Kit) -> El {
    let mut c = col().key("nodes").gap(8.0).w_full().child(
        row()
            .gap(12.0)
            .center_y()
            .child(text("Nodes").font(k.ty.body_medium).color(k.c.text_strong))
            .child(all_button(s, k)),
    );
    if s.ui.nodes.is_empty() {
        c = c.child(text("No nodes yet").font(k.ty.ui).color(k.c.text_faint));
    }
    for n in &s.ui.nodes {
        c = c.child(node_row(s, k, n));
    }
    c
}
