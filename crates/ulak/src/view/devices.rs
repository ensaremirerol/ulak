//! Devices screen: add a device (shows the one-time code), list, revoke with a second click.
use super::nodebar::pill;
use crate::{
    app::{App, DEVICE_KINDS},
    model::{Intent, OtpOffer},
};
use rune::{col, row, text, El, ScrollSpec};
use rune_kit::{Button, ButtonVariant, Kit, SegmentedControl};
use ulak_proto::{DeviceInfo, DeviceKind};

/// Whole seconds left, never negative.
pub fn secs_left(o: &OtpOffer, now_ms: u64) -> u64 {
    o.expires_ms.saturating_sub(now_ms).div_ceil(1000)
}

pub fn countdown(secs: u64) -> String {
    if secs == 0 {
        "Expired".into()
    } else {
        format!("Expires in {}:{:02}", secs / 60, secs % 60)
    }
}

pub fn kind_name(k: DeviceKind) -> &'static str {
    DEVICE_KINDS
        .iter()
        .find(|(_, d)| *d == k)
        .map_or("?", |(n, _)| n)
}

fn otp_card(s: &App, k: &Kit, o: &OtpOffer) -> El {
    let left = secs_left(o, s.ui.now_ms);
    let dead = left == 0;
    let code = o.otp.expose();
    col()
        .key("otp-card")
        .gap(8.0)
        .padding(16.0)
        .w_full()
        .radius(8.0)
        .fill(k.c.layer_01)
        .border(1.0, if dead { k.c.border_base } else { k.c.text_accent })
        .child(
            text(format!(
                "One-time code for a new {} device",
                kind_name(o.kind)
            ))
            .font(k.ty.ui)
            .color(k.c.text_muted),
        )
        .child(
            text(format!("{} {}", &code[..4], &code[4..]))
                .key("otp-code")
                .font(rune::TextStyle {
                    size: 40.0,
                    line_height: 48.0,
                    ..k.ty.mono
                })
                .color(if dead {
                    k.c.text_faint
                } else {
                    k.c.text_strong
                }),
        )
        .child(
            text(countdown(left))
                .key("otp-countdown")
                .font(k.ty.ui_medium)
                .color(if dead { k.c.danger.fg } else { k.c.success.fg }),
        )
        .child(
            text(format!("Hub fingerprint: {}", o.hub_fingerprint))
                .key("otp-fingerprint")
                .wrap()
                .font(k.ty.mono)
                .color(k.c.text_base),
        )
        .child(
            text(if dead {
                "This code no longer works. Press Add device for a new one."
            } else {
                "On the new machine run `ulakd join <hub-url>` (or `ulak join`), check the fingerprint matches, then enter the code."
            })
            .wrap()
            .font(k.ty.small)
            .color(k.c.text_muted),
        )
}

fn device_row(s: &App, k: &Kit, d: &DeviceInfo) -> El {
    let id = d.id.clone();
    let armed = s.revoke_armed.as_ref() == Some(&id);
    let mut r = row()
        .key(format!("device-{}", id.as_str()))
        .gap(12.0)
        .center_y()
        .px(10.0)
        .py(6.0)
        .w_full()
        .radius(6.0)
        .fill(k.c.layer_01)
        .child(
            text(id.as_str().to_owned())
                .w(160.0)
                .font(k.ty.ui_medium)
                .color(k.c.text_base),
        )
        .child(
            text(kind_name(d.kind))
                .w(70.0)
                .font(k.ty.ui)
                .color(k.c.text_muted),
        )
        .child(
            text(d.hostname.clone())
                .grow(1.0)
                .font(k.ty.ui)
                .color(k.c.text_base),
        )
        .child(
            text(format!("created {}", super::mailbox::clock(d.created_ms)))
                .font(k.ty.small)
                .color(k.c.text_muted),
        );
    if d.revoked {
        r = r.child(pill(k, "revoked".into(), &k.c.danger).key(format!("revoked-{}", id.as_str())));
    } else {
        let b = id.clone();
        r = r.child(
            Button::new(if armed {
                "Click again to revoke"
            } else {
                "Revoke"
            })
            .variant(ButtonVariant::Danger)
            .on_press(move |s: &mut App| s.revoke_click(&b))
            .build(k)
            .key(format!("revoke-{}", id.as_str())),
        );
        if armed {
            r = r.child(
                Button::new("Cancel")
                    .variant(ButtonVariant::Ghost)
                    .on_press(|s: &mut App| s.revoke_armed = None)
                    .build(k)
                    .key(format!("revoke-cancel-{}", id.as_str())),
            );
        }
    }
    r
}

pub fn view(s: &App, k: &Kit) -> El {
    let kinds = SegmentedControl::new(
        &DEVICE_KINDS.map(|d| d.0),
        Some(s.device_kind.min(DEVICE_KINDS.len() - 1)),
    )
    .fit()
    .label("Device kind")
    .on_pick(|s: &mut App, i, _| s.device_kind = i)
    .build(k)
    .key("device-kind");
    let mut c = col()
        .gap(12.0)
        .padding(16.0)
        .w_full()
        .child(
            text("Devices")
                .font(k.ty.body_medium)
                .color(k.c.text_strong),
        )
        .child(
            row()
                .gap(8.0)
                .center_y()
                .child(kinds)
                .child(
                    Button::new("Add device")
                        .variant(ButtonVariant::Submit)
                        .on_press(|s: &mut App| {
                            let kind = DEVICE_KINDS[s.device_kind.min(DEVICE_KINDS.len() - 1)].1;
                            s.dispatch(Intent::AddDevice(kind))
                        })
                        .build(k)
                        .key("add-device"),
                )
                .child(
                    Button::new("Refresh")
                        .variant(ButtonVariant::Outline)
                        .on_press(|s: &mut App| s.dispatch(Intent::RefreshDevices))
                        .build(k)
                        .key("devices-refresh"),
                ),
        );
    if let Some(o) = &s.ui.otp {
        c = c.child(otp_card(s, k, o));
    }
    if s.ui.devices.is_empty() {
        c = c.child(
            text("No devices enrolled yet. Choose a kind and press Add device to get a one-time code, then enroll the machine with it.")
                .key("devices-empty")
                .wrap()
                .font(k.ty.ui)
                .color(k.c.text_faint),
        );
    }
    for d in &s.ui.devices {
        c = c.child(device_row(s, k, d));
    }
    c = c.child(super::nodes::view(s, k));
    c.scroll(ScrollSpec {
        stick_bottom: false,
    })
    .grow(1.0)
}
