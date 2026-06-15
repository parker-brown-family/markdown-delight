//! Hot-reloaded theme — ported from terminal-delight (MIT sibling), extended
//! with the [shell] section (the complement-coloured monitor housing).
//! Source of truth: ~/.config/markdown-delight/theme.toml (seeded with the
//! hacker theme on first run) → embedded default. A background task polls
//! mtime (~300ms) and swaps the global on change. Day-one pillar: modify on
//! the fly, no recompile.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
    sync::Arc,
    time::{Duration, SystemTime},
};

use gpui::{rgb, App, Global, Hsla};
use serde::Deserialize;

pub const DEFAULT_THEME_TOML: &str = include_str!("../themes/hacker.toml");

/// Bundled themes, in cycle order. The first is the default. Per-pane theme
/// overrides pick from here (plus any user themes discovered at runtime).
pub const BUNDLED_THEMES: &[&str] = &[
    include_str!("../themes/hacker.toml"),
    include_str!("../themes/amber.toml"),
    include_str!("../themes/ice.toml"),
    include_str!("../themes/paper.toml"),
];

/// Live text-zoom multiplier driven by the bezel scrubber. Process-global (not
/// in the hot-reloaded Theme) so the slider can nudge it every drag-frame
/// without rebuilding the theme. 0 is the "untouched = 1.0" sentinel.
pub const FONT_SCALE_MIN: f32 = 0.6;
pub const FONT_SCALE_MAX: f32 = 2.0;
static FONT_SCALE: AtomicU32 = AtomicU32::new(0);

pub fn font_scale() -> f32 {
    match FONT_SCALE.load(Ordering::Relaxed) {
        0 => 1.0,
        bits => f32::from_bits(bits),
    }
}

pub fn set_font_scale(v: f32) {
    FONT_SCALE.store(
        v.clamp(FONT_SCALE_MIN, FONT_SCALE_MAX).to_bits(),
        Ordering::Relaxed,
    );
}

#[derive(Deserialize)]
struct FileColors {
    bg: String,
    surface: String,
    text: String,
    accent: String,
    faint: String,
}

#[derive(Deserialize, Default)]
struct FileShell {
    frame_bg: Option<String>,
    frame_text: Option<String>,
    frame_border: Option<String>,
    frame_faint: Option<String>,
}

#[derive(Deserialize, Default)]
struct FileEffects {
    scanline_opacity: Option<f32>,
    scanline_step: Option<f32>,
    vignette: Option<f32>,
    glow: Option<f32>,
    bloom: Option<f32>,
    tracking: Option<f32>,
    tracking_period: Option<f32>,
    tracking_sweep: Option<f32>,
    flicker: Option<f32>,
    jiggle: Option<f32>,
    curvature: Option<f32>,
    screen_glare: Option<f32>,
}

#[derive(Deserialize, Default)]
struct FileFont {
    family: Option<String>,
    size: Option<f32>,
}

#[derive(Deserialize)]
struct ThemeFile {
    name: Option<String>,
    colors: FileColors,
    #[serde(default)]
    shell: FileShell,
    #[serde(default)]
    effects: FileEffects,
    #[serde(default)]
    font: FileFont,
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub bg: Hsla,
    pub surface: Hsla,
    pub text: Hsla,
    pub accent: Hsla,
    pub faint: Hsla,
    pub frame_bg: Hsla,
    pub frame_text: Hsla,
    pub frame_border: Hsla,
    pub frame_faint: Hsla,
    pub scanline_opacity: f32,
    pub scanline_step: f32,
    pub vignette: f32,
    pub glow: f32,
    pub bloom: f32,
    pub tracking: f32,
    pub tracking_period: f32,
    pub tracking_sweep: f32,
    pub flicker: f32,
    pub jiggle: f32,
    pub curvature: f32,
    pub screen_glare: f32,
    pub font_family: String,
    pub font_size: f32,
}

pub struct ActiveTheme(pub Arc<Theme>);
impl Global for ActiveTheme {}

pub fn theme(cx: &App) -> Arc<Theme> {
    cx.global::<ActiveTheme>().0.clone()
}

/// All selectable themes: bundled + any from ~/.config/markdown-delight/themes.
/// Built once at init; per-pane theme overrides resolve names against this.
pub struct ThemeRegistry(pub Vec<Arc<Theme>>);
impl Global for ThemeRegistry {}

impl ThemeRegistry {
    pub fn by_name(&self, name: &str) -> Option<Arc<Theme>> {
        self.0.iter().find(|t| t.name == name).cloned()
    }
}

/// All registered themes as `(name, glyph)`, in cycle order — drives the theme
/// tray's icon row. Glyph is a small per-theme emblem (terminal-delight style).
pub fn registry_list(cx: &App) -> Vec<(String, &'static str)> {
    cx.global::<ThemeRegistry>()
        .0
        .iter()
        .map(|t| (t.name.clone(), theme_glyph(&t.name)))
        .collect()
}

/// A small emblem for a theme name, for the tray's THEME row.
pub fn theme_glyph(name: &str) -> &'static str {
    match name {
        "hacker" => ">_",
        "amber" => "★",
        "ice" => "❄",
        "paper" => "☼",
        _ => "◆",
    }
}

/// Parse a `#rrggbb` swatch into an Hsla (for the tray's SEED COLOUR row).
pub fn parse_hex(s: &str) -> Option<Hsla> {
    hex(s)
}

/// Fold a theme's phosphor onto one seed hue: accent / text / faint take the
/// seed's hue+saturation but KEEP their own lightness (so contrast and
/// readability survive), while bg/surface stay put. This is what the SEED
/// COLOUR swatches apply — a whole-tube recolour layered over the base theme.
pub fn recolor(base: &Theme, seed: Hsla) -> Theme {
    let fold = |c: Hsla| Hsla {
        h: seed.h,
        s: seed.s,
        l: c.l,
        a: c.a,
    };
    let mut t = base.clone();
    t.accent = fold(t.accent);
    t.text = fold(t.text);
    t.faint = fold(t.faint);
    t
}

/// Look up a theme by name from the global registry (None → global active
/// theme). The hot-reloaded `theme.toml` is kept in the registry under its own
/// name (see `upsert_registry`), so resolving it by name returns the live copy.
pub fn resolve(cx: &App, name: Option<&str>) -> Arc<Theme> {
    match name {
        Some(n) => cx
            .global::<ThemeRegistry>()
            .by_name(n)
            .unwrap_or_else(|| theme(cx)),
        None => theme(cx),
    }
}

/// Insert or replace a theme in the registry by name — keeps the registry the
/// single source of truth for `resolve`, including the live hot-reloaded theme.
fn upsert_registry(cx: &mut App, t: Arc<Theme>) {
    if !cx.has_global::<ThemeRegistry>() {
        cx.set_global(ThemeRegistry(vec![t]));
        return;
    }
    let reg = cx.global_mut::<ThemeRegistry>();
    match reg.0.iter_mut().find(|x| x.name == t.name) {
        Some(slot) => *slot = t,
        None => reg.0.push(t),
    }
}

fn themes_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".config/markdown-delight/themes"))
}

/// Bundled themes first (cycle order), then user themes (override by name / append).
fn build_registry() -> Vec<Arc<Theme>> {
    let mut out: Vec<Arc<Theme>> = Vec::new();
    for src in BUNDLED_THEMES {
        if let Ok(t) = parse(src) {
            out.push(Arc::new(t));
        }
    }
    if let Some(dir) = themes_dir() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.extension().and_then(|s| s.to_str()) != Some("toml") {
                    continue;
                }
                if let Ok(src) = fs::read_to_string(&p) {
                    if let Ok(t) = parse(&src) {
                        let t = Arc::new(t);
                        match out.iter_mut().find(|x| x.name == t.name) {
                            Some(slot) => *slot = t,
                            None => out.push(t),
                        }
                    }
                }
            }
        }
    }
    out
}

/// Push the curvature dial into the renderer's CRT warp pass (td-crt-pass patch).
fn apply_warp(theme: &Theme) {
    #[cfg(target_os = "linux")]
    gpui_wgpu::set_crt_warp(theme.curvature * 0.14, theme.curvature * 0.06);
}

/// Push the CURRENT active theme's curvature dial into the renderer (the global
/// gate for the per-tube warp). Call after swapping the outer theme at runtime.
pub fn apply_warp_theme(cx: &App) {
    apply_warp(&theme(cx));
}

fn hex(value: &str) -> Option<Hsla> {
    let v = value.trim().trim_start_matches('#');
    if v.len() != 6 {
        return None;
    }
    u32::from_str_radix(v, 16).ok().map(|c| rgb(c).into())
}

fn parse(source: &str) -> Result<Theme, String> {
    let file: ThemeFile = toml::from_str(source).map_err(|e| e.to_string())?;
    let c = &file.colors;
    let need = |s: &String, what: &str| hex(s).ok_or(format!("bad color for {what}: {s}"));
    let opt =
        |s: &Option<String>, fallback: Hsla| s.as_ref().and_then(|v| hex(v)).unwrap_or(fallback);
    let accent = need(&c.accent, "accent")?;
    let surface = need(&c.surface, "surface")?;
    let name = file.name.unwrap_or_else(|| "unnamed".into());
    let default_screen_glare = if name == "hacker" { 0.42 } else { 0.0 };
    Ok(Theme {
        name,
        bg: need(&c.bg, "bg")?,
        surface,
        text: need(&c.text, "text")?,
        accent,
        faint: need(&c.faint, "faint")?,
        frame_bg: opt(&file.shell.frame_bg, surface),
        frame_text: opt(&file.shell.frame_text, accent),
        frame_border: opt(&file.shell.frame_border, accent),
        frame_faint: opt(&file.shell.frame_faint, accent),
        scanline_opacity: file.effects.scanline_opacity.unwrap_or(0.).clamp(0., 0.6),
        scanline_step: file.effects.scanline_step.unwrap_or(4.).max(2.),
        vignette: file.effects.vignette.unwrap_or(0.).clamp(0., 1.),
        glow: file.effects.glow.unwrap_or(0.).clamp(0., 1.),
        bloom: file.effects.bloom.unwrap_or(0.).clamp(0., 1.),
        tracking: file.effects.tracking.unwrap_or(0.).clamp(0., 1.),
        tracking_period: file.effects.tracking_period.unwrap_or(14.).clamp(2., 120.),
        tracking_sweep: file.effects.tracking_sweep.unwrap_or(7.).clamp(1., 30.),
        flicker: file.effects.flicker.unwrap_or(0.).clamp(0., 1.),
        jiggle: file.effects.jiggle.unwrap_or(0.).clamp(0., 1.),
        curvature: file.effects.curvature.unwrap_or(0.).clamp(0., 1.),
        screen_glare: file
            .effects
            .screen_glare
            .unwrap_or(default_screen_glare)
            .clamp(0., 1.),
        font_family: file.font.family.unwrap_or_else(|| "JetBrains Mono".into()),
        font_size: file.font.size.unwrap_or(14.).clamp(8., 32.),
    })
}

fn theme_path() -> PathBuf {
    if let Ok(p) = std::env::var("MD_THEME") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/markdown-delight/theme.toml")
}

fn mtime(path: &PathBuf) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Load the theme, seed the user config on first run, start the hot-reload watcher.
pub fn init(cx: &mut App) {
    let path = theme_path();
    if std::env::var("MD_THEME").is_err() && !path.exists() {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(&path, DEFAULT_THEME_TOML);
    }
    let initial = fs::read_to_string(&path)
        .ok()
        .and_then(|s| parse(&s).ok())
        .unwrap_or_else(|| parse(DEFAULT_THEME_TOML).expect("embedded theme parses"));
    apply_warp(&initial);
    let initial = Arc::new(initial);
    cx.set_global(ActiveTheme(initial.clone()));
    cx.set_global(ThemeRegistry(build_registry()));
    // the live theme.toml wins its name slot in the registry (so panes that
    // resolve it by name get the hot-reloaded copy, not the bundled one)
    upsert_registry(cx, initial);

    let mut last = mtime(&path);
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let now = mtime(&path);
            if now != last {
                last = now;
                if let Ok(source) = fs::read_to_string(&path) {
                    match parse(&source) {
                        Ok(theme) => {
                            apply_warp(&theme);
                            let theme = Arc::new(theme);
                            cx.update(|cx| {
                                cx.set_global(ActiveTheme(theme.clone()));
                                // keep the registry's same-named slot live too,
                                // so inheriting panes pick up the edit
                                upsert_registry(cx, theme);
                                cx.refresh_windows();
                            });
                        }
                        Err(err) => eprintln!("theme reload error (keeping current): {err}"),
                    }
                }
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_known() {
        // #22c55e is the hacker accent — round-trips to a green hue.
        let c = parse_hex("#22c55e").expect("valid hex");
        assert!((0.25..0.45).contains(&c.h), "green hue, got {}", c.h);
        assert!(parse_hex("nope").is_none());
        assert!(parse_hex("#12345").is_none()); // wrong length
    }

    #[test]
    fn recolor_folds_hue_keeps_lightness() {
        let base = parse("name='t'\n[colors]\nbg='#000000'\nsurface='#111111'\ntext='#86efac'\naccent='#22c55e'\nfaint='#14401f'\n").expect("parses");
        let seed = parse_hex("#2f6fdd").expect("blue"); // ~0.6 hue
        let out = recolor(&base, seed);
        // accent/text/faint take the seed hue+sat...
        assert!((out.accent.h - seed.h).abs() < 1e-4);
        assert!((out.text.h - seed.h).abs() < 1e-4);
        assert!((out.faint.h - seed.h).abs() < 1e-4);
        // ...but KEEP their own lightness (readability preserved)
        assert!((out.text.l - base.text.l).abs() < 1e-6);
        assert!((out.accent.l - base.accent.l).abs() < 1e-6);
        // bg/surface are untouched
        assert_eq!(out.bg, base.bg);
        assert_eq!(out.surface, base.surface);
    }
}
