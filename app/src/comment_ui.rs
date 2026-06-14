//! comment_ui.rs — the "device" visual vocabulary for comment mode.
//!
//! DELIBERATELY not the green CRT tube: comment mode opens a solid, slate
//! instrument panel (think the INTELLIMASS field-command assistant) so the
//! reviewing surface reads as a separate physical device floating over the
//! glass. Two **sunken screens** (a magnifier for the quoted text + the comment
//! thread) and **chunky bevelled buttons** for the composer. The theme's accent
//! drives the edge ring/glow so it still belongs to the active palette, but the
//! body stays neutral slate. These are pure style builders — interaction
//! (listeners, state) lives in `pane.rs`.

use gpui::{
    BoxShadow, Div, FontWeight, SharedString, Styled, div, hsla, point, prelude::*, px, rgb, white,
};

use crate::theme;

/// solid slate device body — distinct from any CRT theme background
const DEVICE_BG: u32 = 0x0d1219;
/// recessed screen face (darker than the body so the inset shadow reads)
const SCREEN_BG: u32 = 0x06090d;
/// neutral button cap
const BTN_BG: u32 = 0x1b2531;
/// light neutral label/body text on the device
const INK: u32 = 0xc4d0dd;
const INK_DIM: u32 = 0x7f8da0;

/// The floating device shell: dark fill, accent ring + outer glow + drop shadow.
pub fn device_panel(th: &theme::Theme) -> Div {
    let ring = BoxShadow {
        color: th.accent.alpha(0.55),
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(1.),
        inset: false,
    };
    let glow = BoxShadow {
        color: th.accent.alpha(0.22),
        offset: point(px(0.), px(0.)),
        blur_radius: px(30.),
        spread_radius: px(2.),
        inset: false,
    };
    let drop = BoxShadow {
        color: hsla(0., 0., 0., 0.62),
        offset: point(px(0.), px(20.)),
        blur_radius: px(48.),
        spread_radius: px(0.),
        inset: false,
    };
    div()
        .bg(rgb(DEVICE_BG))
        .rounded_lg()
        .border_1()
        .border_color(th.accent.alpha(0.4))
        .shadow(vec![ring, glow, drop])
        .text_color(rgb(INK))
        .flex()
        .flex_col()
}

/// A recessed "screen": dark face with an inset top shadow + faint accent rim.
pub fn sunken_screen(th: &theme::Theme) -> Div {
    let inset_top = BoxShadow {
        color: hsla(0., 0., 0., 0.7),
        offset: point(px(0.), px(2.)),
        blur_radius: px(7.),
        spread_radius: px(0.),
        inset: true,
    };
    let rim = BoxShadow {
        color: th.accent.alpha(0.12),
        offset: point(px(0.), px(0.)),
        blur_radius: px(0.),
        spread_radius: px(1.),
        inset: true,
    };
    div()
        .bg(rgb(SCREEN_BG))
        .rounded_md()
        .shadow(vec![inset_top, rim])
        .text_color(rgb(INK))
}

/// A small spaced-uppercase instrument label (like equipment-panel kickers).
pub fn kicker(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(9.5))
        .text_color(rgb(INK_DIM))
        .child(text.into())
}

/// A chunky physical button cap. `primary` tints it with the theme accent.
/// Returns a plain `Div`; the caller attaches the click listener.
pub fn device_button(th: &theme::Theme, label: &str, primary: bool) -> Div {
    let glint = BoxShadow {
        color: white().alpha(0.16),
        offset: point(px(1.), px(1.)),
        blur_radius: px(0.),
        spread_radius: px(0.),
        inset: true,
    };
    let seat = BoxShadow {
        color: hsla(0., 0., 0., 0.55),
        offset: point(px(2.), px(3.)),
        blur_radius: px(4.),
        spread_radius: px(0.),
        inset: false,
    };
    let b = div()
        .px_4()
        .py(px(7.))
        .rounded_md()
        .border_1()
        .text_size(px(12.))
        .font_weight(FontWeight::BOLD)
        .cursor_pointer()
        .shadow(vec![glint, seat]);
    if primary {
        b.bg(th.accent.alpha(0.9))
            .border_color(th.accent)
            .text_color(th.bg)
            .hover(|s| s.bg(th.accent))
            .child(label.to_string())
    } else {
        b.bg(rgb(BTN_BG))
            .border_color(hsla(0., 0., 0., 0.5))
            .text_color(rgb(INK))
            .hover(|s| s.bg(rgb(0x232f3d)))
            .child(label.to_string())
    }
}

/// Coarse relative time for a comment timestamp (millis since epoch).
pub fn ago(ts: i64) -> String {
    let now = crate::comments::now_millis();
    let s = (now - ts).max(0) / 1000;
    if s < 60 {
        "just now".into()
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 86_400 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86_400)
    }
}
