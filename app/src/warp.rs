//! Per-pane CRT warp registry. Each visible pane registers its content rect
//! (physical px) during prepaint; the renderer's td-crt-pass warps exactly
//! those rects, leaving chrome flat so hit-testing stays honest.
//! The workspace clears the set at the start of every frame.

use std::sync::Mutex;

static RECTS: Mutex<Vec<([f32; 4], f32)>> = Mutex::new(Vec::new());

pub fn begin_frame() {
    let mut rects = RECTS.lock().unwrap();
    rects.clear();
    push(&rects);
}

#[allow(dead_code)]
pub fn register(rect: [f32; 4]) {
    register_with_glare(rect, 0.0);
}

pub fn register_with_glare(rect: [f32; 4], glare: f32) {
    let mut rects = RECTS.lock().unwrap();
    if rects.len() < 8 {
        rects.push((rect, glare.clamp(0.0, 1.0)));
    }
    push(&rects);
}

#[allow(unused_variables)]
fn push(rects: &[([f32; 4], f32)]) {
    #[cfg(target_os = "linux")]
    gpui_wgpu::set_crt_rects_with_glare(rects);
}
