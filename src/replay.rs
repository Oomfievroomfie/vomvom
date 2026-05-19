// Scripted input replay for headless debugging.
// Build a sequence of ScriptedEvent values and call run_script() to drive
// an in-memory session through them, saving numbered PNGs at Screenshot markers.
//
// Usage (from main or a test):
//   use crate::replay::{ScriptedEvent::*, run_script};
//   run_script("my_test", 1024, 768, vec![
//       Type("hello\n"),
//       Screenshot,
//       Click(100.0, 200.0),
//       ScreenshotNamed("after_click"),
//   ]);
//
// Output folder: replay_screenshots/<name>/
// Output files:  0000.png, 0001.png, ...  or  0000_label.png, 0001_label.png, ...

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::collections::HashMap;

use femtovg::{renderer::OpenGl, Canvas, FontId, Path as FPath, Paint};
use glutin::{
    config::ConfigTemplateBuilder,
    context::{ContextApi, ContextAttributesBuilder, NotCurrentGlContext},
    display::GetGlDisplay,
    prelude::*,
    surface::{Surface, SurfaceAttributesBuilder, WindowSurface},
};
use glutin_winit::DisplayBuilder;
use raw_window_handle::HasWindowHandle;
use winit::{
    event_loop::EventLoop,
    window::WindowAttributes,
    dpi::PhysicalSize,
};

use crate::render::glyph_cache::GlyphCache;
use crate::render::layout::LayoutBox;
use crate::render::tree::Document;
use crate::session::Session;
use crate::{
    build_demo_scene, rebuild_highlight_cache, render_frame,
    build_stylesheet, editor_content_height,
    scroll_to_cursor, close_all_menus, open_menu, any_menu_open,
    hit_test_menu_item, hit_test_menu_header, hit_test_tab,
    execute_menu_action, hit_test_editor,
    SANS_BYTES, MONO_BYTES,
};

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
    /// Scroll the active buffer to scroll_line.
    ScrollTo(usize),
    /// Save a screenshot at this point.  Files are named <prefix>_NNN.png.
    Screenshot,
    /// Save a screenshot with a custom label suffix, e.g. "before_close".
    ScreenshotNamed(&'a str),
}

struct ReplayState {
    canvas: Canvas<OpenGl>,
    gl_surface: Surface<WindowSurface>,
    gl_context: glutin::context::PossiblyCurrentContext,
    glyph_cache: GlyphCache,
    femtovg_fonts: Option<(FontId, FontId)>,
    session: Session,
    doc: Document,
    highlight_cache: HashMap<i64, Vec<Vec<(String, &'static str)>>>,
    mouse_pos: (f32, f32),
    last_layout: Option<LayoutBox>,
    editor_font_size: f32,
    width: u32,
    height: u32,
}

impl ReplayState {
    fn render_to_pixels(&mut self, mouse_pos: Option<(f32, f32)>) -> (Vec<u8>, u32, u32) {
        let w = self.width as f32;
        let h = self.height as f32;
        let font_size = self.editor_font_size;
        let sheet = build_stylesheet(font_size);

        let lb = render_frame(
            &mut self.canvas,
            &mut self.glyph_cache,
            &mut self.doc,
            &self.session,
            &self.highlight_cache,
            &sheet,
            font_size,
            w, h, 1.0,
            self.femtovg_fonts,
            true,
            true,
            self.last_layout.as_ref(),
        );
        self.last_layout = Some(lb.clone());

        // Paint selection if any.
        let buf = self.session.active();
        let scroll = buf.scroll_line;
        let mono_font = self.femtovg_fonts.map(|(_, m)| m);
        if let Some((sel_start, sel_end)) = buf.selection_range() {
            crate::paint_selection(
                &mut self.canvas, &lb, &self.doc.root,
                sel_start, sel_end, scroll,
                MONO_BYTES, mono_font, font_size,
            );
        }

        // Paint cursor.
        if buf.cursor.line >= scroll {
            let line_text = buf.line(buf.cursor.line);
            let layout_line = buf.cursor.line - scroll;
            let cursors = vec![(layout_line, buf.cursor.col, line_text.as_str())];
            crate::paint_cursors_with_text(
                &mut self.canvas, &lb, &self.doc.root,
                &cursors, MONO_BYTES, mono_font, font_size,
            );
        }

        // Paint dummy mouse cursor if a position is supplied.
        if let Some((mx, my)) = mouse_pos {
            paint_mouse_cursor(&mut self.canvas, mx, my);
        }

        self.canvas.flush();

        let img = self.canvas.screenshot().expect("screenshot failed");
        let (iw, ih) = (img.width(), img.height());
        let pixels: Vec<u8> = img.pixels().flat_map(|p| [p.r, p.g, p.b, p.a]).collect();

        self.gl_surface.swap_buffers(&self.gl_context).unwrap();

        (pixels, iw as u32, ih as u32)
    }
}

fn paint_mouse_cursor(canvas: &mut Canvas<OpenGl>, mx: f32, my: f32) {
    // Draw a simple crosshair + dot.
    let arm = 8.0_f32;
    let r = 4.0_f32;
    let col = femtovg::Color::rgbaf(1.0, 0.2, 0.2, 0.9);
    let outline = femtovg::Color::rgbaf(0.0, 0.0, 0.0, 0.7);

    // Crosshair outline.
    for (dx, dy) in &[(-1.0_f32, 0.0_f32), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
        let mut p = FPath::new();
        p.move_to(mx - arm + dx, my + dy);
        p.line_to(mx + arm + dx, my + dy);
        p.move_to(mx + dx, my - arm + dy);
        p.line_to(mx + dx, my + arm + dy);
        canvas.stroke_path(&p, &Paint::color(outline).with_line_width(3.0));
    }
    // Crosshair.
    let mut p = FPath::new();
    p.move_to(mx - arm, my);
    p.line_to(mx + arm, my);
    p.move_to(mx, my - arm);
    p.line_to(mx, my + arm);
    canvas.stroke_path(&p, &Paint::color(col).with_line_width(1.5));

    // Dot outline.
    let mut circle = FPath::new();
    circle.circle(mx, my, r + 1.0);
    canvas.fill_path(&circle, &Paint::color(outline));

    // Dot.
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
    let event_loop = EventLoop::new().unwrap();

    let win_attrs = WindowAttributes::default()
        .with_title("vomvom-replay")
        .with_inner_size(PhysicalSize::new(width, height))
        .with_visible(false);

    let template = ConfigTemplateBuilder::new().with_alpha_size(8);
    let (window, gl_config) = DisplayBuilder::new()
        .with_window_attributes(Some(win_attrs))
        .build(&event_loop, template, |configs| {
            configs
                .reduce(|a, b| if b.num_samples() > a.num_samples() { b } else { a })
                .unwrap()
        })
        .expect("replay: failed to create window");

    let window = Arc::new(window.unwrap());
    let raw_handle = window.window_handle().unwrap();

    let ctx_attrs = ContextAttributesBuilder::new().build(Some(raw_handle.as_raw()));
    let fallback = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::Gles(None))
        .build(Some(raw_handle.as_raw()));

    let gl_display = gl_config.display();
    let not_current = unsafe {
        gl_display.create_context(&gl_config, &ctx_attrs)
            .or_else(|_| gl_display.create_context(&gl_config, &fallback))
            .expect("replay: failed to create GL context")
    };

    let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
        raw_handle.as_raw(),
        NonZeroU32::new(width.max(1)).unwrap(),
        NonZeroU32::new(height.max(1)).unwrap(),
    );
    let gl_surface: Surface<WindowSurface> = unsafe {
        gl_display.create_window_surface(&gl_config, &surface_attrs)
            .expect("replay: failed to create GL surface")
    };
    let gl_context = not_current.make_current(&gl_surface).unwrap();

    let renderer = unsafe {
        OpenGl::new_from_function_cstr(|s| gl_display.get_proc_address(s) as *const _)
            .expect("replay: failed to create femtovg renderer")
    };

    let mut canvas = Canvas::new(renderer).expect("replay: failed to create canvas");
    let sans_id = canvas.add_font_mem(SANS_BYTES).expect("load sans");
    let mono_id = canvas.add_font_mem(MONO_BYTES).expect("load mono");

    let (doc, _sheet, session) = build_demo_scene();
    let mut highlight_cache = HashMap::new();
    rebuild_highlight_cache(&mut highlight_cache, &session);

    let mut state = ReplayState {
        canvas,
        gl_surface,
        gl_context,
        glyph_cache: GlyphCache::new(),
        femtovg_fonts: Some((sans_id, mono_id)),
        session,
        doc,
        highlight_cache,
        mouse_pos: (0.0, 0.0),
        last_layout: None,
        editor_font_size: 11.5,
        width,
        height,
    };

    let mut shot_idx = 0usize;

    for event in &events {
        match event {
            ScriptedEvent::Type(text) => {
                state.session.active_mut().insert(text);
                rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
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
            }
            ScriptedEvent::MouseMove(x, y) => {
                state.mouse_pos = (*x, *y);
            }
            ScriptedEvent::Click => {
                let (mx, my) = state.mouse_pos;
                handle_click(&mut state, mx, my, false);
            }
            ScriptedEvent::ClickAt(x, y) => {
                state.mouse_pos = (*x, *y);
                handle_click(&mut state, *x, *y, false);
            }
            ScriptedEvent::ShiftClickAt(x, y) => {
                state.mouse_pos = (*x, *y);
                handle_click(&mut state, *x, *y, true);
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
            ScriptedEvent::ScrollTo(line) => {
                state.session.active_mut().scroll_line = *line;
            }
            ScriptedEvent::Screenshot => {
                let mp = Some(state.mouse_pos);
                let (pixels, w, h) = state.render_to_pixels(mp);
                let path_str = format!("{}/{:04}.png", folder, shot_idx);
                save_pixels(&pixels, w, h, Path::new(&path_str));
                shot_idx += 1;
            }
            ScriptedEvent::ScreenshotNamed(label) => {
                let mp = Some(state.mouse_pos);
                let (pixels, w, h) = state.render_to_pixels(mp);
                let path_str = format!("{}/{:04}_{}.png", folder, shot_idx, label);
                save_pixels(&pixels, w, h, Path::new(&path_str));
                shot_idx += 1;
            }
        }
    }
}

fn handle_click(state: &mut ReplayState, mx: f32, my: f32, shift: bool) {
    // Render a frame first to get up-to-date layout.
    let _ = state.render_to_pixels(None);
    let Some(ref lb) = state.last_layout.clone() else { return };

    if any_menu_open(&state.doc) {
        if let Some(action) = hit_test_menu_item(&state.doc.root, lb, mx, my) {
            close_all_menus(&mut state.doc);
            execute_menu_action(&action, &mut state.session);
            rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
            return;
        }
        close_all_menus(&mut state.doc);
        return;
    }
    if let Some(menu_id) = hit_test_menu_header(&state.doc.root, lb, mx, my) {
        open_menu(&mut state.doc, &menu_id);
        return;
    }
    if let Some(idx) = hit_test_tab(&state.doc.root, lb, mx, my) {
        state.session.set_active(idx);
        rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
        return;
    }
    // Editor click.
    let mono_font = state.femtovg_fonts.map(|(_, m)| m);
    if let Some((line, col)) = hit_test_editor(
        &mut state.canvas, lb, &state.doc.root, &state.session,
        mx, my, MONO_BYTES, mono_font, state.editor_font_size,
    ) {
        let buf = state.session.active_mut();
        if shift {
            buf.set_anchor_if_none();
        } else {
            buf.clear_selection();
        }
        buf.move_cursor(line, col);
        buf.break_undo_group();
        let editor_h = editor_content_height(state.height as f32);
        scroll_to_cursor(&mut state.session, editor_h, state.editor_font_size);
    }
}
