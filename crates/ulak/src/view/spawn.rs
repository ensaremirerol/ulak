//! Spawn dialog: profile, optional config dir, workdir, agent id, prompt, target node.
//! Hub `Error` frames (and local validation errors) show inline via `ui.last_error`; the form
//! clears when the agent appears (`App::settle_spawn`).
use super::nodebar::state_label;
use crate::{
    app::{App, PROFILES},
    model::kind_title,
};
use rune::{col, row, text, El, ScrollSpec};
use rune_kit::{Button, ButtonVariant, Kit, SegmentedControl, TextArea, TextInput};
use ulak_proto::{BlockerKind, NodeInfo, NodeState};

fn field(k: &Kit, label: &str, input: El) -> El {
    col()
        .gap(4.0)
        .w_full()
        .child(text(label).font(k.ty.small_medium).color(k.c.text_muted))
        .child(input)
}

fn target_button(k: &Kit, s: &App, n: Option<&NodeInfo>) -> El {
    let (key, label) = match n {
        None => ("target-auto".to_owned(), "Auto".to_owned()),
        Some(n) => {
            let mut note = match n.state {
                NodeState::Idle => String::new(),
                _ => format!(" - not eligible: {}", state_label(&n.state)),
            };
            if let Some(b) = s.spawn.profile_key().and_then(|key| {
                s.ui.blockers_for(Some(&n.id), &key)
                    .first()
                    .map(|b| (*b).clone())
            }) {
                let name = b.profile().as_str().split('@').next().unwrap_or_default();
                note = match b.kind() {
                    BlockerKind::Login => format!(" - login required for {name}"),
                    BlockerKind::Install => format!(" - install failed for {name}"),
                };
            }
            (
                format!("target-{}", n.id.as_str()),
                format!("{}{note}", n.hostname),
            )
        }
    };
    let id = n.map(|n| n.id.clone());
    let sel = s.spawn.node == id;
    Button::new(label)
        .variant(if sel {
            ButtonVariant::Contrast
        } else {
            ButtonVariant::Outline
        })
        .on_press(move |s: &mut App| s.spawn.node = id.clone())
        .build(k)
        .key(key)
}

pub fn view(s: &App, k: &Kit) -> El {
    let sp = &s.spawn;
    let profile = SegmentedControl::new(
        &PROFILES.map(|p| p.0),
        Some(sp.profile.min(PROFILES.len() - 1)),
    )
    .fit()
    .label("Profile")
    .on_pick(|s: &mut App, i, _| s.spawn.profile = i)
    .build(k)
    .key("spawn-profile");
    let mut f = col().gap(12.0).w(460.0);
    f = f.child(field(k, "Profile", profile));
    if PROFILES[sp.profile.min(PROFILES.len() - 1)].1.is_empty() {
        f = f.child(field(
            k,
            "Binary",
            TextInput::new(&sp.binary)
                .placeholder("/usr/local/bin/my-agent")
                .width(None)
                .on_edit(|s: &mut App| &mut s.spawn.binary)
                .build(k)
                .key("spawn-binary"),
        ));
    }
    f = f
        .child(field(
            k,
            "CLAUDE_CONFIG_DIR (optional)",
            TextInput::new(&sp.config_dir)
                .placeholder("~/.claude-work")
                .width(None)
                .on_edit(|s: &mut App| &mut s.spawn.config_dir)
                .build(k)
                .key("spawn-config-dir"),
        ))
        .child(field(
            k,
            "Working directory",
            TextInput::new(&sp.workdir)
                .placeholder("/home/me/project")
                .width(None)
                .on_edit(|s: &mut App| &mut s.spawn.workdir)
                .build(k)
                .key("spawn-workdir"),
        ));
    let mut id_in = TextInput::new(&sp.agent_id)
        .placeholder("worker-1")
        .width(None)
        .on_edit(|s: &mut App| &mut s.spawn.agent_id);
    let id_err = sp.agent_id_error();
    id_in = id_in.invalid(id_err.is_some());
    let mut id_col = col()
        .gap(4.0)
        .w_full()
        .child(id_in.build(k).key("spawn-agent-id"));
    if let Some(e) = id_err {
        id_col = id_col.child(
            text(e)
                .key("spawn-agent-id-error")
                .font(k.ty.small)
                .color(k.c.danger.fg),
        );
    }
    f = f.child(field(k, "Agent id", id_col));
    let mut g = col().gap(12.0).w(460.0);
    g = g.child(field(
        k,
        "Prompt",
        TextArea::new(&sp.prompt)
            .placeholder("What should the agent do?")
            .width(None)
            .rows(3)
            .on_edit(|s: &mut App| &mut s.spawn.prompt)
            .build(k)
            .key("spawn-prompt"),
    ));
    let mut targets = col().gap(6.0).child(target_button(k, s, None));
    for n in &s.ui.nodes {
        targets = targets.child(target_button(k, s, Some(n)));
    }
    g = g.child(field(k, "Target node", targets));
    let blocker = sp.profile_key().and_then(|_| s.spawn_blocker());
    if let Some(b) = blocker {
        g = g.child(
            text(format!(
                "{} on {} ({}): {} Resolve it in the Questions tab first.",
                kind_title(b.kind()),
                b.node().as_str(),
                b.profile().as_str(),
                b.detail().chars().take(120).collect::<String>()
            ))
            .key("spawn-blocked")
            .wrap()
            .font(k.ty.ui)
            .color(k.c.warning.fg),
        );
    } else if let (None, Some(key)) = (&sp.node, sp.profile_key()) {
        let mut ex: Vec<&str> =
            s.ui.blockers_for(None, &key)
                .into_iter()
                .map(|b| b.node().as_str())
                .collect();
        ex.dedup();
        if !ex.is_empty() {
            g = g.child(
                text(format!(
                    "Auto skips {} because of blockers for {key}.",
                    ex.join(", ")
                ))
                .key("spawn-auto-note")
                .wrap()
                .font(k.ty.small)
                .color(k.c.warning.fg),
            );
        }
    }
    if let Some(e) = &s.ui.last_error {
        g = g.child(
            text(e.clone())
                .key("spawn-error")
                .wrap()
                .font(k.ty.ui)
                .color(k.c.danger.fg),
        );
    }
    g = g.child(
        row().child(
            Button::new(if sp.pending.is_some() {
                "Spawning..."
            } else {
                "Spawn"
            })
            .variant(ButtonVariant::Submit)
            .disabled(blocker.is_some())
            .on_press(|s: &mut App| s.submit_spawn())
            .build(k)
            .key("spawn-submit"),
        ),
    );
    col()
        .w_full()
        .padding(16.0)
        .child(
            row()
                .gap(24.0)
                .padding(16.0)
                .radius(8.0)
                .fill(k.c.layer_01)
                .border(1.0, k.c.border_base)
                .child(f)
                .child(g),
        )
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .grow(1.0)
}
