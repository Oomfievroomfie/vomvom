mod highlight;
mod render;
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
static MONO_BYTES: &[u8] = include_bytes!("../UbuntuSansMono-Medium.ttf");

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
    ime_preedit: String,
    debug_boxes: bool,
    scrollbar_drag: bool,
    highlight_cache: std::collections::HashMap<i64, Vec<Vec<(String, &'static str)>>>,
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

        let sheet = build_stylesheet();

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
            use_femtovg: true,
            femtovg_fonts: None,
            sheet,
            session,
            doc,
            modifiers: Modifiers::default(),
            mouse_pos: (0.0, 0.0),
            last_layout: None,
            needs_redraw: true,
            ime_preedit: String::new(),
            debug_boxes: false,
            scrollbar_drag: false,
            highlight_cache,
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
                if event.state != ElementState::Pressed { return; }
                use winit::keyboard::{Key, NamedKey};
                let ctrl = state.modifiers.state().control_key();
                let key = event.logical_key.clone();
                let mut dirty = true;

                match &key {
                    Key::Character(s) if ctrl && (s == "o" || s == "O") => {
                        open_file_dialog(&mut state.session);
                        rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
                        sync_doc_to_session(&mut state.doc, &state.session, &state.highlight_cache);
                        state.needs_redraw = true;
                        return;
                    }
                    Key::Character(s) if ctrl && (s == "s" || s == "S") => {
                        let _ = state.session.save_active();
                        update_statusbar(&mut state.doc, &state.session);
                        state.needs_redraw = true;
                        return;
                    }
                    Key::Character(s) if ctrl && (s == "f" || s == "F") => {
                        state.use_femtovg = !state.use_femtovg;
                        state.needs_redraw = true;
                        return;
                    }
                    Key::Character(s) if ctrl && (s == "h" || s == "H") => {
                        state.hint = !state.hint;
                        state.glyph_cache = GlyphCache::new();
                        state.needs_redraw = true;
                        return;
                    }
                    Key::Character(s) if ctrl && (s == "d" || s == "D") => {
                        state.debug_boxes = !state.debug_boxes;
                        state.needs_redraw = true;
                        return;
                    }
                    _ => {}
                }

                let buf = state.session.active_mut();
                match &key {
                    Key::Named(NamedKey::Escape) => {
                        close_all_menus(&mut state.doc);
                        state.needs_redraw = true;
                        return;
                    }
                    Key::Character(s) if ctrl && (s == "z" || s == "Z") => { buf.undo(); }
                    Key::Character(s) if ctrl && (s == "y" || s == "Y") => { buf.redo(); }
                    Key::Named(NamedKey::Backspace) => { buf.backspace(); }
                    Key::Named(NamedKey::Delete) => { buf.delete_forward(); }
                    Key::Named(NamedKey::Enter) => { buf.insert("\n"); }
                    Key::Named(NamedKey::Space) => { buf.insert(" "); }
                    Key::Named(NamedKey::Tab) => { buf.insert("    "); }
                    Key::Named(NamedKey::ArrowLeft) => {
                        let pos = buf.cursor;
                        let (l, c) = if pos.col > 0 { (pos.line, pos.col - 1) } else if pos.line > 0 { (pos.line - 1, buf.line(pos.line - 1).chars().count()) } else { (0, 0) };
                        buf.move_cursor(l, c);
                        buf.break_undo_group();
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        let pos = buf.cursor;
                        let line_len = buf.line(pos.line).chars().count();
                        let (l, c) = if pos.col < line_len { (pos.line, pos.col + 1) } else if pos.line + 1 < buf.line_count() { (pos.line + 1, 0) } else { (pos.line, pos.col) };
                        buf.move_cursor(l, c);
                        buf.break_undo_group();
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        let pos = buf.cursor;
                        if pos.line > 0 { buf.move_cursor(pos.line - 1, pos.col); }
                        buf.break_undo_group();
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        let pos = buf.cursor;
                        if pos.line + 1 < buf.line_count() { buf.move_cursor(pos.line + 1, pos.col); }
                        buf.break_undo_group();
                    }
                    Key::Named(NamedKey::Home) => {
                        let line = buf.cursor.line;
                        buf.move_cursor(line, 0);
                        buf.break_undo_group();
                    }
                    Key::Named(NamedKey::End) => {
                        let line = buf.cursor.line;
                        let len = buf.line(line).chars().count();
                        buf.move_cursor(line, len);
                        buf.break_undo_group();
                    }
                    Key::Character(s) if !ctrl => { buf.insert(s); }
                    _ => { dirty = false; }
                }
                if dirty {
                    let editor_h = editor_content_height(state.window.inner_size().height as f32);
                    scroll_to_cursor(&mut state.session, editor_h);
                    rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
                    update_editor(&mut state.doc, &state.session, &state.highlight_cache);
                    update_statusbar(&mut state.doc, &state.session);
                    state.needs_redraw = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                state.mouse_pos = (position.x as f32, position.y as f32);
                if state.scrollbar_drag {
                    let (mx, my) = state.mouse_pos;
                    let win_h = state.window.inner_size().height as f32;
                    if let Some(ref lb) = state.last_layout.clone() {
                        if let Some(track_lb) = scrollbar_track_lb(lb, mx, my)
                            .or_else(|| lb.children.get(2).and_then(|e| e.children.last()))
                        {
                            let track_lb = track_lb.clone();
                            let scroll = scrollbar_y_to_scroll(&track_lb, &state.session, my, win_h);
                            state.session.active_mut().scroll_line = scroll;
                            state.session.active_mut().dirty = true;
                            update_editor(&mut state.doc, &state.session, &state.highlight_cache);
                            state.needs_redraw = true;
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: winit::event::MouseButton::Left, .. } => {
                state.scrollbar_drag = false;
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: winit::event::MouseButton::Left, .. } => {
                let (mx, my) = state.mouse_pos;
                if let Some(ref lb) = state.last_layout.clone() {
                    // Menu item click (dropdown open)?
                    if any_menu_open(&state.doc) {
                        if let Some(action) = hit_test_menu_item(&state.doc.root, lb, mx, my) {
                            close_all_menus(&mut state.doc);
                            execute_menu_action(&action, &mut state.session);
                            rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
                            sync_doc_to_session(&mut state.doc, &state.session, &state.highlight_cache);
                            state.needs_redraw = true;
                            return;
                        }
                        close_all_menus(&mut state.doc);
                        state.needs_redraw = true;
                        return;
                    }
                    // Menu header click?
                    if let Some(menu_id) = hit_test_menu_header(&state.doc.root, lb, mx, my) {
                        open_menu(&mut state.doc, &menu_id);
                        state.needs_redraw = true;
                        return;
                    }
                    // Tab click?
                    if let Some(idx) = hit_test_tab(&state.doc.root, lb, mx, my) {
                        state.session.set_active(idx);
                        rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
                        sync_doc_to_session(&mut state.doc, &state.session, &state.highlight_cache);
                        state.needs_redraw = true;
                        return;
                    }
                    // Scrollbar track click — jump scroll position and start drag.
                    let win_h = state.window.inner_size().height as f32;
                    if let Some(track_lb) = scrollbar_track_lb(lb, mx, my) {
                        let track_lb = track_lb.clone();
                        let scroll = scrollbar_y_to_scroll(&track_lb, &state.session, my, win_h);
                        state.session.active_mut().scroll_line = scroll;
                        state.session.active_mut().dirty = true;
                        state.scrollbar_drag = true;
                        update_editor(&mut state.doc, &state.session, &state.highlight_cache);
                        state.needs_redraw = true;
                    // Editor click — place cursor.
                    } else if let Some((line, col)) = hit_test_editor(lb, &state.session, mx, my, MONO_BYTES) {
                        let buf = state.session.active_mut();
                        buf.move_cursor(line, col);
                        buf.break_undo_group();
                        scroll_to_cursor(&mut state.session, editor_content_height(win_h));
                        update_editor(&mut state.doc, &state.session, &state.highlight_cache);
                        update_statusbar(&mut state.doc, &state.session);
                        state.needs_redraw = true;
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                use winit::event::MouseScrollDelta;
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -y as f32,
                    MouseScrollDelta::PixelDelta(pos) => -pos.y as f32 / 20.0,
                };
                let buf = state.session.active_mut();
                let max_scroll = buf.line_count().saturating_sub(1);
                buf.scroll_line = (buf.scroll_line as f32 + lines).round()
                    .clamp(0.0, max_scroll as f32) as usize;
                buf.dirty = true;
                update_editor(&mut state.doc, &state.session, &state.highlight_cache);
                state.needs_redraw = true;
            }
            WindowEvent::Ime(ime_event) => {
                use winit::event::Ime;
                match ime_event {
                    Ime::Commit(text) => {
                        state.ime_preedit.clear();
                        state.session.active_mut().insert(&text);
                        rebuild_highlight_cache(&mut state.highlight_cache, &state.session);
                        update_editor(&mut state.doc, &state.session, &state.highlight_cache);
                        update_statusbar(&mut state.doc, &state.session);
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
                let _ = state.session.tick();
                let size = state.window.inner_size();
                let w = size.width as f32;
                let h = size.height as f32;
                let scale = state.window.scale_factor() as f32;

                state.canvas.set_size(size.width, size.height, scale);
                state.canvas.clear_rect(0, 0, size.width, size.height,
                    femtovg::Color::rgbf(0.15, 0.15, 0.18));

                apply_styles(&mut state.doc.root, &state.sheet, &[], None);
                let editor_h = editor_content_height(h);
                update_scrollbar_styles(&mut state.doc.root, &state.session, editor_h);
                let mut measurer = render::femtovg_measurer::SwashMeasurer {
                    sans_data: SANS_BYTES,
                    mono_data: MONO_BYTES,
                };
                let mut lb = layout(&state.doc.root, Constraints::new(w, h), &mut measurer);
                finalize_positions(&mut lb);
                state.last_layout = Some(lb.clone());

                if state.femtovg_fonts.is_none() {
                    let sans_id = state.canvas.add_font_mem(SANS_BYTES).expect("load sans");
                    let mono_id = state.canvas.add_font_mem(MONO_BYTES).expect("load mono");
                    state.femtovg_fonts = Some((sans_id, mono_id));
                }

                let mut ctx = PaintContext {
                    canvas: &mut state.canvas,
                    glyph_cache: &mut state.glyph_cache,
                    sans_data: SANS_BYTES,
                    mono_data: MONO_BYTES,
                    hint: state.hint,
                    use_femtovg: state.use_femtovg,
                    femtovg_fonts: state.femtovg_fonts,
                };
                paint_tree_root(&mut ctx, &state.doc.root, &lb);

                if state.debug_boxes {
                    render::paint::paint_debug_boxes(&mut state.canvas, &lb);
                }

                // Paint text cursors and scrollbar on top.
                let buf = state.session.active();
                let scroll = buf.scroll_line;
                if buf.cursor.line >= scroll {
                    let line_text = buf.line(buf.cursor.line);
                    let layout_line = buf.cursor.line - scroll;
                    let cursors = vec![(layout_line, buf.cursor.col, line_text.as_str())];
                    paint_cursors_with_text(&mut state.canvas, &lb, &cursors, MONO_BYTES);
                }

                state.canvas.flush();
                state.gl_surface.swap_buffers(&state.gl_context).unwrap();
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
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(state) = &mut self.state {
            if state.needs_redraw {
                state.needs_redraw = false;
                state.window.request_redraw();
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(250),
        ));
    }
}

fn build_stylesheet() -> Stylesheet {
    parse_stylesheet(MAIN_CSS)
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
    update_editor_node(&mut root, session, cache);
    update_statusbar_node(&mut root, session);
    root
}

fn make_tab_node(b: &session::buffer::Buffer, idx: usize, active: bool) -> Node {
    let label = b.path.as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("[untitled]")
        .to_string();
    let modified = if b.is_modified { " ●" } else { "" };
    let mut tab = Node::element("div").with_class("tab")
        .with_attr("tab-idx", &idx.to_string())
        .with_child(Node::text(format!("{}{}", label, modified)));
    if active { tab = tab.with_class("active"); }
    tab
}

const SCROLLBAR_MIN_THUMB_PX: f32 = 32.0;

/// Returns (thumb_top_px, thumb_h_px) relative to track top, or None if no scrollbar needed.
/// thumb_h_px accounts for the same minimum as the CSS min-height so top is always correct.
fn scrollbar_thumb_geometry(total: usize, scroll: usize, visible: usize, track_h: f32) -> Option<(f32, f32)> {
    if total <= visible { return None; }
    let thumb_h = (visible as f32 / total as f32 * track_h).max(SCROLLBAR_MIN_THUMB_PX.min(track_h));
    let travel = track_h - thumb_h;
    let max_scroll = (total - visible) as f32;
    let top = if max_scroll > 0.0 { scroll as f32 / max_scroll * travel } else { 0.0 };
    Some((top, thumb_h))
}

fn update_scrollbar_styles(root: &mut Node, session: &Session, editor_h: f32) {
    let buf = session.active();
    let total = buf.line_count();
    let visible = visible_lines(editor_h);

    let Some(thumb) = root.get_element_by_id("scrollbar-thumb") else { return };
    let Some((top_px, thumb_h_px)) = scrollbar_thumb_geometry(total, buf.scroll_line, visible, editor_h) else {
        thumb.style.display = render::style::Display::None;
        return;
    };
    thumb.style.display = render::style::Display::Block;
    thumb.style.top = render::style::Length::Percent(top_px / editor_h * 100.0);
    thumb.style.height = render::style::Length::Percent(thumb_h_px / editor_h * 100.0);
}

/// Height of the editor content area (inside padding) — equals the scrollbar track height.
fn editor_content_height(window_h: f32) -> f32 {
    // toolbar 32 + tab-bar 28 + statusbar 24 = 84px fixed chrome; editor padding 16px top+bottom
    (window_h - 84.0 - 32.0).max(0.0)
}

fn visible_lines(editor_content_h: f32) -> usize {
    let line_h = 11.5f32 * 1.4f32;
    (editor_content_h / line_h).floor().max(1.0) as usize
}

// Scroll so the cursor line is visible, given the editor's pixel height.
fn scroll_to_cursor(session: &mut Session, editor_h: f32) {
    let visible_lines = visible_lines(editor_h);
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

fn rebuild_highlight_cache(cache: &mut std::collections::HashMap<i64, Vec<Vec<(String, &'static str)>>>, session: &Session) {
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

fn update_editor_node(root: &mut Node, session: &Session, cache: &std::collections::HashMap<i64, Vec<Vec<(String, &'static str)>>>) {
    let buf = session.active();
    let cursor = buf.cursor;
    let scroll = buf.scroll_line;
    let empty_cache: Vec<Vec<(String, &'static str)>> = vec![];
    let lines = cache.get(&buf.id).unwrap_or(&empty_cache);
    let Some(editor) = root.get_element_by_id("editor") else { return };
    editor.clear_children();
    for i in scroll..buf.line_count() {
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
    let modified_marker = if buf.is_modified { " ●" } else { "" };
    let status_left = format!("{}{}", path_label, modified_marker);
    let status_right = format!("Ln {}, Col {}  UTF-8", cursor.line + 1, cursor.col + 1);
    if let Some(n) = root.get_element_by_id("status-left") { n.set_text_content(status_left); }
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
fn update_editor(doc: &mut Document, session: &Session, cache: &std::collections::HashMap<i64, Vec<Vec<(String, &'static str)>>>) {
    update_editor_node(&mut doc.root, session, cache);
    doc.mark_dirty();
}

fn update_statusbar(doc: &mut Document, session: &Session) {
    update_statusbar_node(&mut doc.root, session);
    doc.mark_dirty();
}

fn sync_doc_to_session(doc: &mut Document, session: &Session, cache: &std::collections::HashMap<i64, Vec<Vec<(String, &'static str)>>>) {
    update_tab_bar_node(&mut doc.root, session);
    update_editor_node(&mut doc.root, session, cache);
    update_statusbar_node(&mut doc.root, session);
    doc.mark_dirty();
}

// --- Menu open/close (mutates the retained document) ---

fn any_menu_open(doc: &Document) -> bool {
    for id in ["menu-file", "menu-edit"] {
        if let Some(n) = doc.root.get_element_by_id_ref(id) {
            if n.has_class("open") { return true; }
        }
    }
    false
}

fn open_menu(doc: &mut Document, menu_id: &str) {
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

fn close_all_menus(doc: &mut Document) {
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

pub fn build_demo_scene() -> (Node, Stylesheet) {
    let sheet = build_stylesheet();
    let mut session = Session::open(":memory:").expect("in-memory session");
    session.active_mut().path = Some("demo.rs".into());
    session.active_mut().insert("// vomvom — custom rendering engine\n\nmod render;\n\nfn main() {\n    // build scene, run event loop\n    println!(\"hello world\");\n}");
    let mut cache = std::collections::HashMap::new();
    rebuild_highlight_cache(&mut cache, &session);
    let mut doc = Document::new(build_initial_document(&session, &cache));
    open_menu(&mut doc, "file");
    (doc.root, sheet)
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

fn execute_menu_action(action: &str, session: &mut Session) {
    match action {
        "open" => open_file_dialog(session),
        "save" => { let _ = session.save_active(); }
        "undo" => { session.active_mut().undo(); }
        "redo" => { session.active_mut().redo(); }
        "close-tab" => { /* TODO */ }
        _ => {}
    }
}

// Walk toolbar menu headers by hit-testing their layout boxes, then reading
// the "menu" attribute from the corresponding node to identify which menu.
fn hit_test_menu_header(root: &Node, lb: &LayoutBox, mx: f32, my: f32) -> Option<String> {
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
fn hit_test_menu_item(root: &Node, lb: &LayoutBox, mx: f32, my: f32) -> Option<String> {
    use render::tree::NodeContent;
    let toolbar_lb = lb.children.first()?;
    let toolbar_node = root.children().first()?;
    for (i, header_lb) in toolbar_lb.children.iter().enumerate() {
        let header_node = &toolbar_node.children()[i];
        let dropdown_node = header_node.children().last()?;
        if !dropdown_node.has_class("dropdown") { continue; }
        let dropdown_lb = header_lb.children.last()?;
        for (j, item_lb) in dropdown_lb.children.iter().enumerate() {
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

fn hit_test_tab(root: &Node, lb: &LayoutBox, mx: f32, my: f32) -> Option<usize> {
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


/// Paint cursor bars at the given (line, col, text) positions over the editor area.
fn paint_cursors_with_text(
    canvas: &mut Canvas<OpenGl>,
    lb: &LayoutBox,
    cursors: &[(usize, usize, &str)], // (line_idx, col, line_text)
    mono_data: &'static [u8],
) {
    let Some(editor_lb) = lb.children.get(2) else { return };

    let cursor_color = femtovg::Color::rgbaf(0.9, 0.9, 1.0, 0.85);
    let font_size = 11.5f32;

    for &(line_idx, col, line_text) in cursors {
        let Some(line_lb) = editor_lb.children.get(line_idx) else { continue };
        let line_h = line_lb.border_box.h;
        let line_top = line_lb.border_box.y;

        let prefix: String = line_text.chars().take(col).collect();
        let x_off = render::glyph_cache::measure_text_width(mono_data, &prefix, font_size);
        let cursor_x = line_lb.content.x + x_off;

        let mut path = Path::new();
        path.rect(cursor_x, line_top, 2.0, line_h);
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
fn scrollbar_y_to_scroll(track_lb: &LayoutBox, session: &Session, my: f32, window_h: f32) -> usize {
    let buf = session.active();
    let total = buf.line_count();
    let visible = visible_lines(editor_content_height(window_h));
    let track_h = track_lb.border_box.h;
    let max_scroll = (total.saturating_sub(visible)) as f32;
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
fn hit_test_editor(
    lb: &LayoutBox,
    session: &Session,
    mx: f32,
    my: f32,
    mono_data: &'static [u8],
) -> Option<(usize, usize)> {
    let editor_lb = lb.children.get(2)?;
    if !editor_lb.border_box.contains(mx, my) { return None; }

    let buf = session.active();
    let scroll = buf.scroll_line;
    let font_size = 11.5f32;

    // Last child is the scrollbar track (absolutely positioned), not a text line.
    let line_count = editor_lb.children.len().saturating_sub(1);
    if line_count == 0 { return Some((0, 0)); }

    let mut best_layout_line = 0;
    for (i, line_lb) in editor_lb.children[..line_count].iter().enumerate() {
        if my >= line_lb.border_box.y {
            best_layout_line = i;
        }
    }

    let buf_line = best_layout_line + scroll;
    let line_text = buf.line(buf_line);
    let line_lb = &editor_lb.children[best_layout_line];
    let local_x = mx - line_lb.content.x;

    let chars: Vec<char> = line_text.chars().collect();
    let mut best_col = 0;
    let mut best_dist = f32::INFINITY;

    for col in 0..=chars.len() {
        let prefix: String = chars[..col].iter().collect();
        let x = render::glyph_cache::measure_text_width(mono_data, &prefix, font_size);
        let dist = (x - local_x).abs();
        if dist < best_dist {
            best_dist = dist;
            best_col = col;
        }
    }

    Some((buf_line, best_col))
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
