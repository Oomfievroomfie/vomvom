// Headless screenshot: spin up a hidden window, render one frame, save PNG.

use std::path::Path;

use crate::headless_gl;
use crate::AppState;

pub fn save_screenshot(path: &Path, width: u32, height: u32) {
    let (hgl, _event_loop) = headless_gl::setup(width, height);
    let mut state = AppState::new_headless(hgl.canvas, hgl.gl_surface, hgl.gl_context, width, height);
    let (pixels, w, h) = state.capture_pixels();
    image::save_buffer(path, &pixels, w, h, image::ColorType::Rgba8)
        .expect("failed to save PNG");
    println!("Screenshot saved to {}", path.display());
}
