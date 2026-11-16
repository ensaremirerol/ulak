//! Keyboard: Rune `KeyEvent` -> terminal bytes, and the app-level `on_key` hook (panel shortcuts,
//! then forward to the focused pane).
use crate::{app::App, model::Intent, model::Panel};
use rune::{Key, KeyEvent, Tx};

/// Bytes a terminal expects for `ev`; None when the key has no terminal meaning (modifier keys,
/// function keys, app chords).
pub fn to_bytes(ev: &KeyEvent) -> Option<Vec<u8>> {
    let m = ev.mods;
    let ctrl = m.ctrl || m.cmd;
    let csi = |s: &str| Some([b"\x1b".as_slice(), s.as_bytes()].concat());
    match &ev.key {
        Key::Text(t) if !ctrl => {
            let mut out = if m.alt { vec![0x1b] } else { vec![] };
            out.extend(t.as_bytes());
            Some(out)
        }
        Key::Command(c) if ctrl && c.is_ascii() => match *c {
            'a'..='z' => Some(vec![*c as u8 - b'a' + 1]),
            '[' => Some(vec![0x1b]),
            '\\' => Some(vec![0x1c]),
            ']' => Some(vec![0x1d]),
            ' ' | '@' => Some(vec![0]),
            _ => None,
        },
        Key::Enter => Some(b"\r".to_vec()),
        Key::Backspace => Some(vec![0x7f]),
        Key::Tab if m.shift => csi("[Z"),
        Key::Tab => Some(b"\t".to_vec()),
        Key::Escape => Some(vec![0x1b]),
        Key::Up => csi("[A"),
        Key::Down => csi("[B"),
        Key::Right => csi("[C"),
        Key::Left => csi("[D"),
        Key::Home => csi("[H"),
        Key::End => csi("[F"),
        Key::Delete => csi("[3~"),
        Key::PageUp => csi("[5~"),
        Key::PageDown => csi("[6~"),
        _ => None,
    }
}

/// Ctrl+1..5 switch panels (Mux, Questions, Spawn, Mailbox, Devices). Rune reports Ctrl+digit as
/// `Key::Command('1')` on every platform; Alt+digit is layout dependent, so it is not used.
pub fn shortcut_panel(ev: &KeyEvent) -> Option<Panel> {
    let Key::Command(c) = ev.key else { return None };
    if !(ev.mods.ctrl || ev.mods.cmd) {
        return None;
    }
    let i = c.to_digit(10)?.checked_sub(1)? as usize;
    super::shell::TABS.get(i).map(|t| t.0)
}

/// `AppSpec::on_key`: runs for keys no focused control took.
pub fn on_key(app: &mut App, ev: &KeyEvent, _: &mut Tx) -> bool {
    if let Some(p) = shortcut_panel(ev) {
        app.open_panel(p);
        return true;
    }
    if app.ui.panel != Panel::Terminal || app.ui.selected.is_none() {
        return false;
    }
    match to_bytes(ev) {
        Some(b) => {
            app.dispatch(Intent::SendKeys(b));
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::Mods;

    fn ev(key: Key, mods: Mods) -> KeyEvent {
        KeyEvent { key, mods }
    }
    fn plain(key: Key) -> KeyEvent {
        ev(key, Mods::default())
    }
    fn ctrl() -> Mods {
        Mods {
            ctrl: true,
            cmd: true,
            ..Mods::default()
        }
    }

    #[test]
    fn text_and_specials() {
        assert_eq!(to_bytes(&plain(Key::Text("é".into()))), Some("é".into()));
        assert_eq!(to_bytes(&plain(Key::Enter)), Some(b"\r".to_vec()));
        assert_eq!(to_bytes(&plain(Key::Backspace)), Some(vec![0x7f]));
        assert_eq!(to_bytes(&plain(Key::Tab)), Some(b"\t".to_vec()));
        assert_eq!(to_bytes(&plain(Key::Escape)), Some(vec![0x1b]));
        assert_eq!(to_bytes(&plain(Key::Up)), Some(b"\x1b[A".to_vec()));
        assert_eq!(to_bytes(&plain(Key::Left)), Some(b"\x1b[D".to_vec()));
        assert_eq!(to_bytes(&plain(Key::Delete)), Some(b"\x1b[3~".to_vec()));
        assert_eq!(to_bytes(&plain(Key::Function(1))), None);
    }

    #[test]
    fn ctrl_letters_and_alt() {
        assert_eq!(to_bytes(&ev(Key::Command('c'), ctrl())), Some(vec![3]));
        assert_eq!(to_bytes(&ev(Key::Command('d'), ctrl())), Some(vec![4]));
        assert_eq!(to_bytes(&ev(Key::Command('1'), ctrl())), None);
        let alt = Mods {
            alt: true,
            ..Mods::default()
        };
        assert_eq!(
            to_bytes(&ev(Key::Text("b".into()), alt)),
            Some(b"\x1bb".to_vec())
        );
    }

    #[test]
    fn digits_with_ctrl_pick_panels() {
        assert_eq!(
            shortcut_panel(&ev(Key::Command('2'), ctrl())),
            Some(Panel::Questions)
        );
        assert_eq!(
            shortcut_panel(&ev(Key::Command('5'), ctrl())),
            Some(Panel::Devices)
        );
        assert_eq!(shortcut_panel(&ev(Key::Command('6'), ctrl())), None);
        assert_eq!(shortcut_panel(&ev(Key::Command('0'), ctrl())), None);
        assert_eq!(shortcut_panel(&plain(Key::Text("2".into()))), None);
    }
}
