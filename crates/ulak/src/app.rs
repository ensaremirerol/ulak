//! The Rune app state: pure `State` plus the (optional) hub connection. Views take `&App`
//! and call `App::dispatch`; `poll` drains net events each frame.
#[cfg(not(target_arch = "wasm32"))]
use crate::net::{config_dir, pick_identity, start_in, IdentitySource};
use crate::{
    model::{apply, ConnStatus, Event, Intent, Panel, SpawnForm, State},
    net::NetHandle,
};
#[cfg(not(target_arch = "wasm32"))]
use rune::Waker;
use rune::{TextEdit, Tx};
use std::collections::BTreeMap;
use ulak_proto::{
    AgentId, AgentProfile, Blocker, BlockerId, DeviceKind, NodeId, NodeState, QuestionId, ThreadId,
    UiToHub,
};
#[cfg(not(target_arch = "wasm32"))]
use ulakd::config::Identity;

/// Wall clock in unix ms (`SystemTime` panics on wasm32-unknown-unknown).
fn now_ms() -> u64 {
    #[cfg(target_arch = "wasm32")]
    return js_sys::Date::now() as u64;
    #[cfg(not(target_arch = "wasm32"))]
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Seconds between `Event::Tick`s (question ages refresh at this rate).
pub const TICK_SECS: f64 = 0.25;

/// Intents queued while disconnected (oldest are kept; the rest is refused with a toast).
pub const OFFLINE_QUEUE_MAX: usize = 32;

/// Profiles offered by the spawn dialog: (label, binary); the last one takes the binary from a field.
pub const PROFILES: [(&str, &str); 3] = [
    ("claude", "claude"),
    ("antigravity", "antigravity"),
    ("custom", ""),
];
pub const DEVICE_KINDS: [(&str, DeviceKind); 4] = [
    // Same code works for `ulakd join` and `ulak join`; recorded as a node (display only).
    ("device", DeviceKind::Node),
    ("node", DeviceKind::Node),
    ("ui", DeviceKind::Ui),
    ("browser", DeviceKind::Browser),
];

/// Spawn dialog fields (view state, not hub state).
#[derive(Default)]
pub struct SpawnUi {
    pub profile: usize,
    pub binary: TextEdit,
    pub config_dir: TextEdit,
    pub workdir: TextEdit,
    pub agent_id: TextEdit,
    pub prompt: TextEdit,
    /// `None` = Auto.
    pub node: Option<NodeId>,
    /// Agent we sent a Spawn for and wait to see appear (or fail).
    pub pending: Option<AgentId>,
}

impl SpawnUi {
    pub fn form(&self) -> SpawnForm {
        let binary = match PROFILES[self.profile.min(PROFILES.len() - 1)] {
            (_, "") => self.binary.text.clone(),
            (_, b) => b.to_owned(),
        };
        SpawnForm {
            node: self
                .node
                .as_ref()
                .map(|n| n.as_str().to_owned())
                .unwrap_or_default(),
            agent_id: self.agent_id.text.clone(),
            binary,
            args: String::new(),
            workdir: self.workdir.text.clone(),
            prompt: self.prompt.text.clone(),
            config_dir: self.config_dir.text.clone(),
        }
    }

    /// Inline error for the agent id field (empty is not an error until Spawn is pressed).
    pub fn agent_id_error(&self) -> Option<String> {
        let t = self.agent_id.text.trim();
        if t.is_empty() {
            return None;
        }
        AgentId::new(t).err().map(|e| e.to_string())
    }

    /// Key of the profile the form would spawn (`claude`, `claude@/dir`); None while invalid.
    pub fn profile_key(&self) -> Option<String> {
        let f = self.form();
        let dir = Some(f.config_dir.trim()).filter(|d| !d.is_empty());
        AgentProfile::new(f.binary.trim(), vec![], dir.map(Into::into))
            .ok()?
            .key()
            .ok()
            .map(|k| k.as_str().to_owned())
    }

    fn clear(&mut self) {
        for e in [
            &mut self.binary,
            &mut self.config_dir,
            &mut self.workdir,
            &mut self.agent_id,
            &mut self.prompt,
        ] {
            e.clear();
        }
        self.pending = None;
    }
}

#[derive(Default)]
pub struct App {
    pub ui: State,
    pub net: Option<NetHandle>,
    /// Footer label for the identity in use (empty when not connected).
    pub identity: String,
    /// Draft answer per pending question (view state, not hub state).
    pub answers: BTreeMap<QuestionId, TextEdit>,
    pub last_tick: f64,
    pub spawn: SpawnUi,
    /// Mailbox thread filter (client side; the list is `ui.mailbox`).
    pub thread_filter: Option<ThreadId>,
    /// Index into `DEVICE_KINDS` for the next "Add device".
    pub device_kind: usize,
    /// Device whose Revoke was clicked once (second click confirms).
    pub revoke_armed: Option<NodeId>,
    /// Frames the user asked for while not connected; flushed after the next `Connected`.
    pub offline: Vec<UiToHub>,
    /// Blocker whose "Mark resolved" was clicked once (second click confirms).
    pub resolve_armed: Option<BlockerId>,
    /// "Update" clicked once for this node (`Some(None)` = Update all); second click confirms.
    pub update_armed: Option<Option<NodeId>>,
    /// Browser sign-in screen state (`web_login`); never visible in the native app.
    pub login: crate::web_login::Login,
    /// This app's own updater (native only; `None` until an identity is found).
    #[cfg(not(target_arch = "wasm32"))]
    pub self_update: Option<crate::selfupdate::SelfUpdate>,
    /// "Update app" clicked once; the second click installs and restarts the app.
    pub app_update_armed: bool,
}

impl App {
    /// Apply every queued net event; true when anything changed. After a (re)connect the
    /// selected agent is re-attached and the device list refreshed.
    pub fn pump(&mut self) -> bool {
        let Some(net) = &self.net else { return false };
        let events: Vec<Event> = net.events.try_iter().collect();
        let changed = !events.is_empty();
        let now = now_ms();
        for e in events {
            let connected = e == Event::Connected;
            apply(&mut self.ui, e);
            if connected {
                let mut frames = self.ui.resync_frames();
                frames.extend(self.ui.intent_to_frames(Intent::RefreshDevices));
                frames.append(&mut self.offline);
                net.send(frames);
            }
        }
        if changed {
            apply(&mut self.ui, Event::Tick(now));
            self.settle_spawn();
        }
        changed
    }

    /// Timer step from `poll(t)`: at most once per `TICK_SECS` apply `Event::Tick(wall clock)`.
    pub fn tick(&mut self, t: f64) -> bool {
        if t - self.last_tick < TICK_SECS {
            return false;
        }
        self.last_tick = t;
        let now = now_ms();
        apply(&mut self.ui, Event::Tick(now));
        true
    }

    /// Switch panel and fetch what it shows (mailbox history and devices are not pushed).
    pub fn open_panel(&mut self, p: Panel) {
        self.dispatch(Intent::SwitchPanel(p));
        match p {
            Panel::Mailbox => self.dispatch(Intent::RefreshMailbox(None)),
            Panel::Devices => self.dispatch(Intent::RefreshDevices),
            _ => {}
        }
    }

    /// The blocker that stops the current spawn form from starting (explicit node, or Auto
    /// when every idle node is blocked); the hub refuses these too.
    pub fn spawn_blocker(&self) -> Option<&Blocker> {
        let key = self.spawn.profile_key()?;
        match &self.spawn.node {
            Some(n) => self.ui.blockers_for(Some(n), &key).first().copied(),
            None => {
                let idle = |b: &Blocker| {
                    self.ui
                        .nodes
                        .iter()
                        .any(|n| &n.id == b.node() && n.state == NodeState::Idle)
                };
                let all_blocked = self
                    .ui
                    .nodes
                    .iter()
                    .filter(|n| n.state == NodeState::Idle)
                    .all(|n| !self.ui.blockers_for(Some(&n.id), &key).is_empty());
                let any = self
                    .ui
                    .blockers_for(None, &key)
                    .into_iter()
                    .find(|b| idle(b));
                if all_blocked {
                    any
                } else {
                    None
                }
            }
        }
    }

    /// First click arms, second click on the same blocker sends `ResolveBlocker`.
    pub fn resolve_click(&mut self, id: &BlockerId) {
        if self.resolve_armed.as_ref() == Some(id) {
            self.resolve_armed = None;
            self.dispatch(Intent::ResolveBlocker { id: id.clone() });
        } else {
            self.resolve_armed = Some(id.clone());
        }
    }

    /// Updating restarts the daemon: first click arms, second click on the same target sends.
    pub fn update_click(&mut self, node: Option<NodeId>) {
        if self.update_armed.as_ref() == Some(&node) {
            self.update_armed = None;
            self.dispatch(Intent::UpdateNode { node });
        } else {
            self.update_armed = Some(node);
        }
    }

    /// Send the spawn form. Invalid input sends nothing and sets `ui.last_error`.
    pub fn submit_spawn(&mut self) {
        self.ui.last_error = None;
        if let Some(b) = self.spawn_blocker() {
            self.ui.last_error = Some(format!(
                "{} on {}: resolve it first",
                crate::model::kind_title(b.kind()),
                b.node().as_str()
            ));
            return;
        }
        let form = self.spawn.form();
        self.dispatch(Intent::Spawn(form));
        if self.ui.last_error.is_none() {
            self.spawn.pending = AgentId::new(self.spawn.agent_id.text.trim()).ok();
        }
    }

    /// After hub events: clear the form once the spawned agent shows up; drop the wait on error.
    pub fn settle_spawn(&mut self) {
        let Some(id) = &self.spawn.pending else {
            return;
        };
        if self.ui.statuses.contains_key(id) || self.ui.node_of(id).is_some() {
            self.spawn.clear();
        } else if self.ui.last_error.is_some() {
            self.spawn.pending = None;
        }
    }

    /// First click arms the revoke, second click on the same device sends it.
    pub fn revoke_click(&mut self, id: &NodeId) {
        if self.revoke_armed.as_ref() == Some(id) {
            self.revoke_armed = None;
            self.dispatch(Intent::RevokeDevice(id.clone()));
        } else {
            self.revoke_armed = Some(id.clone());
        }
    }

    /// State effects now; frames go out when connected. While not connected, user intents
    /// (spawn, answer, kill, ...) are queued and sent after the reconnect, and a toast says so.
    /// Attach/Detach are not queued (the reconnect re-attaches) and keystrokes are refused
    /// (typing them into a pane minutes later would be wrong).
    pub fn dispatch(&mut self, intent: Intent) {
        let frames = self.ui.dispatch(intent);
        let Some(net) = &self.net else { return };
        if self.ui.conn == ConnStatus::Connected {
            net.send(frames);
            return;
        }
        for f in frames {
            match f {
                UiToHub::Attach { .. } | UiToHub::Detach { .. } => {}
                UiToHub::PaneInput { .. } => {
                    self.ui.last_error = Some("not connected: keystrokes were not sent".into());
                }
                f if self.offline.len() < OFFLINE_QUEUE_MAX => {
                    self.offline.push(f);
                    self.ui.last_error =
                        Some("not connected: queued, will be sent after reconnect".into());
                }
                _ => {
                    self.ui.last_error =
                        Some("not connected and the queue is full: action refused".into());
                }
            }
        }
    }
}

/// `AppSpec::setup`: start the network client unless this is a headless run.
#[cfg(not(target_arch = "wasm32"))]
pub fn setup(app: &mut App, wake: Waker) {
    if rune::tooling::headless_run().is_some() {
        return;
    }
    crate::selfupdate::confirm_started();
    let (dir, src) = match pick_identity(&config_dir(), &ulakd::config::default_dir()) {
        Ok(p) => p,
        Err(e) => {
            app.ui.last_error = Some(format!("not connected: {e}"));
            return;
        }
    };
    app.self_update = crate::selfupdate::SelfUpdate::start(&dir)
        .map_err(|e| eprintln!("ulak: self-update disabled: {e}"))
        .ok();
    // The fallback dir is ulakd's: never renew into it (ulakd renews its own).
    let renew_dir = (src == IdentitySource::Own).then(|| dir.clone());
    match Identity::load(&dir)
        .map_err(|e| e.to_string())
        .and_then(|id| start_in(&id, renew_dir, wake).map_err(|e| e.to_string()))
    {
        Ok(h) => {
            app.net = Some(h);
            app.identity = src.label().into();
        }
        Err(e) => {
            app.ui.last_error = Some(format!("not connected: {e} (run `ulak join <hub-url>`)"))
        }
    }
}

/// `AppSpec::poll`.
pub fn poll(app: &mut App, t: f64, _tx: &mut Tx) -> bool {
    let pumped = app.pump();
    let login = app.login.drain();
    #[cfg(not(target_arch = "wasm32"))]
    let upd = app.self_update.as_mut().is_some_and(|u| u.changed());
    #[cfg(target_arch = "wasm32")]
    let upd = false;
    app.tick(t) || pumped || login || upd
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tokio::sync::mpsc::unbounded_channel;
    use ulak_proto::{AgentId, HubToUi, UiToHub};

    #[test]
    fn pump_applies_events_and_resyncs_after_connect() {
        let (etx, erx) = mpsc::channel();
        let (otx, mut orx) = unbounded_channel();
        let mut app = App {
            net: Some(NetHandle::new(erx, otx)),
            ..App::default()
        };
        app.dispatch(Intent::SelectAgent(AgentId::new("a1").unwrap()));
        assert!(
            orx.try_recv().is_err(),
            "not connected yet: Attach is not sent"
        );
        etx.send(Event::Connected).unwrap();
        etx.send(Event::Hub(HubToUi::Error {
            message: "x".into(),
        }))
        .unwrap();
        assert!(app.pump());
        assert!(!app.pump());
        assert_eq!(app.ui.last_error.as_deref(), Some("x"));
        assert_eq!(
            orx.try_recv().unwrap(),
            UiToHub::Attach {
                agent: AgentId::new("a1").unwrap()
            }
        );
        assert_eq!(orx.try_recv().unwrap(), UiToHub::ListDevices);
    }

    #[test]
    fn intents_while_disconnected_are_queued_and_flushed_after_connect() {
        let (etx, erx) = mpsc::channel();
        let (otx, mut orx) = unbounded_channel();
        let mut app = App {
            net: Some(NetHandle::new(erx, otx)),
            ..App::default()
        };
        app.dispatch(Intent::Kill(AgentId::new("a1").unwrap()));
        app.dispatch(Intent::SendKeys(b"x".to_vec())); // nothing selected: no frame at all
        assert!(orx.try_recv().is_err());
        assert!(app.ui.last_error.as_deref().unwrap().contains("queued"));
        assert_eq!(app.offline.len(), 1);
        etx.send(Event::Connected).unwrap();
        app.pump();
        assert_eq!(orx.try_recv().unwrap(), UiToHub::ListDevices);
        assert_eq!(
            orx.try_recv().unwrap(),
            UiToHub::Kill {
                agent: AgentId::new("a1").unwrap()
            }
        );
        assert!(app.offline.is_empty());
        // Connected: straight through.
        app.dispatch(Intent::Kill(AgentId::new("a2").unwrap()));
        assert!(orx.try_recv().is_ok());
    }

    #[test]
    fn queue_is_bounded_and_keystrokes_are_refused_offline() {
        let (_etx, erx) = mpsc::channel();
        let (otx, mut orx) = unbounded_channel();
        let mut app = App {
            net: Some(NetHandle::new(erx, otx)),
            ..App::default()
        };
        app.ui.selected = Some(AgentId::new("a1").unwrap());
        app.dispatch(Intent::SendKeys(b"x".to_vec()));
        assert!(app.offline.is_empty());
        assert!(app.ui.last_error.as_deref().unwrap().contains("keystrokes"));
        for _ in 0..OFFLINE_QUEUE_MAX + 5 {
            app.dispatch(Intent::RefreshDevices);
        }
        assert_eq!(app.offline.len(), OFFLINE_QUEUE_MAX);
        assert!(app.ui.last_error.as_deref().unwrap().contains("full"));
        assert!(orx.try_recv().is_err());
    }
}
