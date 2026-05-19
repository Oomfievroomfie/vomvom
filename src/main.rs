mod highlight;
mod render;
mod replay;
mod screenshot;
mod session;

use std::num::NonZeroU32;
use std::time::{Duration, Instant};
use std::sync::Arc;

use femtovg::{renderer::OpenGl, Canvas, FontId, Paint, Path};
use glutin::{
    config::ConfigTemplateBuilder,
    context::{ContextApi, ContextAttributesBuilder, PossiblyCurrentContext},
    display::GetGlDisplay,
    prelude::*,
    surface::{Surface, SurfaceAttributesBuilder, WindowSurface},
};
use glutin_winit::DisplayBuilder;
use raw_window_handle::HasWindowHandle;
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, Modifiers, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    window::{Window, WindowId},
};

use render::layout::{layout, finalize_positions, Constraints, LayoutBox};
use render::paint::{PaintContext, paint_tree_root};
use render::style::Stylesheet;
use render::tree::{Document, Node, apply_styles};
use render::glyph_cache::GlyphCache;
use render::css_parse::parse_stylesheet;
use render::html_parse::parse_html;
use session::Session;

static MAIN_CSS: &str = include_str!("../assets/main.cssv");
static DROPDOWN_FILE: &str = include_str!("../assets/dropdown_file.htmv");
static DROPDOWN_EDIT: &str = include_str!("../assets/dropdown_edit.htmv");

static SANS_BYTES: &[u8] = include_bytes!("../OpenSans-Medium.ttf");
static MONO_BYTES: &[u8] = include_bytes!("../Sono-Medium.ttf");

struct App {
    state: Option<AppState>,
    db_path: String,
    initial_files: Vec<String>,
}

struct AppState {
    window: Arc<Window>,
    canvas: Canvas<OpenGl>,
    gl_surface: Surface<WindowSurface>,
    gl_context: PossiblyCurrentContext,
    glyph_cache: GlyphCache,
    hint: bool,
    use_femtovg: bool,
    femtovg_fonts: Option<(FontId, FontId)>,
    sheet: Stylesheet,
    session: Session,
    doc: Document,
    modifiers: Modifiers,
    mouse_pos: (f32, f32),
    last_layout: Option<LayoutBox>,
    needs_redraw: bool,
    redraw_in_flight: bool,
    highlight_dirty: bool,
    ime_preedit: String,
    debug_boxes: bool,
    scrollbar_drag: bool,
    editor_drag: bool,
    highlight_cache: std::collections::HashMap<i64, Vec<Vec<(String, &'static str)>>>,
    last_input: Option<Instant>,
    editor_font_size: f32,
    cursor_ideal_x: Option<f32>,
    clipboard: Option<arboard::Clipboard>,
}

impl App {
    fn new(db_path: String, initial_files: Vec<String>) -> Self {
        App { state: None, db_path, initial_files }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }

        let window_attrs = Window::default_attributes()
            .with_title("vomvom")
            .with_inner_size(LogicalSize::new(1024u32, 768u32));

        let template = ConfigTemplateBuilder::new().with_alpha_size(8);
        let (window, gl_config) = DisplayBuilder::new()
            .with_window_attributes(Some(window_attrs))
            .build(event_loop, template, |configs| {
                configs
                    .reduce(|a, b| {
                        if b.num_samples() > a.num_samples() { b } else { a }
                    })
                    .unwrap()
            })
            .expect("failed to create window");

        let window = Arc::new(window.unwrap());
        window.set_ime_allowed(true);
        let raw_handle = window.window_handle().unwrap();

        let ctx_attrs = ContextAttributesBuilder::new().build(Some(raw_handle.as_raw()));
        let fallback = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(None))
            .build(Some(raw_handle.as_raw()));

        let gl_display = gl_config.display();
        let gl_context = unsafe {
            gl_display.create_context(&gl_config, &ctx_attrs)
                .or_else(|_| gl_display.create_context(&gl_config, &fallback))
                .expect("failed to create GL context")
        };

        let size = window.inner_size();
        let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw_handle.as_raw(),
            NonZeroU32::new(size.width.max(1)).unwrap(),
            NonZeroU32::new(size.height.max(1)).unwrap(),
        );
        let gl_surface = unsafe {
            gl_display.create_window_surface(&gl_config, &surface_attrs)
                .expect("failed to create GL surface")
        };

        let gl_context = gl_context.make_current(&gl_surface).unwrap();

        let renderer = unsafe {
            OpenGl::new_from_function_cstr(|s| gl_display.get_proc_address(s) as *const _)
                .expect("failed to create femtovg renderer")
        };

        let mut canvas = Canvas::new(renderer).expect("failed to create canvas");
        canvas.set_size(size.width, size.height, window.scale_factor() as f32);

        let editor_font_size = 12.0_f32;
        let sheet = build_stylesheet(editor_font_size);

        let mut session = Session::open(&self.db_path).expect("failed to open session db");
        for path in &self.initial_files {
            let idx = session.open_file(path).unwrap_or(0);
            session.set_active(idx);
        }

        let mut highlight_cache = std::collections::HashMap::new();
        rebuild_highlight_cache(&mut highlight_cache, &session);
        let doc = Document::new(build_initial_document(&session, &highlight_cache));

        self.state = Some(AppState {
            window,
            canvas,
            gl_surface,
            gl_context,
            glyph_cache: GlyphCache::new(),
            hint: true,
            use_femtovg: false,
            femtovg_fonts: None,
            sheet,
            session,
            doc,
            modifiers: Modifiers::default(),
            mouse_pos: (0.0, 0.0),
            last_layout: None,
            needs_redraw: true,
            redraw_in_flight: false,
            highlight_dirty: false,
            ime_preedit: String::new(),
            debug_boxes: false,
            scrollbar_drag: false,
            editor_drag: false,
            highlight_cache,
            last_input: None,
            editor_font_size,
            cursor_ideal_x: None,
            clipboard: arboard::Clipboard::new().ok(),
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.state else { return };

        match event {
            WindowEvent::CloseRequested => {
                let _ = state.session.sync_now();
                event_loop.exit();
            }
            WindowEvent::ModifiersChanged(mods) => {
                state.modifiers = mods;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                state.last_input = Some(Instant::now());
                if event.state != ElementState::Pressed { return; }
                use winit::keyboard::{Key, NamedKey};
                let ctrl = state.modifiers.state().control_key();
                let shift = state.modifiers.state().shift_key();
                let key = event.logical_key.clone();
                let mut dirty = true;
                let mut recognized = true;

                // First block: keys that need state borrow before getting buf.
                match &key {
                    Key::Character(s) if ctrl && (s == "o" || s == "O") => {
                        open_file_dialog(&mut state.session);
                        state.highlight_dirty = true;
                        dirty = false;
                    }
                    Key::Character(s) if ctrl && (s == "s" || s == "S") => {
                        let _ = state.session.save_active();
                        dirty = false;
                    }
                    Key::Character(s) if ctrl && (s == "f" || s == "F") => {
                        state.use_femtovg = !state.use_femtovg;
                        dirty = false;
                    }
                    Key::Character(s) if ctrl && (s == "h" || s == "H") => {
                        state.hint = !state.hint;
                        state.glyph_cache = GlyphCache::new();
                        dirty = false;
                    }
                    Key::Character(s) if ctrl && (s == "d" || s == "D") => {
                        state.debug_boxes = !state.debug_boxes;
                        dirty = false;
                    }
                    Key::Character(s) if ctrl && (s == "c" || s == "C") => {
                        let text = state.session.active().selected_text();
                        if !text.is_empty() {
                            if let Some(cb) = &mut state.clipboard { let _ = cb.set_text(text); }
                        }
                        dirty = false;
                    }
                    Key::Character(s) if ctrl && (s == "x" || s == "X") => {
                        let text = state.session.active().selected_text();
                        if !text.is_empty() {
                            if let Some(cb) = &mut state.clipboard { let _ = cb.set_text(text); }
                            state.session.active_mut().delete_selection();
                            state.highlight_dirty = true;
                        }
                        dirty = false;
                    }
                    Key::Character(s) if ctrl && (s == "v" || s == "V") => {
                        let text = state.clipboard.as_mut().and_then(|cb| cb.get_text().ok());
                        if let Some(text) = text {
                            state.session.active_mut().insert(&text);
                            state.highlight_dirty = true;
                        }
                        dirty = false;
                    }
                    Key::Named(NamedKey::Escape) => {
                        state.session.active_mut().clear_selection();
                        close_all_menus(&mut state.doc);
                        dirty = false;
                    }
                    _ => { recognized = false; }
                }

                if !recognized {
                    recognized = true;
                    let buf = state.session.active_mut();
                    match &key {
                        Key::Character(s) if ctrl && (s == "z" || s == "Z") => {
                            buf.clear_selection();
                            buf.undo();
                            state.highlight_dirty = true;
                        }
                        Key::Character(s) if ctrl && (s == "y" || s == "Y") => {
                            buf.clear_selection();
                            buf.redo();
                            state.highlight_dirty = true;
                        }
                        Key::Named(NamedKey::Backspace) if ctrl => { buf.backspace_word(); state.highlight_dirty = true; }
                        Key::Named(NamedKey::Backspace) => { buf.backspace(); state.highlight_dirty = true; }
                        Key::Named(NamedKey::Delete) if ctrl => { buf.delete_forward_word(); state.highlight_dirty = true; }
                        Key::Named(NamedKey::Delete) => { buf.delete_forward(); state.highlight_dirty = true; }
                        Key::Named(NamedKey::Enter) => { buf.insert("\n"); state.highlight_dirty = true; }
                        Key::Named(NamedKey::Space) => { buf.insert(" "); state.highlight_dirty = true; }
                        Key::Named(NamedKey::Tab) => { buf.insert("    "); state.highlight_dirty = true; }
                        Key::Named(NamedKey::ArrowLeft) if ctrl => {
                            if shift { buf.set_anchor_if_none(); } else { buf.clear_selection(); }
                            let pos = buf.word_start_left(buf.cursor);
                            buf.move_cursor(pos.line, pos.col);
                            state.cursor_ideal_x = None;
                            buf.break_undo_group();
                        }
                        Key::Named(NamedKey::ArrowLeft) => {
                            if shift {
                                buf.set_anchor_if_none();
                                let pos = buf.cursor;
                                let (l, c) = if pos.col > 0 { (pos.line, pos.col - 1) } else if pos.line > 0 { (pos.line - 1, buf.line(pos.line - 1).chars().count()) } else { (0, 0) };
                                buf.move_cursor(l, c);
                            } else if buf.selection_range().is_some() {
                                let (start, _) = buf.selection_range().unwrap();
                                buf.clear_selection();
                                buf.move_cursor(start.line, start.col);
                            } else {
                                let pos = buf.cursor;
                                let (l, c) = if pos.col > 0 { (pos.line, pos.col - 1) } else if pos.line > 0 { (pos.line - 1, buf.line(pos.line - 1).chars().count()) } else { (0, 0) };
                                buf.move_cursor(l, c);
                            }
                            state.cursor_ideal_x = None;
                            buf.break_undo_group();
                        }
                        Key::Named(NamedKey::ArrowRight) if ctrl => {
                            if shift { buf.set_anchor_if_none(); } else { buf.clear_selection(); }
                            let pos = buf.word_end_right(buf.cursor);
                            buf.move_cursor(pos.line, pos.col);
                            state.cursor_ideal_x = None;
                            buf.break_undo_group();
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            if shift {
                                buf.set_anchor_if_none();
                                let pos = buf.cursor;
                                let line_len = buf.line(pos.line).chars().count();
                                let (l, c) = if pos.col < line_len { (pos.line, pos.col + 1) } else if pos.line + 1 < buf.line_count() { (pos.line + 1, 0) } else { (pos.line, pos.col) };
                                buf.move_cursor(l, c);
                            } else if buf.selection_range().is_some() {
                                let (_, end) = buf.selection_range().unwrap();
                                buf.clear_selection();
                                buf.move_cursor(end.line, end.col);
                            } else {
                                let pos = buf.cursor;
                                let line_len = buf.line(pos.line).chars().count();
                                let (l, c) = if pos.col < line_len { (pos.line, pos.col + 1) } else if pos.line + 1 < buf.line_count() { (pos.line + 1, 0) } else { (pos.line, pos.col) };
                                buf.move_cursor(l, c);
                            }
                            state.cursor_ideal_x = None;
                            buf.break_undo_group();
                        }
                        Key::Named(NamedKey::ArrowUp) => {
                            if shift { buf.set_anchor_if_none(); } else { buf.clear_selection(); }
                            let pos = buf.cursor;
                            let scroll = buf.scroll_line;
                            let mono_font = state.femtovg_fonts.filter(|_| state.use_femtovg).map(|(_, m)| m);
                            let line_count = buf.line_count();
                            if state.cursor_ideal_x.is_none() {
                                if let Some(x) = compute_cursor_x(&mut state.canvas, &state.last_layout, &state.doc.root, pos.line, pos.col, scroll, mono_font, MONO_BYTES, state.editor_font_size) {
                                    state.cursor_ideal_x = Some(x);
                                }
                            }
                            let ideal_x = state.cursor_ideal_x.unwrap_or(0.0);
                            let (new_line, new_col) = move_cursor_vertical(&mut state.canvas, &state.last_layout, &state.doc.root, pos.line, pos.col, scroll, line_count, -1, ideal_x, mono_font, MONO_BYTES, state.editor_font_size);
                            buf.move_cursor(new_line, new_col);
                            buf.break_undo_group();
                        }
                        Key::Named(NamedKey::ArrowDown) => {
                            if shift { buf.set_anchor_if_none(); } else { buf.clear_selection(); }
                            let pos = buf.cursor;
                            let scroll = buf.scroll_line;
                            let mono_font = state.femtovg_fonts.filter(|_| state.use_femtovg).map(|(_, m)| m);
                            let line_count = buf.line_count();
                            if state.cursor_ideal_x.is_none() {
                                if let Some(x) = compute_cursor_x(&mut state.canvas, &state.last_layout, &state.doc.root, pos.line, pos.col, scroll, mono_font, MONO_BYTES, state.editor_font_size) {
                                    state.cursor_ideal_x = Some(x);
                                }
                            }
                            let ideal_x = state.cursor_ideal_x.unwrap_or(0.0);
                            let (new_line, new_col) = move_cursor_vertical(&mut state.canvas, &state.last_layout, &state.doc.root, pos.line, pos.col, scroll, line_count, 1, ideal_x, mono_font, MONO_BYTES, state.editor_font_size);
                            buf.move_cursor(new_line, new_col);
                            buf.break_undo_group();
                        }
                        Key::Named(NamedKey::Home) => {
                            if shift { buf.set_anchor_if_none(); } else { buf.clear_selection(); }
                            let line = buf.cursor.line;
                            buf.move_cursor(line, 0);
                            state.cursor_ideal_x = None;
                            buf.break_undo_group();
                        }
                        Key::Named(NamedKey::End) => {
                            if shift { buf.set_anchor_if_none(); } else { buf.clear_selection(); }
                            let line = buf.cursor.line;
                            let len = buf.line(line).chars().count();
                            buf.move_cursor(line, len);
                            state.cursor_ideal_x = None;
                            buf.break_undo_group();
                        }
                        Key::Character(s) if !ctrl => {
                            buf.insert(s);
                            state.highlight_dirty = true;
                            state.cursor_ideal_x = None;
                        }
                        _ => { dirty = false; recognized = false; }
                    }
                }

                if recognized {
                    state.needs_redraw = true;
                }
                if dirty {
                    let editor_h = editor_content_height(state.window.inner_size().height as f32);
                    scroll_to_cursor(&mut state.session, editor_h, state.editor_font_size);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                state.last_input = Some(Instant::now());
                state.mouse_pos = (position.x as f32, position.y as f32);
                let (mx, my) = state.mouse_pos;
                if state.scrollbar_drag {
                    let win_h = state.window.inner_size().height as f32;
                    if let Some(ref lb) = state.last_layout.clone() {
                        if let Some(track_lb) = scrollbar_track_lb(lb, mx, my)
                            .or_else(|| lb.children.get(2).and_then(|e| e.children.last()))
                        {
                            let track_lb = track_lb.clone();
                            let scroll = scrollbar_y_to_scroll(&track_lb, &state.session, my, win_h, state.editor_font_size);
                            state.session.active_mut().scroll_line = scroll;
                            state.needs_redraw = true;
                        }
                    }
                } else if state.editor_drag {
                    if let Some(ref lb) = state.last_layout.clone() {
                        let mono_font = state.femtovg_fonts.filter(|_| state.use_femtovg).map(|(_, m)| m);
                        if let Some((line, col)) = hit_test_editor(&mut state.canvas, lb, &state.doc.root, &state.session, mx, my, MONO_BYTES, mono_font, state.editor_font_size) {
                            state.session.active_mut().move_cursor(line, col);
                            state.cursor_ideal_x = None;
                            let win_h = state.window.inner_size().height as f32;
                            let editor_h = editor_content_height(win_h);
                            scroll_to_cursor(&mut state.session, editor_h, state.editor_font_size);
                            state.needs_redraw = true;
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: winit::event::MouseButton::Left, .. } => {
                state.last_input = Some(Instant::now());
                state.scrollbar_drag = false;
                state.editor_drag = false;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: winit::event::MouseButton::Left, .. } => {
                state.last_input = Some(Instant::now());
                let (mx, my) = state.mouse_pos;
                if let Some(ref lb) = state.last_layout.clone() {
                    if any_menu_open(&state.doc) {
                        eprintln!("[click] menu open, hit testing at ({}, {})", mx, my);
                        if let Some(action) = hit_test_menu_item(&state.doc.root, lb, mx, my) {
                            eprintln!("[click] hit action: {:?}", action);
                            close_all_menus(&mut state.doc);
                            execute_menu_action(&action, &mut state.session);
                            state.highlight_dirty = true;
                            state.needs_redraw = true;
                            return;
                        }
                        close_all_menus(&mut state.doc);
                        state.needs_redraw = true;
                        return;
                    }
                    if let Some(menu_id) = hit_test_menu_header(&state.doc.root, lb, mx, my) {
                        open_menu(&mut state.doc, &menu_id);
                        state.needs_redraw = true;
                        return;
                    }
                    if let Some(idx) = hit_test_tab(&state.doc.root, lb, mx, my) {
                        state.session.set_active(idx);
                        state.highlight_dirty = true;
                        state.needs_redraw = true;
                        return;
                    }
                    let win_h = state.window.inner_size().height as f32;
                    if let Some(track_lb) = scrollbar_track_lb(lb, mx, my) {
                        let track_lb = track_lb.clone();
                        let scroll = scrollbar_y_to_scroll(&track_lb, &state.session, my, win_h, state.editor_font_size);
                        state.session.active_mut().scroll_line = scroll;
                        state.scrollbar_drag = true;
                        state.needs_redraw = true;
                    } else if let Some((line, col)) = {
                        let mono_font = state.femtovg_fonts.filter(|_| state.use_femtovg).map(|(_, m)| m);
                        hit_test_editor(&mut state.canvas, lb, &state.doc.root, &state.session, mx, my, MONO_BYTES, mono_font, state.editor_font_size)
                    } {
                        let shift = state.modifiers.state().shift_key();
                        let buf = state.session.active_mut();
                        if shift {
                            buf.set_anchor_if_none();
                        } else {
                            buf.clear_selection();
                            // Set anchor at click point so drag extends selection from here.
                            buf.anchor = Some(session::buffer::Pos::new(line, col));
                        }
                        buf.move_cursor(line, col);
                        buf.break_undo_group();
                        state.cursor_ideal_x = None;
                        state.editor_drag = true;
                        let editor_h = editor_content_height(win_h);
                        scroll_to_cursor(&mut state.session, editor_h, state.editor_font_size);
                        state.needs_redraw = true;
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                state.last_input = Some(Instant::now());
                use winit::event::MouseScrollDelta;
                let ctrl = state.modifiers.state().control_key();
                if ctrl {
                    let steps = match delta {
                        MouseScrollDelta::LineDelta(_, y) => y as f32,
                        MouseScrollDelta::PixelDelta(pos) => pos.y as f32 / 20.0,
                    };
                    state.editor_font_size = (state.editor_font_size + steps * 0.5).clamp(4.0, 64.0);
                    state.sheet = build_stylesheet(state.editor_font_size);
                    state.glyph_cache = GlyphCache::new();
                } else {
                    let lines = match delta {
                        MouseScrollDelta::LineDelta(_, y) => -y as f32,
                        MouseScrollDelta::PixelDelta(pos) => -pos.y as f32 / 20.0,
                    };
                    let buf = state.session.active_mut();
                    let max_scroll = buf.line_count().saturating_sub(1);
                    buf.scroll_line = (buf.scroll_line as f32 + lines).round()
                        .clamp(0.0, max_scroll as f32) as usize;
                }
                state.needs_redraw = true;
            }
            WindowEvent::Ime(ime_event) => {
                state.last_input = Some(Instant::now());
                use winit::event::Ime;
                match ime_event {
                    Ime::Commit(text) => {
                        state.ime_preedit.clear();
                        state.session.active_mut().insert(&text);
                        state.highlight_dirty = true;
                        state.needs_redraw = true;
                    }
                    Ime::Preedit(text, _cursor) => {
                        state.ime_preedit = text;
                        state.needs_redraw = true;
                    }
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {
                state.redraw_in_flight = false;

                let _ = state.session.tick();
                let size = state.window.inner_size();
                let w = size.width as f32;
                let h = size.height as f32;
                let scale = state.window.scale_factor() as f32;

                if state.highlight_dirty {
                    state.highlight_dirty = false;
                    rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
                }

                if state.femtovg_fonts.is_none() {
                    let sans_id = state.canvas.add_font_mem(SANS_BYTES).expect("load sans");
                    let mono_id = state.canvas.add_font_mem(MONO_BYTES).expect("load mono");
                    state.femtovg_fonts = Some((sans_id, mono_id));
                }

                let lb = render_frame(
                    &mut state.canvas,
                    &mut state.glyph_cache,
                    &mut state.doc,
                    &state.session,
                    &state.highlight_cache,
                    &state.sheet,
                    state.editor_font_size,
                    w, h, scale,
                    state.femtovg_fonts,
                    state.hint,
                    state.use_femtovg,
                    state.last_layout.as_ref(),
                );
                state.last_layout = Some(lb.clone());

                if state.debug_boxes {
                    render::paint::paint_debug_boxes(&mut state.canvas, &lb);
                }

                let buf = state.session.active();
                let scroll = buf.scroll_line;
                let mono_font = state.femtovg_fonts.filter(|_| state.use_femtovg).map(|(_, m)| m);

                if let Some((sel_start, sel_end)) = buf.selection_range() {
                    paint_selection(&mut state.canvas, &lb, &state.doc.root, sel_start, sel_end, scroll, MONO_BYTES, mono_font, state.editor_font_size);
                }

                if buf.cursor.line >= scroll {
                    let line_text = buf.line(buf.cursor.line);
                    let layout_line = buf.cursor.line - scroll;
                    let cursors = vec![(layout_line, buf.cursor.col, line_text.as_str())];
                    paint_cursors_with_text(&mut state.canvas, &lb, &state.doc.root, &cursors, MONO_BYTES, mono_font, state.editor_font_size);
                }

                state.canvas.flush();
                state.gl_surface.swap_buffers(&state.gl_context).unwrap();

                // Re-queue if input arrived during this render, then clear the flag.
                if state.needs_redraw {
                    state.needs_redraw = false;
                    state.redraw_in_flight = true;
                    state.window.request_redraw();
                }
            }
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    state.gl_surface.resize(
                        &state.gl_context,
                        NonZeroU32::new(size.width).unwrap(),
                        NonZeroU32::new(size.height).unwrap(),
                    );
                    state.needs_redraw = true;
                }
            }
            _ => {}
        }

        // Queue a redraw only if one isn't already in flight. If a render is
        // already pending, the new state will be picked up when that render
        // completes and re-checks needs_redraw. This prevents a 1000Hz mouse
        // from continuously deferring RedrawRequested indefinitely.
        if let Some(state) = &mut self.state {
            if state.needs_redraw && !state.redraw_in_flight {
                state.redraw_in_flight = true;
                state.window.request_redraw();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let high_rate = self.state.as_ref()
            .and_then(|s| s.last_input)
            .map_or(false, |t| t.elapsed() < Duration::from_millis(100));
        let interval = if high_rate { Duration::from_millis(8) } else { Duration::from_millis(250) };
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + interval));
    }
}

fn build_stylesheet(editor_font_size: f32) -> Stylesheet {
    let patched = MAIN_CSS
        .replace("12px;/*EDITORFONT*/",
                 &format!("{}px;", editor_font_size));
    parse_stylesheet(&patched)
}

// Build the initial retained document. Called once; subsequent updates use
// the targeted mutation functions below.
fn build_initial_document(session: &Session, cache: &std::collections::HashMap<i64, Vec<Vec<(String, &'static str)>>>) -> Node {
    let menus = [("File", "file"), ("Edit", "edit")];
    let mut toolbar = Node::element("div").with_class("toolbar").with_id("toolbar");
    for (label, id) in &menus {
        let header = Node::element("div")
            .with_class("menu-header")
            .with_id(&format!("menu-{}", id))
            .with_attr("menu", *id)
            .with_child(Node::text(*label));
        toolbar = toolbar.with_child(header);
    }

    let mut tab_bar = Node::element("div").with_class("tab-bar").with_id("tab-bar");
    for (i, b) in session.buffers.iter().enumerate() {
        tab_bar = tab_bar.with_child(make_tab_node(b, i, i == session.active_idx));
    }

    let editor = Node::element("div").with_class("editor").with_id("editor");

    let statusbar = Node::element("div")
        .with_class("statusbar")
        .with_id("statusbar")
        .with_child(Node::element("span").with_id("status-left"))
        .with_child(Node::element("span").with_id("status-right"));

    let mut root = Node::element("root")
        .with_child(toolbar)
        .with_child(tab_bar)
        .with_child(editor)
        .with_child(statusbar);

    // Populate dynamic content.
    update_editor_node(&mut root, session, cache, usize::MAX, None);
    update_statusbar_node(&mut root, session);
    root
}

fn make_tab_node(b: &session::buffer::Buffer, idx: usize, active: bool) -> Node {
    let label = b.path.as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("[untitled]")
        .to_string();
    let dot = match buf_status_class(b) {
        Some(_) => " •",
        None    => "",
    };
    let mut tab = Node::element("div").with_class("tab")
        .with_attr("tab-idx", &idx.to_string())
        .with_child(Node::text(format!("{}{}", label, dot)));
    if active { tab = tab.with_class("active"); }
    if let Some(cls) = buf_status_class(b) { tab = tab.with_class(cls); }
    tab
}

fn buf_status_class(b: &session::buffer::Buffer) -> Option<&'static str> {
    use session::buffer::DiskStatus;
    match b.disk_status {
        DiskStatus::Deleted  => Some("status-deleted"),
        DiskStatus::Diverged => Some("status-diverged"),
        DiskStatus::Ok if b.is_modified => Some("status-modified"),
        _ => None,
    }
}

const SCROLLBAR_MIN_THUMB_PX: f32 = 32.0;

/// Returns (thumb_top_px, thumb_h_px) relative to track top, or None if no scrollbar needed.
/// thumb_h_px accounts for the same minimum as the CSS min-height so top is always correct.
fn scrollbar_thumb_geometry(total: usize, scroll: usize, visible: usize, track_h: f32) -> Option<(f32, f32)> {
    if total <= 1 { return None; }
    let virtual_total = (total - 1 + visible) as f32;
    let thumb_h = (visible as f32 / virtual_total * track_h).max(SCROLLBAR_MIN_THUMB_PX.min(track_h));
    let travel = track_h - thumb_h;
    let max_scroll = (total - 1) as f32;
    let top = if max_scroll > 0.0 { scroll as f32 / max_scroll * travel } else { 0.0 };
    Some((top, thumb_h))
}

pub fn update_scrollbar_styles(root: &mut Node, session: &Session, editor_h: f32, font_size: f32) {
    let buf = session.active();
    let total = buf.line_count();
    let visible = visible_lines(editor_h, font_size);

    let Some(thumb) = root.get_element_by_id("scrollbar-thumb") else { return };
    let Some((top_px, thumb_h_px)) = scrollbar_thumb_geometry(total, buf.scroll_line, visible, editor_h) else {
        thumb.style.display = render::style::Display::None;
        return;
    };
    thumb.style.display = render::style::Display::Block;
    thumb.style.top = render::style::Length::Percent(top_px / editor_h * 100.0);
    thumb.style.height = render::style::Length::Percent(thumb_h_px / editor_h * 100.0);
    if std::env::var("VOMVOM_DEBUG_SCROLLBAR").is_ok() {
        eprintln!("[scrollbar] total={total} scroll={} visible={visible} editor_h={editor_h:.1} top_px={top_px:.1} thumb_h_px={thumb_h_px:.1} bottom_px={:.1} top%={:.2} h%={:.2}",
            buf.scroll_line, top_px + thumb_h_px, top_px / editor_h * 100.0, thumb_h_px / editor_h * 100.0);
    }
}

pub fn update_line_numbers_styles(root: &mut Node, session: &Session, font_size: f32) {
    let line_count = session.active().line_count();
    let digits = line_count.to_string().len().max(1);
    let digit_str: String = std::iter::repeat('0').take(digits).collect();
    let text_w = render::glyph_cache::measure_text_width(MONO_BYTES, &digit_str, font_size);
    // Content width of sidebar = text width. CSS padding (16 left + 8 right) adds 24px.
    // Total border-box width = text_w + 24. Editor padding-left must match that total.
    let sidebar_content_w = text_w;
    let sidebar_total_w = sidebar_content_w + 24.0; // 16px pad-left + 8px pad-right

    if let Some(ln) = root.get_element_by_id("line-numbers") {
        ln.style.width = render::style::Length::Px(sidebar_content_w);
    }
    if let Some(editor) = root.get_element_by_id("editor") {
        editor.style.padding.left = render::style::Length::Px(sidebar_total_w);
    }
}

/// Height of the editor content area (inside padding) — equals the scrollbar track height.
pub fn editor_content_height(window_h: f32) -> f32 {
    // toolbar 32 + tab-bar 28 + statusbar 24 = 84px fixed chrome; editor padding 16px top+bottom
    (window_h - 84.0 - 32.0).max(0.0)
}

fn visible_lines(editor_content_h: f32, font_size: f32) -> usize {
    let line_h = font_size * 1.4;
    (editor_content_h / line_h).floor().max(1.0) as usize
}

// Scroll so the cursor line is visible, given the editor's pixel height.
pub fn scroll_to_cursor(session: &mut Session, editor_h: f32, font_size: f32) {
    let visible_lines = visible_lines(editor_h, font_size);
    let buf = session.active_mut();
    let cursor_line = buf.cursor.line;
    if cursor_line < buf.scroll_line {
        buf.scroll_line = cursor_line;
        buf.dirty = true;
    } else if cursor_line >= buf.scroll_line + visible_lines {
        buf.scroll_line = cursor_line + 1 - visible_lines;
        buf.dirty = true;
    }
}

// --- Targeted document mutation functions ---

pub fn rebuild_highlight_cache(cache: &mut std::collections::HashMap<i64, Vec<Vec<(String, &'static str)>>>, session: &Session) {
    let buf = session.active();
    let lang = highlight::lang_from_path(buf.path.as_deref());
    let lines: Vec<Vec<(String, &'static str)>> = (0..buf.line_count())
        .map(|i| {
            let text = buf.line(i);
            if text.is_empty() {
                vec![(" ".to_string(), "")]
            } else {
                highlight::tokenize_line(&text, lang)
                    .into_iter()
                    .map(|(s, c)| (s.to_string(), c))
                    .collect()
            }
        })
        .collect();
    cache.insert(buf.id, lines);
}

fn update_editor_node(root: &mut Node, session: &Session, cache: &std::collections::HashMap<i64, Vec<Vec<(String, &'static str)>>>, max_lines: usize, last_layout: Option<&LayoutBox>) {
    let buf = session.active();
    let cursor = buf.cursor;
    let scroll = buf.scroll_line;
    let empty_cache: Vec<Vec<(String, &'static str)>> = vec![];
    let lines = cache.get(&buf.id).unwrap_or(&empty_cache);
    let Some(editor) = root.get_element_by_id("editor") else { return };
    editor.clear_children();

    // Get previous layout's text line boxes to count visual rows per logical line.
    // editor child[0] = line-numbers (abs), child[1..n-1] = text lines, last = scrollbar (abs).
    let prev_editor_lb = last_layout.and_then(|lb| lb.children.get(2));

    let end_line = (scroll + max_lines).min(buf.line_count());
    let mut line_nums = Node::element("div").with_class("line-numbers").with_id("line-numbers");
    for (layout_idx, i) in (scroll..end_line).enumerate() {
        line_nums = line_nums.with_child(
            Node::element("div").with_class("line-number").with_child(Node::text((i + 1).to_string()))
        );
        // Add blank spans for any extra visual rows this line wraps into.
        let extra_rows = prev_editor_lb
            .and_then(|elb| elb.children.get(layout_idx + 1)) // +1 for line-numbers placeholder
            .map(|line_lb| {
                let span_count = lines.get(i).map_or(1, |v| v.len().max(1));
                visual_rows_for_line(line_lb, span_count).len().saturating_sub(1)
            })
            .unwrap_or(0);
        for _ in 0..extra_rows {
            line_nums = line_nums.with_child(
                Node::element("div").with_class("line-number").with_child(Node::text(" "))
            );
        }
    }
    editor.append_child(line_nums);

    for i in scroll..end_line {
        let mut line_node = Node::element("div").with_class("line");
        if i == cursor.line { line_node = line_node.with_class("cursor-line"); }
        let tokens = lines.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
        if tokens.is_empty() {
            line_node = line_node.with_child(Node::text(" "));
        } else {
            for (tok_text, tok_class) in tokens {
                let mut span = Node::element("span");
                if !tok_class.is_empty() { span = span.with_class(*tok_class); }
                span = span.with_child(Node::text(tok_text.clone()));
                line_node = line_node.with_child(span);
            }
        }
        editor.append_child(line_node);
    }
    let scrollbar = Node::element("div").with_class("scrollbar-track").with_id("scrollbar-track")
        .with_child(Node::element("div").with_class("scrollbar-thumb").with_id("scrollbar-thumb"));
    editor.append_child(scrollbar);
}

fn update_statusbar_node(root: &mut Node, session: &Session) {
    let buf = session.active();
    let cursor = buf.cursor;
    let path_label = buf.path.as_deref().unwrap_or("[untitled]");
    let status_right = format!("Ln {}, Col {}  UTF-8", cursor.line + 1, cursor.col + 1);
    if let Some(n) = root.get_element_by_id("status-left") {
        let cls = buf_status_class(buf);
        let dot = if cls.is_some() { " •" } else { "" };
        n.set_text_content(format!("{}{}", path_label, dot));
        n.remove_class("status-modified");
        n.remove_class("status-diverged");
        n.remove_class("status-deleted");
        if let Some(c) = cls { n.add_class(c); }
    }
    if let Some(n) = root.get_element_by_id("status-right") { n.set_text_content(status_right); }
}

fn update_tab_bar_node(root: &mut Node, session: &Session) {
    let Some(tab_bar) = root.get_element_by_id("tab-bar") else { return };
    tab_bar.clear_children();
    for (i, b) in session.buffers.iter().enumerate() {
        tab_bar.append_child(make_tab_node(b, i, i == session.active_idx));
    }
}

// Public wrappers used by event handlers.
fn update_editor(doc: &mut Document, session: &Session, cache: &std::collections::HashMap<i64, Vec<Vec<(String, &'static str)>>>, max_lines: usize) {
    update_editor_node(&mut doc.root, session, cache, max_lines, None);
    doc.mark_dirty();
}

fn update_statusbar(doc: &mut Document, session: &Session) {
    update_statusbar_node(&mut doc.root, session);
    doc.mark_dirty();
}

fn sync_doc_to_session(doc: &mut Document, session: &Session, cache: &std::collections::HashMap<i64, Vec<Vec<(String, &'static str)>>>, max_lines: usize, last_layout: Option<&LayoutBox>) {
    update_tab_bar_node(&mut doc.root, session);
    update_editor_node(&mut doc.root, session, cache, max_lines, last_layout);
    update_statusbar_node(&mut doc.root, session);
    doc.mark_dirty();
}

// --- Menu open/close (mutates the retained document) ---

pub fn any_menu_open(doc: &Document) -> bool {
    for id in ["menu-file", "menu-edit"] {
        if let Some(n) = doc.root.get_element_by_id_ref(id) {
            if n.has_class("open") { return true; }
        }
    }
    false
}

pub fn open_menu(doc: &mut Document, menu_id: &str) {
    // Close any other open menus first.
    close_all_menus(doc);
    let node_id = format!("menu-{}", menu_id);
    if let Some(header) = doc.get_element_by_id(&node_id) {
        header.add_class("open");
        let dropdown = build_dropdown(menu_id);
        header.append_child(dropdown);
    }
    doc.mark_dirty();
}

pub fn close_all_menus(doc: &mut Document) {
    for id in ["menu-file", "menu-edit"] {
        if let Some(header) = doc.get_element_by_id(id) {
            if header.has_class("open") {
                header.remove_class("open");
                // Remove all children except the first (the label text node).
                if let render::tree::NodeContent::Element { children, .. } = &mut header.content {
                    children.truncate(1);
                }
            }
        }
    }
    doc.mark_dirty();
}

/// Shared render logic used by both the live window loop and the screenshot path.
/// Syncs the document from session state, runs layout, paints, and returns the layout tree.
/// Does NOT flush/swap — callers do that.
pub fn render_frame(
    canvas: &mut Canvas<OpenGl>,
    glyph_cache: &mut GlyphCache,
    doc: &mut Document,
    session: &Session,
    highlight_cache: &std::collections::HashMap<i64, Vec<Vec<(String, &'static str)>>>,
    sheet: &Stylesheet,
    font_size: f32,
    w: f32,
    h: f32,
    scale: f32,
    femtovg_fonts: Option<(FontId, FontId)>,
    hint: bool,
    use_femtovg: bool,
    last_layout: Option<&LayoutBox>,
) -> LayoutBox {
    let editor_h = editor_content_height(h);
    sync_doc_to_session(doc, session, highlight_cache, visible_lines(editor_h, font_size) + 1, last_layout);

    canvas.set_size(w as u32, h as u32, scale);
    canvas.clear_rect(0, 0, w as u32, h as u32, femtovg::Color::rgbf(0.15, 0.15, 0.18));

    apply_styles(&mut doc.root, sheet, &[], None);
    update_scrollbar_styles(&mut doc.root, session, editor_h, font_size);
    update_line_numbers_styles(&mut doc.root, session, font_size);

    let mut measurer = render::femtovg_measurer::SwashMeasurer {
        sans_data: SANS_BYTES,
        mono_data: MONO_BYTES,
    };
    let mut lb = layout(&doc.root, Constraints::new(w, h), &mut measurer);
    finalize_positions(&mut lb);

    let mut ctx = PaintContext {
        canvas,
        glyph_cache,
        sans_data: SANS_BYTES,
        mono_data: MONO_BYTES,
        hint,
        use_femtovg,
        femtovg_fonts,
    };
    paint_tree_root(&mut ctx, &doc.root, &lb);

    lb
}

pub fn build_demo_scene() -> (Document, Stylesheet, Session) {
    use session::buffer::DiskStatus;
    let font_size = 11.5_f32;
    let sheet = build_stylesheet(font_size);
    // Demo session: no SQLite connection, purely in-memory buffers.
    let mut session = Session::new_demo();
    session.active_mut().path = Some("demo.rs".into());
    session.active_mut().insert("// vomvom — custom rendering engine\n\nmod render;\n\nfn main() {\n    // build scene, run event loop\n    println!(\"hello world\");\n}");
    session.active_mut().disk_status = DiskStatus::Diverged;

    // Extra tabs for visual testing — push Buffer structs directly, no DB.
    let mut buf2 = session::buffer::Buffer::new(1, Some("/tmp/modified.rs".into()), "hello");
    buf2.is_modified = true;
    session.buffers.push(buf2);

    let mut buf3 = session::buffer::Buffer::new(2, Some("/tmp/deleted.rs".into()), "");
    buf3.disk_status = DiskStatus::Deleted;
    session.buffers.push(buf3);

    session.buffers.push(session::buffer::Buffer::new(3, None, ""));

    let mut cache = std::collections::HashMap::new();
    rebuild_highlight_cache(&mut cache, &session);
    let doc = Document::new(build_initial_document(&session, &cache));
    (doc, sheet, session)
}

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    args.remove(0); // strip binary name

    if let Some(pos) = args.iter().position(|a| a == "--screenshot") {
        args.remove(pos);
        let path = args.get(0).map(|s| s.as_str()).unwrap_or("screenshots/screenshot.png");
        std::fs::create_dir_all("screenshots").unwrap();
        screenshot::save_screenshot(std::path::Path::new(path), 1024, 768);
        return;
    }

    if let Some(pos) = args.iter().position(|a| a == "--replay") {
        args.remove(pos);
        let script = args.get(0).map(|s| s.as_str()).unwrap_or("close-tab");
        run_replay_script(script);
        return;
    }

    let db_path = std::env::var("VOMVOM_DB")
        .unwrap_or_else(|_| {
            let mut p = dirs_or_local();
            p.push("vomvom_session.db");
            p.to_string_lossy().into_owned()
        });

    let event_loop = EventLoop::new().unwrap();
    let mut app = App::new(db_path, args);
    event_loop.run_app(&mut app).unwrap();
}

fn build_dropdown(menu_id: &str) -> Node {
    let src = match menu_id {
        "file" => DROPDOWN_FILE,
        "edit" => DROPDOWN_EDIT,
        _ => return Node::element("div").with_class("dropdown"),
    };
    parse_html(src).into_iter().next().unwrap_or_else(|| Node::element("div").with_class("dropdown"))
}

pub fn execute_menu_action(action: &str, session: &mut Session) {
    eprintln!("[menu] execute_menu_action: {:?}", action);
    match action {
        "open" => open_file_dialog(session),
        "save" => { let _ = session.save_active(); }
        "undo" => { session.active_mut().undo(); }
        "redo" => { session.active_mut().redo(); }
        "close-tab" => {
            eprintln!("[menu] close-tab: buffers before = {}, active_idx = {}", session.buffers.len(), session.active_idx);
            match session.close_active() {
                Ok(()) => {}
                Err(e) => eprintln!("[menu] close-tab ERROR: {:?}", e),
            }
            eprintln!("[menu] close-tab: buffers after = {}, active_idx = {}", session.buffers.len(), session.active_idx);
        }
        _ => { eprintln!("[menu] unhandled action: {:?}", action); }
    }
}

// Walk toolbar menu headers by hit-testing their layout boxes, then reading
// the "menu" attribute from the corresponding node to identify which menu.
pub fn hit_test_menu_header(root: &Node, lb: &LayoutBox, mx: f32, my: f32) -> Option<String> {
    use render::tree::NodeContent;
    let toolbar_lb = lb.children.first()?;
    let toolbar_node = root.children().first()?;
    for (i, child_lb) in toolbar_lb.children.iter().enumerate() {
        if child_lb.border_box.contains(mx, my) {
            let child_node = &toolbar_node.children()[i];
            if let NodeContent::Element { attrs, .. } = &child_node.content {
                if let Some(menu) = attrs.get("menu") {
                    return Some(menu.clone());
                }
            }
        }
    }
    None
}

// Walk the open dropdown's items, returning the "action" attr of the hit item.
pub fn hit_test_menu_item(root: &Node, lb: &LayoutBox, mx: f32, my: f32) -> Option<String> {
    use render::tree::NodeContent;
    let toolbar_lb = lb.children.first()?;
    let toolbar_node = root.children().first()?;
    for (i, header_lb) in toolbar_lb.children.iter().enumerate() {
        let header_node = &toolbar_node.children()[i];
        let dropdown_node = header_node.children().last()?;
        if !dropdown_node.has_class("dropdown") { continue; }
        let dropdown_lb = header_lb.children.last()?;
        for (j, item_lb) in dropdown_lb.children.iter().enumerate() {
            eprintln!("[hit_test_menu_item] item {} bbox={:?} vs ({},{})", j, item_lb.border_box, mx, my);
            if item_lb.border_box.contains(mx, my) {
                let item_node = &dropdown_node.children()[j];
                if let NodeContent::Element { attrs, .. } = &item_node.content {
                    if let Some(action) = attrs.get("action") {
                        return Some(action.clone());
                    }
                }
            }
        }
    }
    None
}

fn open_file_dialog(session: &mut Session) {
    if let Some(path) = rfd::FileDialog::new().pick_file() {
        if let Some(path_str) = path.to_str() {
            let idx = session.open_file(path_str).unwrap_or(0);
            session.set_active(idx);
        }
    }
}

pub fn hit_test_tab(root: &Node, lb: &LayoutBox, mx: f32, my: f32) -> Option<usize> {
    use render::tree::NodeContent;
    let tab_bar_lb = lb.children.get(1)?;
    let tab_bar_node = root.children().get(1)?;
    for (i, tab_lb) in tab_bar_lb.children.iter().enumerate() {
        let r = tab_lb.border_box;
        if mx >= r.x && mx <= r.x + r.w && my >= r.y && my <= r.y + r.h {
            let tab_node = &tab_bar_node.children()[i];
            if let NodeContent::Element { attrs, .. } = &tab_node.content {
                if let Some(idx_str) = attrs.get("tab-idx") {
                    return idx_str.parse().ok();
                }
            }
        }
    }
    None
}


fn text_prefix_width(
    canvas: &mut Canvas<OpenGl>,
    mono_font: Option<FontId>,
    mono_data: &'static [u8],
    prefix: &str,
    font_size: f32,
) -> f32 {
    if let Some(font_id) = mono_font {
        let mut paint = Paint::color(femtovg::Color::black());
        paint.set_font(&[font_id]);
        paint.set_font_size(font_size);
        canvas.measure_text(0.0, 0.0, prefix, &paint)
            .map(|m| m.width())
            .unwrap_or(0.0)
    } else {
        render::glyph_cache::measure_text_width(mono_data, prefix, font_size)
    }
}

/// Return the pixel x of the cursor on its current visual row, or None if layout unavailable.
fn compute_cursor_x(
    canvas: &mut Canvas<OpenGl>,
    last_layout: &Option<LayoutBox>,
    doc_root: &Node,
    logical_line: usize,
    col: usize,
    scroll: usize,
    mono_font: Option<FontId>,
    mono_data: &'static [u8],
    font_size: f32,
) -> Option<f32> {
    if logical_line < scroll { return None; }
    let layout_idx = logical_line - scroll;
    let editor_lb = last_layout.as_ref()?.children.get(2)?;
    let editor_node = doc_root.children().get(2)?;
    let line_lb = editor_lb.children.get(layout_idx + 1)?;
    let line_node = editor_node.children().get(layout_idx + 1)?;
    let (_, x) = cursor_visual_row_and_x(canvas, line_lb, line_node, col, mono_font, mono_data, font_size);
    Some(x)
}

/// Move cursor one visual row up (dir=-1) or down (dir=1), returning new (logical_line, col).
/// Wraps across logical lines, landing at the closest column to ideal_x.
fn move_cursor_vertical(
    canvas: &mut Canvas<OpenGl>,
    last_layout: &Option<LayoutBox>,
    doc_root: &Node,
    logical_line: usize,
    col: usize,
    scroll: usize,
    line_count: usize,
    dir: i32, // -1 or 1
    ideal_x: f32,
    mono_font: Option<FontId>,
    mono_data: &'static [u8],
    font_size: f32,
) -> (usize, usize) {
    // Try within the same logical line.
    if logical_line >= scroll {
        let layout_idx = logical_line - scroll;
        if let (Some(editor_lb), Some(editor_node)) = (
            last_layout.as_ref().and_then(|lb| lb.children.get(2)),
            doc_root.children().get(2),
        ) {
            if let (Some(line_lb), Some(line_node)) = (editor_lb.children.get(layout_idx + 1), editor_node.children().get(layout_idx + 1)) {
                let span_count = line_node.children().len();
                let rows = visual_rows_for_line(line_lb, span_count);
                let (cur_vrow, _) = cursor_visual_row_and_x(canvas, line_lb, line_node, col, mono_font, mono_data, font_size);
                let target_vrow = cur_vrow as i32 + dir;
                if target_vrow >= 0 && (target_vrow as usize) < rows.len() {
                    let row = &rows[target_vrow as usize];
                    let base = char_base_for_row(line_node, row);
                    let new_col = col_at_x_on_row(canvas, line_lb, line_node, row, base, ideal_x, mono_font, mono_data, font_size);
                    return (logical_line, new_col);
                }
            }
        }
    }

    // Cross to adjacent logical line.
    let target_line = logical_line as i32 + dir;
    if target_line < 0 || target_line as usize >= line_count {
        return (logical_line, col);
    }
    let target_logical = target_line as usize;
    if target_logical < scroll {
        // Target line not in layout; just preserve col.
        return (target_logical, col);
    }
    let target_layout_idx = target_logical - scroll;
    if let (Some(editor_lb), Some(editor_node)) = (
        last_layout.as_ref().and_then(|lb| lb.children.get(2)),
        doc_root.children().get(2),
    ) {
        if let (Some(line_lb), Some(line_node)) = (editor_lb.children.get(target_layout_idx + 1), editor_node.children().get(target_layout_idx + 1)) {
            let span_count = line_node.children().len();
            let rows = visual_rows_for_line(line_lb, span_count);
            // Going up → last visual row; going down → first visual row.
            let row = if dir < 0 {
                rows.last().map(|r| r.as_slice()).unwrap_or(&[])
            } else {
                rows.first().map(|r| r.as_slice()).unwrap_or(&[])
            };
            let base = char_base_for_row(line_node, row);
            let new_col = col_at_x_on_row(canvas, line_lb, line_node, row, base, ideal_x, mono_font, mono_data, font_size);
            return (target_logical, new_col);
        }
    }
    (target_logical, col)
}

/// Returns visual rows for a line as groups of span indices, sorted top-to-bottom.
/// Each group contains the span indices that share the same border_box.y (within 1px).
fn visual_rows_for_line(line_lb: &LayoutBox, span_count: usize) -> Vec<Vec<usize>> {
    let mut rows: Vec<(f32, Vec<usize>)> = Vec::new();
    for si in 0..span_count {
        let Some(span_lb) = line_lb.children.get(si) else { continue };
        let y = span_lb.border_box.y;
        if let Some(row) = rows.iter_mut().find(|(ry, _)| (ry - y).abs() < 1.0) {
            row.1.push(si);
        } else {
            rows.push((y, vec![si]));
        }
    }
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    rows.into_iter().map(|(_, v)| v).collect()
}

/// Given a line node + lb + cursor col, return (visual_row_idx, x_pixel) for the cursor.
fn cursor_visual_row_and_x(
    canvas: &mut Canvas<OpenGl>,
    line_lb: &LayoutBox,
    line_node: &Node,
    col: usize,
    mono_font: Option<FontId>,
    mono_data: &'static [u8],
    font_size: f32,
) -> (usize, f32) {
    let span_count = line_node.children().len();
    let rows = visual_rows_for_line(line_lb, span_count);

    let mut char_offset = 0usize;
    for si in 0..span_count {
        let span_text = span_text_of(line_node, si);
        let span_chars = span_text.chars().count();
        if col <= char_offset + span_chars || si + 1 == span_count {
            // Find which visual row this span is in.
            let vrow = rows.iter().position(|r| r.contains(&si)).unwrap_or(0);
            let intra = col.saturating_sub(char_offset).min(span_chars);
            let prefix: String = span_text.chars().take(intra).collect();
            let x_off = text_prefix_width(canvas, mono_font, mono_data, &prefix, font_size);
            let span_x = line_lb.children.get(si).map_or(0.0, |s| s.border_box.x);
            return (vrow, span_x + x_off);
        }
        char_offset += span_chars;
    }
    (0, line_lb.content.x)
}

/// Return col closest to target_x on a given visual row of a line.
fn col_at_x_on_row(
    canvas: &mut Canvas<OpenGl>,
    line_lb: &LayoutBox,
    line_node: &Node,
    row_spans: &[usize],
    char_base_of_row: usize,
    target_x: f32,
    mono_font: Option<FontId>,
    mono_data: &'static [u8],
    font_size: f32,
) -> usize {
    let mut best_col = char_base_of_row;
    let mut best_dist = f32::INFINITY;
    let mut char_offset = char_base_of_row;

    for &si in row_spans {
        let span_text = span_text_of(line_node, si);
        let chars: Vec<char> = span_text.chars().collect();
        let span_x = line_lb.children.get(si).map_or(0.0, |s| s.border_box.x);
        for intra in 0..=chars.len() {
            let prefix: String = chars[..intra].iter().collect();
            let x = span_x + text_prefix_width(canvas, mono_font, mono_data, &prefix, font_size);
            let dist = (x - target_x).abs();
            if dist < best_dist {
                best_dist = dist;
                best_col = char_offset + intra;
            }
        }
        char_offset += chars.len();
    }
    best_col
}

/// Char offset of the first character on a given visual row.
fn char_base_for_row(line_node: &Node, row_spans: &[usize]) -> usize {
    let first_si = match row_spans.first() { Some(&s) => s, None => return 0 };
    let mut offset = 0usize;
    for si in 0..first_si {
        offset += span_text_of(line_node, si).chars().count();
    }
    offset
}

fn span_text_of<'a>(line_node: &'a Node, si: usize) -> &'a str {
    line_node.children().get(si)
        .and_then(|span| span.children().first())
        .and_then(|n| if let render::tree::NodeContent::Text(t) = &n.content { Some(t.as_str()) } else { None })
        .unwrap_or("")
}

/// Paint selection highlight rectangles for a single (anchor, cursor) selection.
/// sel_start and sel_end must be in document order (start <= end).
pub fn paint_selection(
    canvas: &mut Canvas<OpenGl>,
    lb: &LayoutBox,
    doc_root: &Node,
    sel_start: session::buffer::Pos,
    sel_end: session::buffer::Pos,
    scroll: usize,
    mono_data: &'static [u8],
    mono_font: Option<FontId>,
    font_size: f32,
) {
    let Some(editor_lb) = lb.children.get(2) else { return };
    let Some(editor_node) = doc_root.children().get(2) else { return };
    let sel_color = femtovg::Color::rgbaf(0.3, 0.5, 0.9, 0.35);
    let line_h = font_size * 1.4;

    // total text-line children: editor children[1..last-1]
    let total_children = editor_lb.children.len();
    if total_children < 2 { return; }

    let first_visible_logical = scroll;
    let last_visible_logical = scroll + (total_children - 2); // exclusive

    let paint_start_logical = sel_start.line.max(first_visible_logical);
    let paint_end_logical = sel_end.line.min(last_visible_logical.saturating_sub(1));
    if paint_start_logical > paint_end_logical { return; }

    for logical_line in paint_start_logical..=paint_end_logical {
        let layout_idx = logical_line - scroll;
        let Some(line_lb) = editor_lb.children.get(layout_idx + 1) else { continue };
        let Some(line_node) = editor_node.children().get(layout_idx + 1) else { continue };

        let span_count = line_node.children().len();
        let rows = visual_rows_for_line(line_lb, span_count);
        if rows.is_empty() { continue; }

        // Determine which char range is selected on this logical line.
        let line_char_len: usize = line_node.children().iter()
            .map(|s| span_text_of_node(s))
            .map(|t| t.chars().count())
            .sum();

        let col_start = if logical_line == sel_start.line { sel_start.col } else { 0 };
        let col_end = if logical_line == sel_end.line { sel_end.col } else { line_char_len };

        // Paint row by row within this logical line.
        for row in &rows {
            let row_base = char_base_for_row(line_node, row);
            // Char length of this visual row.
            let row_char_len: usize = row.iter().map(|&si| span_text_of(line_node, si).chars().count()).sum();
            let row_end = row_base + row_char_len;

            // Skip rows entirely outside the selection.
            if col_end <= row_base || col_start >= row_end && logical_line == sel_end.line { continue; }
            if col_start > row_end { continue; }

            // x_left: pixel x of selection start on this row.
            let row_sel_start = col_start.max(row_base);
            let row_sel_end = col_end.min(row_end);

            let x_left = x_for_col_on_row(canvas, line_lb, line_node, row, row_base, row_sel_start, mono_font, mono_data, font_size);
            let x_right = if logical_line == sel_end.line && col_end <= row_end {
                x_for_col_on_row(canvas, line_lb, line_node, row, row_base, row_sel_end, mono_font, mono_data, font_size)
            } else {
                // Selection continues past this row: extend to edge of editor content area.
                editor_lb.content.x + editor_lb.content.w
            };

            if x_right <= x_left { continue; }
            let row_y = row.first().and_then(|&si| line_lb.children.get(si)).map_or(line_lb.border_box.y, |s| s.border_box.y);
            let mut path = Path::new();
            path.rect(x_left, row_y, x_right - x_left, line_h);
            canvas.fill_path(&path, &Paint::color(sel_color));
        }
    }
}

fn span_text_of_node(span: &Node) -> &str {
    span.children().first()
        .and_then(|n| if let render::tree::NodeContent::Text(t) = &n.content { Some(t.as_str()) } else { None })
        .unwrap_or("")
}

/// Pixel x for a given char col on a specific visual row.
fn x_for_col_on_row(
    canvas: &mut Canvas<OpenGl>,
    line_lb: &LayoutBox,
    line_node: &Node,
    row_spans: &[usize],
    row_base: usize,
    col: usize,
    mono_font: Option<FontId>,
    mono_data: &'static [u8],
    font_size: f32,
) -> f32 {
    let mut char_offset = row_base;
    for &si in row_spans {
        let span_text = span_text_of(line_node, si);
        let chars: Vec<char> = span_text.chars().collect();
        let span_x = line_lb.children.get(si).map_or(0.0, |s| s.border_box.x);
        if col <= char_offset + chars.len() {
            let intra = col.saturating_sub(char_offset).min(chars.len());
            let prefix: String = chars[..intra].iter().collect();
            return span_x + text_prefix_width(canvas, mono_font, mono_data, &prefix, font_size);
        }
        char_offset += chars.len();
    }
    // Past all spans: return right edge of last span
    row_spans.last().and_then(|&si| line_lb.children.get(si))
        .map_or(0.0, |s| s.border_box.x + s.border_box.w)
}

/// Paint cursor bars at the given (line, col, text) positions over the editor area.
pub fn paint_cursors_with_text(
    canvas: &mut Canvas<OpenGl>,
    lb: &LayoutBox,
    doc_root: &Node,
    cursors: &[(usize, usize, &str)], // (line_idx, col, line_text)
    mono_data: &'static [u8],
    mono_font: Option<FontId>,
    font_size: f32,
) {
    let Some(editor_lb) = lb.children.get(2) else { return };
    // editor node is child index 2 of root
    let Some(editor_node) = doc_root.children().get(2) else { return };

    let cursor_color = femtovg::Color::rgbaf(0.9, 0.9, 1.0, 0.85);
    let line_h = font_size * 1.4;

    for &(line_idx, col, _line_text) in cursors {
        // child[0] is line-numbers sidebar; text lines start at child[1]
        let Some(line_lb) = editor_lb.children.get(line_idx + 1) else { continue };
        let Some(line_node) = editor_node.children().get(line_idx + 1) else { continue };

        // Walk spans to find which span contains col, and x offset within it.
        let mut char_offset = 0usize;
        let mut cursor_x = line_lb.content.x;
        let mut cursor_y = line_lb.border_box.y;

        let spans = line_node.children();
        for (si, span_node) in spans.iter().enumerate() {
            let span_text = span_node.children().first()
                .and_then(|n| if let render::tree::NodeContent::Text(t) = &n.content { Some(t.as_str()) } else { None })
                .unwrap_or("");
            let span_chars = span_text.chars().count();
            let Some(span_lb) = line_lb.children.get(si) else { break };

            if col <= char_offset + span_chars || si + 1 == spans.len() {
                // Cursor is inside this span (or we're past all spans).
                let intra = col.saturating_sub(char_offset).min(span_chars);
                let prefix: String = span_text.chars().take(intra).collect();
                let x_off = text_prefix_width(canvas, mono_font, mono_data, &prefix, font_size);
                cursor_x = span_lb.border_box.x + x_off;
                cursor_y = span_lb.border_box.y;
                break;
            }
            char_offset += span_chars;
        }

        let mut path = Path::new();
        path.rect(cursor_x, cursor_y, 2.0, line_h);
        let paint = Paint::color(cursor_color);
        canvas.fill_path(&path, &paint);
    }
}

/// Returns the track LayoutBox if mx,my is inside the scrollbar track.
fn scrollbar_track_lb<'a>(lb: &'a LayoutBox, mx: f32, my: f32) -> Option<&'a LayoutBox> {
    let editor_lb = lb.children.get(2)?;
    let track_lb = editor_lb.children.last()?;
    if track_lb.border_box.contains(mx, my) { Some(track_lb) } else { None }
}

/// Map a click at my inside the track to a scroll_line, centering the thumb on the click.
fn scrollbar_y_to_scroll(track_lb: &LayoutBox, session: &Session, my: f32, window_h: f32, font_size: f32) -> usize {
    let buf = session.active();
    let total = buf.line_count();
    let visible = visible_lines(editor_content_height(window_h), font_size);
    let track_h = track_lb.border_box.h;
    let max_scroll = total.saturating_sub(1) as f32;
    if max_scroll <= 0.0 { return 0; }
    let (_, thumb_h) = scrollbar_thumb_geometry(total, buf.scroll_line, visible, track_h)
        .unwrap_or((0.0, 0.0));
    let travel = track_h - thumb_h;
    if travel <= 0.0 { return 0; }
    // Center thumb on click point.
    let rel_y = (my - track_lb.border_box.y - thumb_h / 2.0).clamp(0.0, travel);
    (rel_y / travel * max_scroll).round() as usize
}

/// Hit-test a mouse click in the editor area; return (line, col).
pub fn hit_test_editor(
    canvas: &mut Canvas<OpenGl>,
    lb: &LayoutBox,
    doc_root: &Node,
    session: &Session,
    mx: f32,
    my: f32,
    mono_data: &'static [u8],
    mono_font: Option<FontId>,
    font_size: f32,
) -> Option<(usize, usize)> {
    let editor_lb = lb.children.get(2)?;
    if !editor_lb.border_box.contains(mx, my) { return None; }
    let editor_node = doc_root.children().get(2)?;

    let buf = session.active();
    let scroll = buf.scroll_line;
    let line_h = font_size * 1.4;

    // child[0] is line-numbers (abs), child[1..n-1] are text lines, last is scrollbar (abs).
    let total_children = editor_lb.children.len();
    if total_children < 2 { return Some((0, 0)); }
    // Text lines are children[1..total_children-1].
    let text_lines = &editor_lb.children[1..total_children - 1];
    if text_lines.is_empty() { return Some((0, 0)); }

    // Find the best visual row by scanning all spans across all text lines.
    // A span's visual row top is span_lb.border_box.y. We want the row whose
    // top is <= my and is closest (i.e. largest y <= my).
    let mut best_row_y = f32::NEG_INFINITY;
    for line_lb in text_lines {
        for span_lb in &line_lb.children {
            let sy = span_lb.border_box.y;
            if sy <= my && sy > best_row_y {
                best_row_y = sy;
            }
        }
    }
    // If nothing found above the click, use the topmost row.
    if best_row_y == f32::NEG_INFINITY {
        best_row_y = text_lines.iter()
            .flat_map(|l| l.children.iter())
            .map(|s| s.border_box.y)
            .fold(f32::INFINITY, f32::min);
    }

    // Among all spans on that visual row, find which span contains the click x.
    // Pick the first span whose right edge >= mx; fall back to the last span on the row.
    // best_line_idx is the index into text_lines (0-based among text lines).
    let mut best_line_idx = 0usize;
    let mut best_span_idx = 0usize;
    let mut found = false;

    'outer: for (li, line_lb) in text_lines.iter().enumerate() {
        let row_spans: Vec<(usize, &LayoutBox)> = line_lb.children.iter().enumerate()
            .filter(|(_, s)| (s.border_box.y - best_row_y).abs() <= line_h * 0.5)
            .collect();
        if row_spans.is_empty() { continue; }
        for &(si, span_lb) in &row_spans {
            if mx <= span_lb.border_box.x + span_lb.border_box.w {
                best_line_idx = li;
                best_span_idx = si;
                found = true;
                break 'outer;
            }
        }
        // Click is past all spans on this row — use the last one.
        if let Some(&(si, _)) = row_spans.last() {
            best_line_idx = li;
            best_span_idx = si;
            found = true;
        }
    }
    if !found { best_line_idx = 0; best_span_idx = 0; }

    let buf_line = best_line_idx + scroll;
    // editor_node child[0] is line-numbers, text lines start at child[1].
    let line_node = editor_node.children().get(best_line_idx + 1)?;

    // Accumulate char offset up to best_span_idx.
    let mut char_base = 0usize;
    for si in 0..best_span_idx {
        if let Some(span) = line_node.children().get(si) {
            if let Some(text_node) = span.children().first() {
                if let render::tree::NodeContent::Text(t) = &text_node.content {
                    char_base += t.chars().count();
                }
            }
        }
    }

    // Now find col within this span.
    let span_node = line_node.children().get(best_span_idx)?;
    let span_text = span_node.children().first()
        .and_then(|n| if let render::tree::NodeContent::Text(t) = &n.content { Some(t.as_str()) } else { None })
        .unwrap_or("");
    let span_lb = &editor_lb.children[best_line_idx + 1].children[best_span_idx];
    let local_x = mx - span_lb.border_box.x;

    let chars: Vec<char> = span_text.chars().collect();
    let mut best_col = char_base;
    let mut best_dist = f32::INFINITY;
    for intra in 0..=chars.len() {
        let prefix: String = chars[..intra].iter().collect();
        let x = text_prefix_width(canvas, mono_font, mono_data, &prefix, font_size);
        let dist = (x - local_x).abs();
        if dist < best_dist {
            best_dist = dist;
            best_col = char_base + intra;
        }
    }

    Some((buf_line, best_col))
}

fn run_replay_script(script: &str) {
    use replay::ScriptedEvent::*;
    match script {
        "close-tab" => {
            // Debug script: open File menu, click "Close Tab", verify tab count drops.
            replay::run_script("close_tab", 1024, 768, vec![
                // Initial state: show the demo scene.
                ScreenshotNamed("initial"),
                // Click the File menu header (approximately x=30, y=16 based on toolbar layout).
                ClickAt(30.0, 16.0),
                ScreenshotNamed("menu_open"),
                // Click "Close Tab" item — 4th item (Open/Save/separator/Close Tab).
                // From debug output: items at y≈26, 56, ~86(sep), ~110. Use y=115.
                ClickAt(30.0, 115.0),
                ScreenshotNamed("after_close"),
            ]);
        }
        "type-text" => {
            replay::run_script("type_text", 1024, 768, vec![
                ScreenshotNamed("initial"),
                Type("hello, world!\n"),
                ScreenshotNamed("after_type"),
                MoveCursor(0, 0),
                ScreenshotNamed("cursor_home"),
            ]);
        }
        "drag-select" => {
            replay::run_script("drag_select", 1024, 768, vec![
                ScreenshotNamed("initial"),
                // Drag from start of line 1 to end of line 3 in the demo content.
                DragFrom(50.0, 84.0, 300.0, 140.0),
                ScreenshotNamed("after_drag"),
            ]);
        }
        _ => {
            eprintln!("[replay] unknown script {:?}. Available: close-tab, type-text, drag-select", script);
        }
    }
}

fn dirs_or_local() -> std::path::PathBuf {
    // Try %APPDATA% on Windows, ~/.local/share on Linux, else cwd.
    if let Some(dir) = std::env::var_os("APPDATA") {
        let mut p = std::path::PathBuf::from(dir);
        p.push("vomvom");
        let _ = std::fs::create_dir_all(&p);
        return p;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut p = std::path::PathBuf::from(home);
        p.push(".local/share/vomvom");
        let _ = std::fs::create_dir_all(&p);
        return p;
    }
    std::path::PathBuf::from(".")
}

#[cfg(test)]
mod perf_tests {
    use super::*;
    use std::time::Instant;
    use render::layout::{layout, finalize_positions, Constraints};
    use render::tree::apply_styles;
    use render::css_parse::parse_stylesheet;
    use render::glyph_cache::measure_text_width;

    static MAIN_RS: &str = include_str!("main.rs");

    fn time_iters<F: FnMut()>(label: &str, iters: u32, mut f: F) {
        let t = Instant::now();
        for _ in 0..iters { f(); }
        let elapsed = t.elapsed();
        println!("{label}: {:.2}ms total over {iters} iters = {:.3}ms/iter",
            elapsed.as_secs_f64() * 1000.0,
            elapsed.as_secs_f64() * 1000.0 / iters as f64);
    }

    #[test]
    fn perf_measure_text_width_per_line() {
        let lines: Vec<&str> = MAIN_RS.lines().collect();
        let iters = 100u32;
        time_iters("measure_text_width uncached (all lines × 100)", iters, || {
            for line in &lines {
                measure_text_width(MONO_BYTES, line, 11.5);
            }
        });
        println!("  ({} lines)", lines.len());
    }

    #[test]
    fn perf_style_cascade_and_layout() {
        let sheet = parse_stylesheet(MAIN_CSS);
        let mut session = Session::open(":memory:").unwrap();
        // Load each line so the buffer has the right line structure.
        for line in MAIN_RS.lines() {
            session.active_mut().insert(line);
            session.active_mut().insert("\n");
        }
        let mut cache = std::collections::HashMap::new();
        rebuild_highlight_cache(&mut cache, &session);
        let mut doc = Document::new(build_initial_document(&session, &cache));
        let iters = 20u32;
        time_iters("apply_styles + layout (full frame × 20)", iters, || {
            apply_styles(&mut doc.root, &sheet, &[], None);
            let mut measurer = render::femtovg_measurer::SwashMeasurer {
                sans_data: SANS_BYTES,
                mono_data: MONO_BYTES,
            };
            let mut lb = layout(&doc.root, Constraints::new(1024.0, 768.0), &mut measurer);
            finalize_positions(&mut lb);
            let _ = lb;
        });
    }

    #[test]
    fn perf_measure_width_vs_line_count() {
        let lines: Vec<String> = MAIN_RS.lines().map(|s| s.to_string()).collect();
        let total_chars: usize = lines.iter().map(|l| l.chars().count()).sum();
        println!("\nmain.rs: {} lines, {} chars", lines.len(), total_chars);

        // Baseline: single measure pass.
        let t = Instant::now();
        for line in &lines {
            measure_text_width(MONO_BYTES, line, 11.5);
        }
        println!("Single measure pass (uncached): {:.2}ms", t.elapsed().as_secs_f64() * 1000.0);

        // Cost of FontRef::from_index per call.
        let iters = lines.len() as u32;
        let t = Instant::now();
        for _ in 0..iters {
            swash::FontRef::from_index(MONO_BYTES, 0);
        }
        println!("FontRef::from_index × {} (once per line): {:.2}ms", iters, t.elapsed().as_secs_f64() * 1000.0);

        // Cost of glyph_metrics setup per call (charmap + glyph_metrics scaled).
        let font_ref = swash::FontRef::from_index(MONO_BYTES, 0).unwrap();
        let t = Instant::now();
        for _ in 0..iters {
            let _cm = font_ref.charmap();
            let _gm = font_ref.glyph_metrics(&[]).scale(11.5);
        }
        println!("charmap + glyph_metrics().scale() × {} (once per line): {:.2}ms", iters, t.elapsed().as_secs_f64() * 1000.0);

        // Cost of just advance_width calls for all chars (with metrics already built).
        let charmap = font_ref.charmap();
        let gm = font_ref.glyph_metrics(&[]).scale(11.5);
        let t = Instant::now();
        for line in &lines {
            for ch in line.chars() {
                let _ = gm.advance_width(charmap.map(ch));
            }
        }
        println!("advance_width per char ({} chars, metrics pre-built): {:.2}ms", total_chars, t.elapsed().as_secs_f64() * 1000.0);

        // Cost of advance_width with per-glyph HashMap caching (pre-built metrics, cached advances).
        let mut adv_cache: std::collections::HashMap<u16, f32> = std::collections::HashMap::new();
        let t = Instant::now();
        for line in &lines {
            for ch in line.chars() {
                let gid = charmap.map(ch);
                adv_cache.entry(gid).or_insert_with(|| gm.advance_width(gid));
            }
        }
        let cached_total: f32 = lines.iter().flat_map(|l| l.chars()).map(|ch| adv_cache[&charmap.map(ch)]).sum();
        println!("advance_width cached via HashMap ({} chars): {:.2}ms, sum={:.1}", total_chars, t.elapsed().as_secs_f64() * 1000.0, cached_total);
    }
}
