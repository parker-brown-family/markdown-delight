//! Per-pane CRT warp registry. Each visible pane registers its content rect
//! (physical px) during prepaint; the renderer's td-crt-pass barrel-warps
//! exactly those rects, leaving chrome flat so hit-testing stays honest.
//! The workspace clears the set at the start of every frame.
//!
//! Curvature is PER TUBE: each pane carries the barrel coefficients (k1, k2) of
//! its OWN resolved theme, so a bent pane bows even when the window theme is
//! flat — and a flat pane stays flat beside a bent one. (The v2 td-crt shader
//! is per-rect authoritative: a tube with k≈0 passes straight through, which is
//! why registering with a zero curvature — the old `register_with_glare` — made
//! the whole screen look flat regardless of the global warp dial.)
//!
//! When an overlay is open (the theme tray or the unsaved-changes modal) the
//! workspace SUPPRESSES the pass for that frame: the warp is a pixel
//! post-process, so a panel floating over a tube would bow with the glass while
//! gpui keeps hit-testing its flat layout box — visibly off-target. Suppressing
//! registers no tubes, so the pass is a no-op and the glass reads flat: what you
//! see is what you click.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// One registered tube: (rect[x,y,w,h] physical px, glass glare, k1, k2).
type Tube = ([f32; 4], f32, f32, f32);

static RECTS: Mutex<Vec<Tube>> = Mutex::new(Vec::new());
static SUPPRESSED: AtomicBool = AtomicBool::new(false);

/// Suppress the warp pass for the current frame (set in the workspace render
/// before panes paint). While suppressed no tube registers, so the renderer's
/// rect set is empty and the pass is a no-op — the glass reads flat.
pub fn set_suppressed(suppressed: bool) {
    SUPPRESSED.store(suppressed, Ordering::Relaxed);
}

pub fn begin_frame() {
    let mut rects = RECTS.lock().unwrap();
    rects.clear();
    push(&rects);
}

/// Register one pane's tube for this frame: its content rect (physical px), the
/// top-left glass glare, and its own barrel curvature (k1, k2) from its
/// resolved theme. A genuinely flat tube (k1 = k2 = 0) passes through untouched.
pub fn register_tube(rect: [f32; 4], glare: f32, k1: f32, k2: f32) {
    if SUPPRESSED.load(Ordering::Relaxed) {
        return;
    }
    let mut rects = RECTS.lock().unwrap();
    if rects.len() < 8 {
        rects.push((rect, glare.clamp(0.0, 1.0), k1, k2));
    }
    push(&rects);
}

#[allow(unused_variables)]
fn push(rects: &[Tube]) {
    #[cfg(target_os = "linux")]
    gpui_wgpu::set_crt_rects_tubes(rects);
}
