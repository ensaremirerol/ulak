//! Always-visible strip under the header: one chip per node.
use crate::app::App;
use rune::{col, el, row, text, El};
use rune_kit::Kit;
use ulak_proto::{BusyReason, NodeInfo, NodeState};

const BAR_W: f32 = 56.0;

/// (label, semantic colors) for the state pill.
pub fn state_label(s: &NodeState) -> String {
    match s {
        NodeState::Idle => "Idle".into(),
        NodeState::Busy(BusyReason::Gaming { process }) => format!("Busy: gaming ({process})"),
        NodeState::Busy(BusyReason::Load) => "Busy: load".into(),
        NodeState::Offline => "Offline".into(),
    }
}

/// "1 agent", "2 agents".
pub fn plural(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

pub fn pill(k: &Kit, label: String, c: &rune_kit::StateColors) -> El {
    row()
        .px(8.0)
        .h(18.0)
        .center()
        .radius(9.0)
        .fill(c.bg)
        .child(text(label).font(k.ty.small_medium).color(c.fg))
}

fn bar(k: &Kit, key: String, name: &str, pct: f32) -> El {
    let c = if pct > 90.0 {
        k.c.danger.icon
    } else if pct > 70.0 {
        k.c.warning.icon
    } else {
        k.c.success.icon
    };
    row()
        .gap(4.0)
        .center_y()
        .child(text(name).font(k.ty.small).color(k.c.text_muted))
        .child(
            el().key(key)
                .w(BAR_W)
                .h(5.0)
                .radius(2.5)
                .fill(k.c.layer_03)
                .child(
                    el().w(BAR_W * pct.clamp(0.0, 100.0) / 100.0)
                        .h(5.0)
                        .radius(2.5)
                        .fill(c),
                ),
        )
}

fn chip(k: &Kit, n: &NodeInfo) -> El {
    let id = n.id.as_str();
    let colors = match n.state {
        NodeState::Idle => &k.c.success,
        NodeState::Busy(_) => &k.c.warning,
        NodeState::Offline => &k.c.danger,
    };
    let t = &n.telemetry;
    let mut bars = row().gap(10.0).center_y();
    if n.state != NodeState::Offline {
        bars = bars.child(bar(k, format!("node-{id}-cpu"), "CPU", t.cpu_pct()));
        if let Some(g) = t.gpu_pct() {
            bars = bars.child(bar(k, format!("node-{id}-gpu"), "GPU", g));
        }
    }
    col()
        .key(format!("node-{id}"))
        .gap(4.0)
        .padding(8.0)
        .radius(8.0)
        .fill(k.c.layer_01)
        .border(1.0, k.c.border_base)
        .child(
            row()
                .gap(8.0)
                .center_y()
                .child(
                    text(n.hostname.clone())
                        .font(k.ty.ui_medium)
                        .color(k.c.text_base),
                )
                .child(pill(k, state_label(&n.state), colors).key(format!("node-{id}-state")))
                .child(
                    text(plural(n.agents.len(), "agent"))
                        .key(format!("node-{id}-agents"))
                        .font(k.ty.small)
                        .color(k.c.text_muted),
                ),
        )
        .child(bars)
}

pub fn view(s: &App, k: &Kit) -> El {
    let mut r = row().gap(8.0).px(12.0).py(8.0).w_full().fill(k.c.bg_deep);
    if s.ui.nodes.is_empty() {
        r = r.child(text("No nodes yet").font(k.ty.small).color(k.c.text_faint));
    }
    for n in &s.ui.nodes {
        r = r.child(chip(k, n));
    }
    r
}
