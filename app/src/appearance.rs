//! appearance.rs — the display-config "landing pad".
//!
//! Ported from terminal-delight's per-pane appearance model and generalized: a
//! pane's look is split into FOUR independently-inheriting groups, each
//! delineated by an inheritance + override ("follow outer") structure —
//!
//!   COLOUR  — the theme id + an optional seed hue (what colour the screen is)
//!   TEXTURE — the CRT effect set (scanlines, bloom, tracking, flicker, glare…),
//!             decoupled from colour so you can wear hacker's tube on paper's ink
//!   GRADE   — the monitor-OSD: brightness · contrast · saturation · gamma ·
//!             text-size, applied as HSLA transforms at paint time
//!   CURVE   — the toggleable barrel warp (screen curvature), on its own switch
//!
//! Each group resolves workspace-default → per-pane with a NON-DESTRUCTIVE
//! "follow outer" toggle: a pane keeps its retained override even while
//! inheriting, so toggling back restores exactly what it had. `compose()` folds
//! a resolved appearance into a single `theme::Theme` the renderer already
//! understands (grade baked into the colours, texture dials swapped in, curve
//! gated), plus a text-size multiplier.

use gpui::{App, Global, Hsla};
use serde::{Deserialize, Serialize};

use crate::theme::{self, Theme};

/// The workspace's current outer look, as a global so any pane can resolve its
/// own appearance against it without holding a Workspace handle. Kept in sync by
/// `Workspace::rebuild_outer`.
pub struct ActiveOuter(pub OuterAppearance);
impl Global for ActiveOuter {}

/// Read the active outer appearance (falls back to a hacker default).
pub fn outer(cx: &App) -> OuterAppearance {
    cx.try_global::<ActiveOuter>()
        .map(|o| o.0.clone())
        .unwrap_or_default()
}

// ── GRADE — the monitor-OSD group ───────────────────────────────────────────

/// Paint-time HSLA grade. The first four channels are 0..1 with **0.5 neutral**;
/// `text_scale` is a 0.6..2.0 multiplier with **1.0 neutral**.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Grade {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub gamma: f32,
    pub text_scale: f32,
}

impl Default for Grade {
    fn default() -> Self {
        Self {
            brightness: 0.5,
            contrast: 0.5,
            saturation: 0.5,
            gamma: 0.5,
            text_scale: 1.0,
        }
    }
}

/// The five tunable channels of a [`Grade`], for the monitor-OSD tray.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GradeKey {
    Brightness,
    Contrast,
    Saturation,
    Gamma,
    TextScale,
}

impl GradeKey {
    pub const ALL: [GradeKey; 5] = [
        GradeKey::Brightness,
        GradeKey::Contrast,
        GradeKey::Saturation,
        GradeKey::Gamma,
        GradeKey::TextScale,
    ];

    pub fn label(self) -> &'static str {
        match self {
            GradeKey::Brightness => "brightness",
            GradeKey::Contrast => "contrast",
            GradeKey::Saturation => "saturation",
            GradeKey::Gamma => "gamma",
            GradeKey::TextScale => "text size",
        }
    }

    /// (min, max, neutral) for the channel — drives slider mapping + reset.
    pub fn range(self) -> (f32, f32, f32) {
        match self {
            GradeKey::TextScale => (theme::FONT_SCALE_MIN, theme::FONT_SCALE_MAX, 1.0),
            _ => (0.0, 1.0, 0.5),
        }
    }
}

impl Grade {
    pub fn is_neutral(&self) -> bool {
        let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
        near(self.brightness, 0.5)
            && near(self.contrast, 0.5)
            && near(self.saturation, 0.5)
            && near(self.gamma, 0.5)
            && near(self.text_scale, 1.0)
    }

    pub fn get(&self, key: GradeKey) -> f32 {
        match key {
            GradeKey::Brightness => self.brightness,
            GradeKey::Contrast => self.contrast,
            GradeKey::Saturation => self.saturation,
            GradeKey::Gamma => self.gamma,
            GradeKey::TextScale => self.text_scale,
        }
    }

    pub fn set(&mut self, key: GradeKey, v: f32) {
        let (min, max, _) = key.range();
        let v = v.clamp(min, max);
        match key {
            GradeKey::Brightness => self.brightness = v,
            GradeKey::Contrast => self.contrast = v,
            GradeKey::Saturation => self.saturation = v,
            GradeKey::Gamma => self.gamma = v,
            GradeKey::TextScale => self.text_scale = v,
        }
    }

    /// Grade one colour: gamma → contrast → brightness on lightness, saturation
    /// multiplier on saturation. Neutral grade is the identity (fast path).
    pub fn apply(&self, c: Hsla) -> Hsla {
        if self.is_neutral() {
            return c;
        }
        let s = (c.s * self.saturation / 0.5).clamp(0.0, 1.0);
        let mut l = c.l.clamp(0.0, 1.0);
        // gamma: <0.5 lifts shadows, >0.5 deepens them
        let gamma = 2.0_f32.powf((0.5 - self.gamma) * 2.0);
        l = l.powf(gamma);
        // contrast: push away from / toward mid-grey
        l = (l - 0.5) * (self.contrast / 0.5) + 0.5;
        // master brightness
        l *= self.brightness / 0.5;
        Hsla {
            h: c.h,
            s,
            l: l.clamp(0.0, 1.0),
            a: c.a,
        }
    }
}

// ── COLOUR / TEXTURE / CURVE groups ─────────────────────────────────────────

/// COLOUR group — the theme id and an optional seed hue (hex `#rrggbb`).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Colour {
    pub id: String,
    #[serde(default)]
    pub seed: Option<String>,
}

impl Colour {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            seed: None,
        }
    }
    pub fn seed_hsla(&self) -> Option<Hsla> {
        self.seed.as_deref().and_then(theme::parse_hex)
    }
}

/// `#rrggbb` for an Hsla — so a seed picked in the tray persists as text.
pub fn hsla_to_hex(c: Hsla) -> String {
    let rgba = gpui::Rgba::from(c);
    let to = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", to(rgba.r), to(rgba.g), to(rgba.b))
}

/// TEXTURE group — which theme's CRT effect dials to wear. `None` = follow the
/// colour theme's own effects; `Some(id)` = borrow that theme's tube.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Texture {
    #[serde(default)]
    pub id: Option<String>,
}

/// CURVE group — the toggleable screen curvature. `amount: None` = use the
/// texture theme's own curvature; `on: false` = a flat screen.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Curve {
    pub on: bool,
    #[serde(default)]
    pub amount: Option<f32>,
}

impl Default for Curve {
    fn default() -> Self {
        Self {
            on: true,
            amount: None,
        }
    }
}

// ── workspace defaults + per-pane overrides ─────────────────────────────────

/// The workspace ("outer") look — every group has a concrete value.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct OuterAppearance {
    pub colour: Colour,
    #[serde(default)]
    pub texture: Texture,
    #[serde(default)]
    pub grade: Grade,
    #[serde(default)]
    pub curve: Curve,
}

impl OuterAppearance {
    pub fn new(theme_id: impl Into<String>) -> Self {
        Self {
            colour: Colour::new(theme_id),
            texture: Texture::default(),
            grade: Grade::default(),
            curve: Curve::default(),
        }
    }
}

impl Default for OuterAppearance {
    fn default() -> Self {
        Self::new("hacker")
    }
}

/// Per-pane overrides + per-group inherit flags. A group with `inherit = true`
/// follows the outer value live; with `inherit = false` it uses its retained
/// override. Overrides are kept even while inheriting (non-destructive toggle).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct PaneAppearance {
    #[serde(default)]
    pub colour: Option<Colour>,
    #[serde(default = "yes")]
    pub inherit_colour: bool,
    #[serde(default)]
    pub texture: Option<Texture>,
    #[serde(default = "yes")]
    pub inherit_texture: bool,
    #[serde(default)]
    pub grade: Option<Grade>,
    #[serde(default = "yes")]
    pub inherit_grade: bool,
    #[serde(default)]
    pub curve: Option<Curve>,
    #[serde(default = "yes")]
    pub inherit_curve: bool,
}

fn yes() -> bool {
    true
}

impl Default for PaneAppearance {
    /// A pristine pane: every group inherits the outer look, no overrides yet.
    fn default() -> Self {
        Self {
            colour: None,
            inherit_colour: true,
            texture: None,
            inherit_texture: true,
            grade: None,
            inherit_grade: true,
            curve: None,
            inherit_curve: true,
        }
    }
}

/// A fully-resolved appearance for one pane — every group concrete.
#[derive(Clone, PartialEq, Debug)]
pub struct Resolved {
    pub colour: Colour,
    pub texture: Texture,
    pub grade: Grade,
    pub curve: Curve,
}

impl PaneAppearance {
    /// Resolve each group against the outer defaults, independently.
    pub fn effective(&self, outer: &OuterAppearance) -> Resolved {
        Resolved {
            colour: match (self.inherit_colour, &self.colour) {
                (false, Some(c)) => c.clone(),
                _ => outer.colour.clone(),
            },
            texture: match (self.inherit_texture, &self.texture) {
                (false, Some(t)) => t.clone(),
                _ => outer.texture.clone(),
            },
            grade: match (self.inherit_grade, self.grade) {
                (false, Some(g)) => g,
                _ => outer.grade,
            },
            curve: match (self.inherit_curve, self.curve) {
                (false, Some(c)) => c,
                _ => outer.curve,
            },
        }
    }

    /// True when nothing has ever diverged — safe to omit from a state file.
    pub fn is_pristine(&self) -> bool {
        self.inherit_colour
            && self.inherit_texture
            && self.inherit_grade
            && self.inherit_curve
            && self.colour.is_none()
            && self.texture.is_none()
            && self.grade.is_none()
            && self.curve.is_none()
    }

    // ── setters: pin a group (retain override + stop inheriting) ──

    pub fn set_colour(&mut self, c: Colour) {
        self.colour = Some(c);
        self.inherit_colour = false;
    }
    pub fn set_texture(&mut self, t: Texture) {
        self.texture = Some(t);
        self.inherit_texture = false;
    }
    pub fn set_grade(&mut self, g: Grade) {
        self.grade = Some(g);
        self.inherit_grade = false;
    }
    pub fn set_curve(&mut self, c: Curve) {
        self.curve = Some(c);
        self.inherit_curve = false;
    }

    /// Reset every group to follow the outer look (override retained, hidden).
    pub fn follow_outer(&mut self) {
        self.inherit_colour = true;
        self.inherit_texture = true;
        self.inherit_grade = true;
        self.inherit_curve = true;
    }
}

// ── compose: resolved appearance → one rendered Theme ───────────────────────

/// Fold a resolved appearance into a single [`Theme`]: colour theme + seed,
/// texture theme's effect dials, curve gating, and the grade baked into every
/// colour. The renderer keeps using `Theme` fields unchanged. `text_scale` is
/// returned separately (it multiplies the font, not a colour).
pub fn compose(cx: &App, r: &Resolved) -> Theme {
    // COLOUR — base palette, optionally folded onto the seed hue.
    let colour_base = theme::resolve(cx, Some(&r.colour.id));
    let mut t = match r.colour.seed_hsla() {
        Some(seed) => theme::recolor(&colour_base, seed),
        None => (*colour_base).clone(),
    };

    // TEXTURE — borrow another theme's CRT effect dials (else keep our own).
    let tex = match &r.texture.id {
        Some(id) => theme::resolve(cx, Some(id)),
        None => colour_base.clone(),
    };
    t.scanline_opacity = tex.scanline_opacity;
    t.scanline_step = tex.scanline_step;
    t.vignette = tex.vignette;
    t.glow = tex.glow;
    t.bloom = tex.bloom;
    t.tracking = tex.tracking;
    t.tracking_period = tex.tracking_period;
    t.tracking_sweep = tex.tracking_sweep;
    t.flicker = tex.flicker;
    t.jiggle = tex.jiggle;
    t.screen_glare = tex.screen_glare;

    // CURVE — its own toggle; default amount is the texture theme's curvature.
    t.curvature = if r.curve.on {
        r.curve.amount.unwrap_or(tex.curvature).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // GRADE — bake the monitor-OSD transforms into every colour.
    let g = &r.grade;
    if !g.is_neutral() {
        t.bg = g.apply(t.bg);
        t.surface = g.apply(t.surface);
        t.text = g.apply(t.text);
        t.accent = g.apply(t.accent);
        t.faint = g.apply(t.faint);
        t.frame_bg = g.apply(t.frame_bg);
        t.frame_text = g.apply(t.frame_text);
        t.frame_border = g.apply(t.frame_border);
        t.frame_faint = g.apply(t.frame_faint);
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_grade_is_identity() {
        let g = Grade::default();
        let c = Hsla {
            h: 0.3,
            s: 0.6,
            l: 0.5,
            a: 1.0,
        };
        assert_eq!(g.apply(c), c);
        assert!(g.is_neutral());
    }

    #[test]
    fn brightness_darkens_and_brightens() {
        let c = Hsla {
            h: 0.3,
            s: 0.6,
            l: 0.5,
            a: 1.0,
        };
        let dark = Grade {
            brightness: 0.25,
            ..Default::default()
        }
        .apply(c);
        let bright = Grade {
            brightness: 0.75,
            ..Default::default()
        }
        .apply(c);
        assert!(
            dark.l < c.l,
            "0.25 brightness darkens: {} !< {}",
            dark.l,
            c.l
        );
        assert!(
            bright.l > c.l,
            "0.75 brightness brightens: {} !> {}",
            bright.l,
            c.l
        );
    }

    #[test]
    fn saturation_channel_scales_s() {
        let c = Hsla {
            h: 0.3,
            s: 0.5,
            l: 0.5,
            a: 1.0,
        };
        let desat = Grade {
            saturation: 0.0,
            ..Default::default()
        }
        .apply(c);
        assert!(desat.s < 1e-6, "saturation 0 → greyscale, got {}", desat.s);
    }

    #[test]
    fn grade_key_roundtrip() {
        let mut g = Grade::default();
        for k in GradeKey::ALL {
            let (min, max, _) = k.range();
            g.set(k, max + 10.0); // over-range clamps
            assert!((g.get(k) - max).abs() < 1e-6, "{:?} clamps to max", k);
            g.set(k, min - 10.0);
            assert!((g.get(k) - min).abs() < 1e-6, "{:?} clamps to min", k);
        }
    }

    fn outer() -> OuterAppearance {
        OuterAppearance::new("hacker")
    }

    #[test]
    fn pristine_pane_follows_outer_in_every_group() {
        let p = PaneAppearance::default();
        assert!(p.is_pristine());
        let eff = p.effective(&outer());
        assert_eq!(eff.colour.id, "hacker");
        assert!(eff.texture.id.is_none());
        assert!(eff.grade.is_neutral());
        assert!(eff.curve.on);
    }

    #[test]
    fn groups_inherit_independently() {
        let mut p = PaneAppearance::default();
        // pin ONLY the grade; colour/texture/curve still follow outer
        p.set_grade(Grade {
            brightness: 0.2,
            ..Default::default()
        });
        let mut o = outer();
        o.colour = Colour::new("paper"); // change outer colour
        let eff = p.effective(&o);
        assert_eq!(eff.colour.id, "paper", "colour still follows outer");
        assert!((eff.grade.brightness - 0.2).abs() < 1e-6, "grade is pinned");
        assert!(!p.is_pristine());
    }

    #[test]
    fn follow_outer_toggle_is_non_destructive() {
        let mut p = PaneAppearance::default();
        p.set_texture(Texture {
            id: Some("amber".into()),
        });
        assert!(!p.inherit_texture);
        // go back to inheriting — override stays retained, just hidden
        p.follow_outer();
        let eff = p.effective(&outer());
        assert!(eff.texture.id.is_none(), "now follows outer (no texture)");
        assert_eq!(
            p.texture,
            Some(Texture {
                id: Some("amber".into())
            }),
            "override retained"
        );
        // re-pin: the same override returns
        p.inherit_texture = false;
        assert_eq!(p.effective(&outer()).texture.id.as_deref(), Some("amber"));
    }

    #[test]
    fn pane_appearance_serde_roundtrip() {
        let mut p = PaneAppearance::default();
        p.set_colour(Colour {
            id: "ice".into(),
            seed: Some("#2f6fdd".into()),
        });
        p.set_curve(Curve {
            on: false,
            amount: None,
        });
        let json = serde_json::to_string(&p).unwrap();
        let back: PaneAppearance = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
