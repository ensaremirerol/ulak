//! Rune views, one module per panel. Add a panel: a module with `pub fn view(&App, &Kit) -> El`,
//! a `TABS` entry and a match arm in `shell::view`. See PROGRESS.md.
pub mod devices;
pub mod keys;
pub mod mailbox;
pub mod mux;
pub mod nodebar;
pub mod nodes;
pub mod questions;
pub mod shell;
pub mod spawn;

pub use keys::on_key;
pub use shell::view;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::App,
        model::{apply, Event, Panel, SpawnForm},
        net::NetHandle,
        scenes,
    };
    use rune::{Env, Key, KeyEvent, Mods, Runtime};
    use std::sync::mpsc;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
    use ulak_proto::{
        AgentId, AgentStatus, BlockerId, BlockerKind, DeviceKind, HubToUi, Message, MessageId,
        NodeId, Otp, QuestionId, ThreadId, UiToHub,
    };

    type Rt = Runtime<App>;

    fn rt(setup: fn(&mut App)) -> (Rt, UnboundedReceiver<UiToHub>) {
        let (_etx, erx) = mpsc::channel();
        let (otx, orx) = unbounded_channel();
        let mut app = App {
            net: Some(NetHandle::new(erx, otx)),
            ..App::default()
        };
        setup(&mut app);
        let mut rt = Runtime::new(
            app,
            view,
            (1000.0, 650.0),
            Env {
                system_dark: true,
                scale: 1.0,
            },
        );
        rt.on_key = Some(on_key);
        rt.frame(0.0);
        (rt, orx)
    }

    fn click(rt: &mut Rt, key: &str) {
        let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no element {key}"));
        rt.pointer_move(x, y);
        rt.frame(0.0);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
    }

    fn press(rt: &mut Rt, ev: KeyEvent) {
        rt.key(ev);
        rt.frame(0.0);
    }

    fn has_text(rt: &Rt, t: &str) -> bool {
        rt.elements().iter().any(|e| e.text.as_deref() == Some(t))
    }

    fn drain(rx: &mut UnboundedReceiver<UiToHub>) -> Vec<UiToHub> {
        std::iter::from_fn(|| rx.try_recv().ok()).collect()
    }

    fn aid(s: &str) -> AgentId {
        AgentId::new(s).unwrap()
    }

    #[test]
    fn node_bar_shows_gaming_and_offline() {
        let (rt, _) = rt(scenes::mux);
        assert!(has_text(&rt, "Busy: gaming (eldenring.exe)"));
        assert!(has_text(&rt, "Offline"));
        assert!(has_text(&rt, "Idle"));
        assert!(rt.find("node-n2-gpu").is_some());
        assert!(rt.find("node-n3-cpu").is_none(), "offline node has no bars");
    }

    #[test]
    fn mux_lists_agents_and_select_attaches() {
        let (mut rt, mut orx) = rt(|a| scenes::base(a, Panel::Terminal));
        for id in ["claude-1", "claude-2", "codex-1"] {
            assert!(rt.find(&format!("agent-{id}")).is_some(), "{id}");
        }
        click(&mut rt, "agent-claude-2");
        assert_eq!(
            drain(&mut orx),
            vec![UiToHub::Attach {
                agent: aid("claude-2")
            }]
        );
        click(&mut rt, "agent-codex-1");
        assert_eq!(
            drain(&mut orx),
            vec![
                UiToHub::Detach {
                    agent: aid("claude-2")
                },
                UiToHub::Attach {
                    agent: aid("codex-1")
                }
            ]
        );
        click(&mut rt, "kill-codex-1");
        assert_eq!(
            drain(&mut orx),
            vec![UiToHub::Kill {
                agent: aid("codex-1")
            }]
        );
    }

    #[test]
    fn typing_in_pane_sends_pane_input() {
        let (mut rt, mut orx) = rt(scenes::mux);
        assert!(has_text(&rt, "cargo test") || rt.find("term-row-0").is_some());
        for key in [Key::Text("l".into()), Key::Enter] {
            press(
                &mut rt,
                KeyEvent {
                    key,
                    mods: Mods::default(),
                },
            );
        }
        let got: Vec<u8> = drain(&mut orx)
            .into_iter()
            .flat_map(|f| match f {
                UiToHub::PaneInput { agent, data } => {
                    assert_eq!(agent, aid("claude-1"));
                    data
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(got, b"l\r");
    }

    #[test]
    fn answering_sends_frame_and_close_drops_badge() {
        let (mut rt, mut orx) = rt(|a| scenes::base(a, Panel::Questions));
        let badge = |rt: &Rt| {
            rt.elements()
                .iter()
                .find(|e| e.key.as_deref() == Some("questions-badge"))
                .and_then(|e| e.label.clone())
        };
        assert_eq!(badge(&rt).as_deref(), Some("2"));
        click(&mut rt, "answer-q1");
        rt.paste("yes");
        press(
            &mut rt,
            KeyEvent {
                key: Key::Enter,
                mods: Mods::default(),
            },
        );
        let frames = drain(&mut orx);
        assert_eq!(frames.len(), 1, "{frames:?}");
        let UiToHub::Answer(a) = &frames[0] else {
            panic!("{frames:?}")
        };
        assert_eq!((a.question().as_str(), a.text()), ("q1", "yes"));
        rt.update(|s, _| {
            apply(
                &mut s.ui,
                Event::Hub(HubToUi::QuestionClosed {
                    id: QuestionId::new("q1").unwrap(),
                }),
            )
        });
        rt.frame(0.0);
        assert_eq!(badge(&rt).as_deref(), Some("1"));
        assert!(rt.find("question-q1").is_none());
    }

    #[test]
    fn shortcuts_switch_panels_and_refresh() {
        let (mut rt, mut orx) = rt(|a| scenes::base(a, Panel::Terminal));
        let ctrl = |c| KeyEvent {
            key: Key::Command(c),
            mods: Mods {
                ctrl: true,
                cmd: true,
                ..Mods::default()
            },
        };
        press(&mut rt, ctrl('2'));
        assert_eq!(rt.state.ui.panel, Panel::Questions);
        press(&mut rt, ctrl('4'));
        assert_eq!(rt.state.ui.panel, Panel::Mailbox);
        assert!(matches!(drain(&mut orx)[..], [UiToHub::MailboxLog { .. }]));
        press(&mut rt, ctrl('5'));
        assert_eq!(drain(&mut orx), vec![UiToHub::ListDevices]);
        click(&mut rt, "tab-mux");
        assert_eq!(rt.state.ui.panel, Panel::Terminal);
    }

    #[test]
    fn reconnecting_pill_and_error_toast() {
        let (mut rt, _) = rt(scenes::mux);
        assert!(has_text(&rt, "Connected"));
        rt.update(|s, _| {
            apply(&mut s.ui, Event::Disconnected("x".into()));
            s.ui.last_error = Some("boom".into());
        });
        rt.frame(0.0);
        assert!(has_text(&rt, "Reconnecting"));
        assert!(has_text(&rt, "boom"));
        rt.frame(1.0);
        click(&mut rt, "toast-dismiss");
        assert_eq!(rt.state.ui.last_error, None);
        rt.frame(1.0);
        assert!(!has_text(&rt, "boom"));
    }

    #[test]
    fn tick_refreshes_question_age() {
        let mut app = App::default();
        assert!(app.tick(1.0));
        let first = app.ui.now_ms;
        assert!(!app.tick(1.1), "throttled to TICK_SECS");
        assert!(app.tick(1.3));
        assert!(app.ui.now_ms >= first);
        assert_eq!(super::questions::age(125_000), "2m ago");
    }

    fn type_into(rt: &mut Rt, key: &str, t: &str) {
        click(rt, key);
        rt.paste(t);
        rt.frame(0.0);
    }

    #[test]
    fn spawn_form_sends_the_exact_frame_and_clears_on_success() {
        let (mut rt, mut orx) = rt(|a| scenes::base(a, Panel::Spawn));
        rt.frame(1.0);
        click(&mut rt, "target-n1");
        type_into(&mut rt, "spawn-config-dir", "/cfg");
        type_into(&mut rt, "spawn-workdir", "/w");
        type_into(&mut rt, "spawn-agent-id", "worker-9");
        type_into(&mut rt, "spawn-prompt", "line1\nline2");
        click(&mut rt, "spawn-submit");
        let frames = drain(&mut orx);
        let want = SpawnForm {
            node: "n1".into(),
            agent_id: "worker-9".into(),
            workdir: "/w".into(),
            prompt: "line1\nline2".into(),
            config_dir: "/cfg".into(),
            ..SpawnForm::default()
        }
        .to_frame()
        .unwrap();
        assert_eq!(frames, vec![want]);
        // The hub reports the agent: the form clears.
        rt.update(|s, _| {
            apply(
                &mut s.ui,
                Event::Hub(HubToUi::AgentStatus {
                    agent: aid("worker-9"),
                    node: NodeId::new("n1").unwrap(),
                    status: AgentStatus::Starting,
                }),
            );
            s.settle_spawn();
        });
        rt.frame(0.0);
        assert_eq!(rt.state.spawn.agent_id.text, "");
        assert_eq!(rt.state.spawn.prompt.text, "");
    }

    #[test]
    fn invalid_agent_id_blocks_sending_and_shows_the_error() {
        let (mut rt, mut orx) = rt(|a| scenes::base(a, Panel::Spawn));
        rt.frame(1.0);
        type_into(&mut rt, "spawn-agent-id", "Bad Id");
        assert!(rt.find("spawn-agent-id-error").is_some());
        type_into(&mut rt, "spawn-workdir", "/w");
        type_into(&mut rt, "spawn-prompt", "p");
        click(&mut rt, "spawn-submit");
        assert!(drain(&mut orx).is_empty());
        assert!(rt.state.ui.last_error.is_some());
        assert!(rt.find("spawn-error").is_some());
    }

    #[test]
    fn hub_error_shows_in_dialog_and_keeps_the_form() {
        let (mut rt, mut orx) = rt(|a| scenes::base(a, Panel::Spawn));
        rt.frame(1.0);
        click(&mut rt, "target-n2");
        type_into(&mut rt, "spawn-workdir", "/w");
        type_into(&mut rt, "spawn-agent-id", "w1");
        type_into(&mut rt, "spawn-prompt", "p");
        click(&mut rt, "spawn-submit");
        assert_eq!(drain(&mut orx).len(), 1, "busy node is still choosable");
        rt.update(|s, _| {
            apply(
                &mut s.ui,
                Event::Hub(HubToUi::Error {
                    message: "node n2 is not eligible".into(),
                }),
            );
            s.settle_spawn();
        });
        rt.frame(0.0);
        assert!(has_text(&rt, "node n2 is not eligible"));
        assert!(
            rt.find("toast").is_none(),
            "no duplicate toast on this panel"
        );
        assert_eq!(rt.state.spawn.agent_id.text, "w1");
        assert!(rt.state.spawn.pending.is_none());
    }

    fn msg(id: &str, th: &str) -> Message {
        Message::new(
            MessageId::new(id).unwrap(),
            aid("a1"),
            aid("a2"),
            ThreadId::new(th).unwrap(),
            "hi".into(),
            1,
        )
        .unwrap()
    }

    #[test]
    fn mailbox_thread_filter_narrows_and_clears_and_live_appends() {
        let (mut rt, mut orx) = rt(scenes::mailbox);
        rt.frame(1.0);
        let rows = |rt: &Rt| {
            rt.elements()
                .iter()
                .filter(|e| e.key.as_deref().is_some_and(|k| k.starts_with("msg-")))
                .count()
        };
        assert_eq!(rows(&rt), 3);
        click(&mut rt, "thread-m3");
        rt.frame(1.0);
        assert_eq!(rows(&rt), 1);
        assert!(has_text(&rt, "Mailbox (1 of 3)"));
        rt.update(|s, _| {
            apply(
                &mut s.ui,
                Event::Hub(HubToUi::MessageLogged(msg("m9", "build"))),
            )
        });
        rt.frame(0.0);
        assert_eq!(rows(&rt), 2, "live message in the filtered thread appears");
        rt.update(|s, _| {
            apply(
                &mut s.ui,
                Event::Hub(HubToUi::MessageLogged(msg("m10", "other"))),
            )
        });
        rt.frame(0.0);
        assert_eq!(rows(&rt), 2, "other thread stays hidden");
        click(&mut rt, "clear-filter");
        assert_eq!(rows(&rt), 5);
        click(&mut rt, "mailbox-refresh");
        assert!(matches!(
            drain(&mut orx)[..],
            [UiToHub::MailboxLog { thread: None, .. }]
        ));
    }

    #[test]
    fn add_device_shows_otp_counts_down_and_expires() {
        let (mut rt, mut orx) = rt(|a| scenes::base(a, Panel::Devices));
        rt.frame(1.0);
        assert!(has_text(&rt, "No devices enrolled yet. Choose a kind and press Add device to get a one-time code, then enroll the machine with it."));
        click(&mut rt, "add-device");
        assert_eq!(
            drain(&mut orx),
            vec![UiToHub::AddDevice {
                kind: DeviceKind::Node
            }]
        );
        let t0 = rt.state.ui.now_ms;
        rt.update(|s, _| {
            apply(
                &mut s.ui,
                Event::Hub(HubToUi::OtpOffer {
                    kind: DeviceKind::Node,
                    otp: Otp::new("ABCD1234").unwrap(),
                    hub_fingerprint: "AA:BB".into(),
                    expires_ms: t0 + 90_000,
                }),
            )
        });
        rt.frame(0.0);
        assert!(has_text(&rt, "ABCD 1234"));
        assert!(has_text(&rt, "Hub fingerprint: AA:BB"));
        assert!(has_text(&rt, "Expires in 1:30"));
        rt.update(|s, _| apply(&mut s.ui, Event::Tick(t0 + 31_000)));
        rt.frame(0.0);
        assert!(has_text(&rt, "Expires in 0:59"));
        rt.update(|s, _| apply(&mut s.ui, Event::Tick(t0 + 500_000)));
        rt.frame(0.0);
        assert!(has_text(&rt, "Expired"), "never negative");
    }

    #[test]
    fn revoke_needs_two_clicks() {
        let (mut rt, mut orx) = rt(scenes::devices);
        rt.frame(1.0);
        assert!(
            rt.find("revoke-old-node").is_none(),
            "revoked device has no button"
        );
        click(&mut rt, "revoke-n1");
        assert!(drain(&mut orx).is_empty());
        assert!(has_text(&rt, "Click again to revoke"));
        click(&mut rt, "revoke-n1");
        assert_eq!(
            drain(&mut orx),
            vec![UiToHub::RevokeDevice {
                id: NodeId::new("n1").unwrap()
            }]
        );
        assert_eq!(rt.state.revoke_armed, None);
    }

    #[test]
    fn small_fixes_plural_starting_and_toast_expiry() {
        assert_eq!(nodebar::plural(1, "agent"), "1 agent");
        assert_eq!(nodebar::plural(0, "agent"), "0 agents");
        let (mut rt, _) = rt(|a| scenes::base(a, Panel::Terminal));
        rt.update(|s, _| {
            s.ui.statuses.clear();
            s.ui.last_error = Some("boom".into());
        });
        rt.frame(0.0);
        let label = |rt: &Rt| {
            rt.elements()
                .iter()
                .find(|e| e.key.as_deref() == Some("agent-claude-1"))
                .and_then(|e| e.label.clone())
        };
        assert_eq!(label(&rt).as_deref(), Some("claude-1 (starting)"));
        let t0 = rt.state.ui.now_ms;
        rt.update(|s, _| apply(&mut s.ui, Event::Tick(t0 + 1)));
        rt.update(|s, _| apply(&mut s.ui, Event::Tick(t0 + 1 + crate::model::ERROR_TTL_MS)));
        rt.frame(0.0);
        assert!(!has_text(&rt, "boom"));
    }

    fn bl(id: &str, node: &str, kind: BlockerKind, agent: &str) -> ulak_proto::Blocker {
        ulak_proto::Blocker::new(
            BlockerId::new(id).unwrap(),
            NodeId::new(node).unwrap(),
            ulak_proto::ProfileKey::new("claude").unwrap(),
            kind,
            Some(aid(agent)),
            "Run /login".into(),
            1,
        )
    }

    fn open_blocker(rt: &mut Rt, b: ulak_proto::Blocker) {
        rt.update(|s, _| apply(&mut s.ui, Event::Hub(HubToUi::BlockerOpened(b))));
        rt.frame(1.0);
    }

    #[test]
    fn blocker_cards_badge_open_agent_and_two_click_resolve() {
        let (mut rt, mut orx) = rt(|a| scenes::base(a, Panel::Questions));
        open_blocker(&mut rt, bl("b1", "n1", BlockerKind::Login, "claude-2"));
        open_blocker(&mut rt, bl("b2", "n2", BlockerKind::Install, "codex-1"));
        assert!(has_text(&rt, "Login required") && has_text(&rt, "Install failed"));
        assert!(has_text(&rt, "n1 / claude"));
        assert!(rt.find("blocker-open-b1").is_some());
        assert!(
            rt.find("blocker-open-b2").is_none(),
            "install cards have no Open agent"
        );
        let badge = rt
            .elements()
            .iter()
            .find(|e| e.key.as_deref() == Some("questions-badge"))
            .and_then(|e| e.label.clone());
        assert_eq!(badge.as_deref(), Some("4"), "2 questions + 2 blockers");
        // Never auto-dismissed.
        let t = rt.state.ui.now_ms + 10_000_000;
        rt.update(|s, _| apply(&mut s.ui, Event::Tick(t)));
        rt.frame(0.0);
        assert!(rt.find("blocker-b1").is_some());
        assert!(rt.find("node-n1-blocker").is_some());
        click(&mut rt, "blocker-resolve-b1");
        assert!(drain(&mut orx).is_empty(), "first click only arms");
        assert!(has_text(&rt, "Click again to confirm"));
        click(&mut rt, "blocker-resolve-b1");
        assert_eq!(
            drain(&mut orx),
            vec![UiToHub::ResolveBlocker {
                id: BlockerId::new("b1").unwrap()
            }]
        );
        click(&mut rt, "blocker-open-b1");
        assert_eq!(rt.state.ui.selected, Some(aid("claude-2")));
        assert_eq!(rt.state.ui.panel, Panel::Terminal);
    }

    #[test]
    fn blocked_node_disables_spawn_until_the_blocker_closes() {
        let (mut rt, mut orx) = rt(|a| scenes::base(a, Panel::Spawn));
        rt.frame(1.0);
        type_into(&mut rt, "spawn-agent-id", "w1");
        type_into(&mut rt, "spawn-workdir", "/w");
        type_into(&mut rt, "spawn-prompt", "p");
        click(&mut rt, "target-n1");
        open_blocker(&mut rt, bl("b1", "n1", BlockerKind::Login, "claude-2"));
        assert!(rt.find("spawn-blocked").is_some());
        assert!(has_text(&rt, "workstation - login required for claude"));
        click(&mut rt, "spawn-submit");
        assert!(drain(&mut orx).is_empty(), "disabled button sends nothing");
        rt.update(|s, _| s.submit_spawn());
        assert!(drain(&mut orx).is_empty(), "submit_spawn refuses too");
        rt.update(|s, _| {
            apply(
                &mut s.ui,
                Event::Hub(HubToUi::BlockerClosed {
                    id: BlockerId::new("b1").unwrap(),
                }),
            )
        });
        rt.update(|s, _| s.ui.last_error = None);
        rt.frame(1.0);
        assert!(rt.find("spawn-blocked").is_none());
        click(&mut rt, "spawn-submit");
        assert_eq!(drain(&mut orx).len(), 1);
    }

    #[test]
    fn auto_target_notes_excluded_nodes() {
        let (mut rt, _) = rt(|a| scenes::base(a, Panel::Spawn));
        open_blocker(&mut rt, bl("b1", "n2", BlockerKind::Login, "codex-1"));
        assert!(rt.find("spawn-auto-note").is_some());
        assert!(
            rt.find("spawn-blocked").is_none(),
            "n1 is still idle and free"
        );
        open_blocker(&mut rt, bl("b2", "n1", BlockerKind::Login, "claude-1"));
        assert!(
            rt.find("spawn-blocked").is_some(),
            "every idle node blocked"
        );
    }

    #[test]
    fn mux_labels_installing_and_needs_login() {
        let (mut rt, _) = rt(|a| scenes::base(a, Panel::Terminal));
        rt.update(|s, _| {
            s.ui.statuses
                .insert(aid("claude-1"), AgentStatus::NeedsLogin);
            s.ui.statuses
                .insert(aid("claude-2"), AgentStatus::Installing);
        });
        rt.frame(0.0);
        let label = |rt: &Rt, k: &str| {
            rt.elements()
                .iter()
                .find(|e| e.key.as_deref() == Some(k))
                .and_then(|e| e.label.clone())
        };
        assert_eq!(
            label(&rt, "agent-claude-1").as_deref(),
            Some("claude-1 (needs login)")
        );
        assert_eq!(
            label(&rt, "agent-claude-2").as_deref(),
            Some("claude-2 (installing)")
        );
    }

    fn vi(ver: &str, git: &str, proto: u32) -> ulak_proto::VersionInfo {
        ulak_proto::VersionInfo::new(ver, git, proto, "linux", "x86_64").unwrap()
    }

    fn up(rt: &mut Rt, node: &str, f: HubToUi) {
        let _ = node;
        rt.update(|s, _| apply(&mut s.ui, Event::Hub(f)));
        rt.frame(1.0);
    }

    fn status(node: &str, status: ulak_proto::UpdateState) -> HubToUi {
        HubToUi::UpdateStatus {
            node: NodeId::new(node).unwrap(),
            status,
        }
    }

    fn devices_rt() -> (Rt, UnboundedReceiver<UiToHub>) {
        let (mut rt, rx) = rt(scenes::updates);
        // Start from a clean slate: no versions or statuses.
        rt.update(|s, _| {
            s.ui.node_versions.clear();
            s.ui.updates.clear();
        });
        rt.frame(1.0);
        (rt, rx)
    }

    #[test]
    fn version_line_and_marker() {
        let (mut rt, _rx) = devices_rt();
        assert!(has_text(&rt, "ulak 0.2.0 (def456) / hub 0.2.0 (def456)"));
        assert!(rt.find("version-flag").is_none());
        up(&mut rt, "", HubToUi::HubVersion(vi("0.3.0", "zzz", 1)));
        assert!(has_text(&rt, "ulak 0.2.0 (def456) / hub 0.3.0 (zzz)"));
        assert!(has_text(&rt, "differs from hub"));
        rt.update(|s, _| s.ui.own_override = Some(vi("0.3.0", "zzz", 0)));
        rt.frame(1.0);
        assert!(has_text(&rt, "protocol too old"));
    }

    #[test]
    fn per_row_buttons_send_exact_frames_with_two_click_update() {
        use ulak_proto::UpdateState as U;
        let (mut rt, mut rx) = devices_rt();
        let n1 = NodeId::new("n1").unwrap();
        up(
            &mut rt,
            "n1",
            HubToUi::NodeVersion {
                node: n1.clone(),
                info: vi("0.1.0", "abc", 1),
            },
        );
        assert!(has_text(&rt, "differs from hub"));
        click(&mut rt, "check-n1");
        assert_eq!(
            drain(&mut rx),
            vec![UiToHub::CheckUpdate {
                node: Some(n1.clone())
            }]
        );
        click(&mut rt, "update-n1");
        assert!(drain(&mut rx).is_empty(), "not available: disabled");
        up(
            &mut rt,
            "n1",
            status(
                "n1",
                U::Available {
                    version: "0.2.0".into(),
                },
            ),
        );
        assert!(has_text(&rt, "Available 0.2.0"));
        assert!(rt.find("node-n1-update").is_some(), "chip badge");
        click(&mut rt, "update-n1");
        assert!(drain(&mut rx).is_empty(), "first click only arms");
        assert!(has_text(&rt, "Click again to update"));
        click(&mut rt, "update-n1");
        assert_eq!(drain(&mut rx), vec![UiToHub::UpdateNode { node: Some(n1) }]);
        assert_eq!(rt.state.update_armed, None);
    }

    #[test]
    fn all_buttons_send_none() {
        use ulak_proto::UpdateState as U;
        let (mut rt, mut rx) = devices_rt();
        click(&mut rt, "check-all");
        assert_eq!(drain(&mut rx), vec![UiToHub::CheckUpdate { node: None }]);
        click(&mut rt, "update-all");
        assert!(drain(&mut rx).is_empty(), "nothing available: disabled");
        up(
            &mut rt,
            "n1",
            status(
                "n1",
                U::Available {
                    version: "0.2.0".into(),
                },
            ),
        );
        click(&mut rt, "update-all");
        assert!(drain(&mut rx).is_empty());
        click(&mut rt, "update-all");
        assert_eq!(drain(&mut rx), vec![UiToHub::UpdateNode { node: None }]);
    }

    #[test]
    fn every_update_state_has_a_pill_and_failed_keeps_full_text() {
        use ulak_proto::UpdateState as U;
        let (mut rt, _rx) = devices_rt();
        let long = "x".repeat(200);
        for (st, want) in [
            (
                U::UpToDate {
                    checked_ms: 3_723_000,
                },
                "Up to date, checked 01:02:03",
            ),
            (
                U::Available {
                    version: "0.9.0".into(),
                },
                "Available 0.9.0",
            ),
            (U::Checking, "Checking"),
            (U::Downloading, "Downloading"),
            (U::Verifying, "Verifying"),
            (U::Installing, "Installing"),
            (U::Restarting, "Restarting"),
        ] {
            up(&mut rt, "n1", status("n1", st));
            assert!(has_text(&rt, want), "{want}");
        }
        up(&mut rt, "n1", status("n1", U::failed(&long)));
        assert!(has_text(&rt, &format!("Failed: {}...", "x".repeat(30))));
        assert!(has_text(&rt, &long), "full text on the second line");
    }

    #[test]
    fn offline_node_loses_its_update_status() {
        use ulak_proto::UpdateState as U;
        let (mut rt, _rx) = devices_rt();
        up(&mut rt, "n1", status("n1", U::Checking));
        assert!(has_text(&rt, "Checking"));
        let mut n = rt.state.ui.nodes[0].clone();
        n.state = ulak_proto::NodeState::Offline;
        up(&mut rt, "n1", HubToUi::NodeUpdate(n));
        assert!(!has_text(&rt, "Checking"));
    }
}
