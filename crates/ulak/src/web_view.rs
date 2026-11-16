//! The sign-in screen, shown instead of the shell while `App.login.visible`.
use crate::{
    app::App,
    web_login::{start_login, start_register},
};
use rune::{col, text, El};
use rune_kit::{Button, ButtonVariant, Kit, TextInput};

pub fn view(s: &App, k: &Kit) -> El {
    let l = &s.login;
    let mut card = col()
        .key("login")
        .w(380.0)
        .gap(12.0)
        .padding(24.0)
        .radius(12.0)
        .fill(k.c.layer_01)
        .child(text("ulak").font(k.ty.ui_medium).color(k.c.text_strong))
        .child(
            text("Sign in with a passkey, or register this device with a one-time code from the hub (Devices > Add device > browser).")
                .wrap()
                .font(k.ty.small)
                .color(k.c.text_muted),
        )
        .child(
            Button::new(if l.busy { "Waiting for passkey..." } else { "Sign in with passkey" })
                .variant(ButtonVariant::Submit)
                .on_press(|s: &mut App| {
                    if !s.login.busy {
                        start_login();
                    }
                })
                .build(k)
                .key("login-signin"),
        )
        .child(text("New device").font(k.ty.small_medium).color(k.c.text_muted))
        .child(
            TextInput::new(&l.otp)
                .placeholder("One-time code (XXXX XXXX)")
                .width(None)
                .on_edit(|s: &mut App| &mut s.login.otp)
                .on_submit(|s: &mut App, _| register(s))
                .build(k)
                .key("login-otp"),
        )
        .child(
            TextInput::new(&l.name)
                .placeholder("Device name (optional)")
                .width(None)
                .on_edit(|s: &mut App| &mut s.login.name)
                .build(k)
                .key("login-name"),
        )
        .child(
            Button::new("Register this device")
                .variant(ButtonVariant::Outline)
                .on_press(register)
                .build(k)
                .key("login-register"),
        );
    if let Some(m) = &l.message {
        card = card.child(
            text(m.clone())
                .key("login-error")
                .wrap()
                .font(k.ty.small)
                .color(k.c.danger.fg),
        );
    }
    col()
        .w_full()
        .h_full()
        .fill(k.c.bg_base)
        .center()
        .child(card)
}

fn register(s: &mut App) {
    if !s.login.busy {
        start_register(&s.login.otp.text, &s.login.name.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Runtime};

    #[test]
    fn register_with_empty_otp_shows_error() {
        let mut app = App::default();
        app.login.visible = true;
        let mut rt = Runtime::new(
            app,
            crate::view::view,
            (1000.0, 650.0),
            Env {
                system_dark: true,
                scale: 1.0,
            },
        );
        rt.frame(0.0);
        assert!(rt.find("login-error").is_none());
        let (x, y) = rt.find("login-register").expect("register button");
        rt.pointer_move(x, y);
        rt.frame(0.0);
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        assert!(rt.state.login.drain(), "a message was posted");
        rt.invalidate();
        rt.frame(0.0);
        assert!(rt.state.login.message.is_some());
        let shown = rt.elements();
        assert!(
            shown.iter().any(|e| e.key.as_deref() == Some("login-error")
                && e.text.as_deref() == Some("enter the one-time code first")),
            "{:?}",
            shown.iter().map(|e| (&e.key, &e.text)).collect::<Vec<_>>()
        );
    }
}
