//! Hot-reloaded theme — ported from terminal-delight (MIT sibling), extended
//! with the [shell] section (the complement-coloured monitor housing).
//! Source of truth: ~/.config/markdown-delight/theme.toml (seeded with the
//! hacker theme on first run) → embedded default. A background task polls
//! mtime (~300ms) and swaps the global on change. Day-one pillar: modify on
//! the fly, no recompile.

use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, SystemTime},
};

use gpui::{App, Global, Hsla, rgb};
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
    FONT_SCALE.store(v.clamp(FONT_SCALE_MIN, FONT_SCALE_MAX).to_bits(), Ordering::Relaxed);
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
    /// The theme that follows `name` in cycle order (wraps). Unknown name → first.
    pub fn next_after(&self, name: &str) -> Option<Arc<Theme>> {
        if self.0.is_empty() {
            return None;
        }
        let next = match self.0.iter().position(|t| t.name == name) {
            Some(i) => (i + 1) % self.0.len(),
            None => 0,
        };
        self.0.get(next).cloned()
    }
}

/// Look up a theme by name from the global registry (None → global active theme).
pub fn resolve(cx: &App, name: Option<&str>) -> Arc<Theme> {
    match name {
        Some(n) => cx
            .global::<ThemeRegistry>()
            .by_name(n)
            .unwrap_or_else(|| theme(cx)),
        None => theme(cx),
    }
}

/// The theme name to cycle to after `current` (None = global), for the per-pane
/// theme chip. Always returns Some when the registry is non-empty.
pub fn cycle_name(cx: &App, current: Option<&str>) -> Option<String> {
    let reg = cx.global::<ThemeRegistry>();
    let base = current
        .map(|s| s.to_string())
        .unwrap_or_else(|| theme(cx).name.clone());
    reg.next_after(&base).map(|t| t.name.clone())
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
    let opt = |s: &Option<String>, fallback: Hsla| s.as_ref().and_then(|v| hex(v)).unwrap_or(fallback);
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
    cx.set_global(ActiveTheme(Arc::new(initial)));
    cx.set_global(ThemeRegistry(build_registry()));

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
                            let _ = cx.update(|cx| {
                                cx.set_global(ActiveTheme(Arc::new(theme)));
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
