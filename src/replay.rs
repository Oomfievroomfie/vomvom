// Scripted input replay for headless debugging.
// Build a sequence of ScriptedEvent values and call run_script() to drive
// the real AppState through them, saving numbered PNGs at Screenshot markers.
//
// Usage (from main or a test):
//   use crate::replay::{ScriptedEvent::*, run_script};
//   run_script("my_test", 1024, 768, vec![
//       Type("hello\n"),
//       Screenshot,
//       ClickAt(100.0, 200.0),
//       ScreenshotNamed("after_click"),
//   ]);
//
// Output folder: replay_screenshots/<name>/
// Output files:  0000.png, 0001.png, ...  or  0000_label.png

use std::path::Path;

use femtovg::{renderer::OpenGl, Canvas, Paint, Path as FPath};

use crate::headless_gl;
use crate::AppState;
use crate::{close_all_menus, execute_menu_action, open_menu, rebuild_highlight_cache};

/// One step in a scripted input sequence.
#[derive(Clone)]
pub enum ScriptedEvent<'a> {
    /// Type a string (inserts chars one at a time through buffer.insert, supports \n).
    Type(&'a str),
    /// Press Backspace once.
    Backspace,
    /// Press Delete once.
    Delete,
    /// Move cursor to logical (line, col) directly.
    MoveCursor(usize, usize),
    /// Move the virtual mouse to (x, y) in screen pixels.
    MouseMove(f32, f32),
    /// Left-click at the current mouse position.
    Click,
    /// Left-click at (x, y) (sets mouse pos first).
    ClickAt(f32, f32),
    /// Shift+click at (x, y).
    ShiftClickAt(f32, f32),
    /// Click at (x, y) then drag to (x2, y2), simulating click-and-drag selection.
    DragFrom(f32, f32, f32, f32),
    /// Ctrl+Z undo.
    Undo,
    /// Ctrl+Y redo.
    Redo,
    /// Open a named menu ("file" or "edit").
    OpenMenu(&'a str),
    /// Close all menus.
    CloseMenus,
    /// Execute a menu action by name (without going through menu UI).
    MenuAction(&'a str),
    /// Set the editor font size in pixels.
    SetFontSize(f32),
    /// Scroll the active buffer to scroll_line.
    ScrollTo(usize),
    /// Save a screenshot at this point.  Files are named <prefix>_NNN.png.
    Screenshot,
    /// Save a screenshot with a custom label suffix, e.g. "before_close".
    ScreenshotNamed(&'a str),
}

fn paint_mouse_cursor(canvas: &mut Canvas<OpenGl>, mx: f32, my: f32) {
    let arm = 8.0_f32;
    let r = 4.0_f32;
    let col = femtovg::Color::rgbaf(1.0, 0.2, 0.2, 0.9);
    let outline = femtovg::Color::rgbaf(0.0, 0.0, 0.0, 0.7);

    for (dx, dy) in &[(-1.0_f32, 0.0_f32), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
        let mut p = FPath::new();
        p.move_to(mx - arm + dx, my + dy);
        p.line_to(mx + arm + dx, my + dy);
        p.move_to(mx + dx, my - arm + dy);
        p.line_to(mx + dx, my + arm + dy);
        canvas.stroke_path(&p, &Paint::color(outline).with_line_width(3.0));
    }
    let mut p = FPath::new();
    p.move_to(mx - arm, my);
    p.line_to(mx + arm, my);
    p.move_to(mx, my - arm);
    p.line_to(mx, my + arm);
    canvas.stroke_path(&p, &Paint::color(col).with_line_width(1.5));

    let mut circle = FPath::new();
    circle.circle(mx, my, r + 1.0);
    canvas.fill_path(&circle, &Paint::color(outline));

    let mut dot = FPath::new();
    dot.circle(mx, my, r);
    canvas.fill_path(&dot, &Paint::color(col));
}

fn save_pixels(pixels: &[u8], w: u32, h: u32, path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    image::save_buffer(path, pixels, w, h, image::ColorType::Rgba8)
        .expect("failed to save PNG");
    println!("[replay] screenshot saved: {}", path.display());
}

pub fn run_script(name: &str, width: u32, height: u32, events: Vec<ScriptedEvent<'_>>) {
    let folder = format!("replay_screenshots/{}", name);
    std::fs::create_dir_all(&folder).expect("replay: failed to create output folder");

    let (hgl, _event_loop) = headless_gl::setup(width, height);
    let mut state = AppState::new_headless(hgl.canvas, hgl.gl_surface, hgl.gl_context, width, height);

    // Prime layout with an initial render before processing events.
    state.do_render();

    let mut shot_idx = 0usize;

    for event in &events {
        match event {
            ScriptedEvent::Type(text) => {
                state.session.active_mut().insert(text);
                rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
                state.do_render();
            }
            ScriptedEvent::Backspace => {
                state.session.active_mut().backspace();
                rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
            }
            ScriptedEvent::Delete => {
                state.session.active_mut().delete_forward();
                rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
            }
            ScriptedEvent::MoveCursor(line, col) => {
                state.session.active_mut().move_cursor(*line, *col);
                state.do_render();
            }
            ScriptedEvent::MouseMove(x, y) => {
                state.mouse_pos = (*x, *y);
            }
            ScriptedEvent::Click => {
                let (mx, my) = state.mouse_pos;
                // Render first so hit-testing uses current layout.
                state.do_render();
                state.on_mouse_press(mx, my, false);
                state.on_mouse_release();
            }
            ScriptedEvent::ClickAt(x, y) => {
                state.mouse_pos = (*x, *y);
                state.do_render();
                state.on_mouse_press(*x, *y, false);
                state.on_mouse_release();
            }
            ScriptedEvent::ShiftClickAt(x, y) => {
                state.mouse_pos = (*x, *y);
                state.do_render();
                state.on_mouse_press(*x, *y, true);
                state.on_mouse_release();
            }
            ScriptedEvent::DragFrom(x1, y1, x2, y2) => {
                state.mouse_pos = (*x1, *y1);
                state.do_render();
                state.on_mouse_press(*x1, *y1, false);
                // Simulate drag through interpolated steps.
                for i in 1..=8 {
                    let t = i as f32 / 8.0;
                    let mx = x1 + (x2 - x1) * t;
                    let my = y1 + (y2 - y1) * t;
                    state.mouse_pos = (mx, my);
                    state.do_render();
                    state.on_mouse_drag(mx, my);
                }
                state.on_mouse_release();
            }
            ScriptedEvent::Undo => {
                state.session.active_mut().undo();
                rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
            }
            ScriptedEvent::Redo => {
                state.session.active_mut().redo();
                rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
            }
            ScriptedEvent::OpenMenu(menu_id) => {
                open_menu(&mut state.doc, menu_id);
            }
            ScriptedEvent::CloseMenus => {
                close_all_menus(&mut state.doc);
            }
            ScriptedEvent::MenuAction(action) => {
                close_all_menus(&mut state.doc);
                execute_menu_action(action, &mut state.session);
                rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
            }
            ScriptedEvent::SetFontSize(size) => {
                state.editor_font_size = *size;
                state.sheet = crate::build_stylesheet(*size);
                state.do_render();
            }
            ScriptedEvent::ScrollTo(line) => {
                state.session.active_mut().scroll_line = *line;
            }
            ScriptedEvent::Screenshot => {
                state.do_render();
                let (mx, my) = state.mouse_pos;
                let (mut pixels, w, h) = state.capture_pixels();
                paint_mouse_cursor_onto(&mut pixels, w, h, mx, my, &mut state.canvas);
                let path_str = format!("{}/{:04}.png", folder, shot_idx);
                save_pixels(&pixels, w, h, Path::new(&path_str));
                shot_idx += 1;
            }
            ScriptedEvent::ScreenshotNamed(label) => {
                state.do_render();
                let (mx, my) = state.mouse_pos;
                let (mut pixels, w, h) = state.capture_pixels();
                paint_mouse_cursor_onto(&mut pixels, w, h, mx, my, &mut state.canvas);
                let path_str = format!("{}/{:04}_{}.png", folder, shot_idx, label);
                save_pixels(&pixels, w, h, Path::new(&path_str));
                shot_idx += 1;
            }
        }
    }
}

/// Render a mouse cursor overlay onto already-captured pixel data.
/// Re-renders one extra pass with the cursor painted, then grabs pixels again.
fn paint_mouse_cursor_onto(
    pixels: &mut Vec<u8>,
    w: u32,
    h: u32,
    mx: f32,
    my: f32,
    canvas: &mut Canvas<OpenGl>,
) {
    // We draw on top of the existing canvas state (which already has the frame).
    paint_mouse_cursor(canvas, mx, my);
    canvas.flush();
    if let Ok(img) = canvas.screenshot() {
        *pixels = img.pixels().flat_map(|p| [p.r, p.g, p.b, p.a]).collect();
    }
    let _ = (w, h);
}
