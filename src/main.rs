mod render;
mod screenshot;
mod session;

use std::num::NonZeroU32;
use std::sync::Arc;

use femtovg::{renderer::OpenGl, Canvas, FontId};
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
    event_loop::{ActiveEventLoop, EventLoop},
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
static MONO_BYTES: &[u8] = include_bytes!("../FiraMono-Regular.ttf");

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

        let doc = Document::new(build_initial_document(&session));

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
                        sync_doc_to_session(&mut state.doc, &state.session);
                        state.window.request_redraw();
                        return;
                    }
                    Key::Character(s) if ctrl && (s == "s" || s == "S") => {
                        let _ = state.session.save_active();
                        update_statusbar(&mut state.doc, &state.session);
                        state.window.request_redraw();
                        return;
                    }
                    Key::Character(s) if ctrl && (s == "f" || s == "F") => {
                        state.use_femtovg = !state.use_femtovg;
                        state.window.request_redraw();
                        return;
                    }
                    Key::Character(s) if ctrl && (s == "h" || s == "H") => {
                        state.hint = !state.hint;
                        state.glyph_cache = GlyphCache::new();
                        state.window.request_redraw();
                        return;
                    }
                    _ => {}
                }

                let buf = state.session.active_mut();
                match &key {
                    Key::Named(NamedKey::Escape) => {
                        close_all_menus(&mut state.doc);
                        state.window.request_redraw();
                        return;
                    }
                    Key::Character(s) if ctrl && (s == "z" || s == "Z") => { buf.undo(); }
                    Key::Character(s) if ctrl && (s == "y" || s == "Y") => { buf.redo(); }
                    Key::Named(NamedKey::Backspace) => { buf.backspace(); }
                    Key::Named(NamedKey::Delete) => { buf.delete_forward(); }
                    Key::Named(NamedKey::Enter) => { buf.insert("\n"); }
                    Key::Named(NamedKey::Tab) => { buf.insert("    "); }
                    Key::Named(NamedKey::ArrowLeft) => {
                        let pos = buf.cursor;
                        let (l, c) = if pos.col > 0 { (pos.line, pos.col - 1) } else if pos.line > 0 { (pos.line - 1, buf.line(pos.line - 1).len()) } else { (0, 0) };
                        buf.move_cursor(l, c);
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        let pos = buf.cursor;
                        let line_len = buf.line(pos.line).len();
                        let (l, c) = if pos.col < line_len { (pos.line, pos.col + 1) } else if pos.line + 1 < buf.line_count() { (pos.line + 1, 0) } else { (pos.line, pos.col) };
                        buf.move_cursor(l, c);
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        let pos = buf.cursor;
                        if pos.line > 0 { buf.move_cursor(pos.line - 1, pos.col); }
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        let pos = buf.cursor;
                        if pos.line + 1 < buf.line_count() { buf.move_cursor(pos.line + 1, pos.col); }
                    }
                    Key::Named(NamedKey::Home) => {
                        let line = buf.cursor.line;
                        buf.move_cursor(line, 0);
                    }
                    Key::Named(NamedKey::End) => {
                        let line = buf.cursor.line;
                        let len = buf.line(line).len();
                        buf.move_cursor(line, len);
                    }
                    Key::Character(s) if !ctrl => { buf.insert(s); }
                    _ => { dirty = false; }
                }
                if dirty {
                    update_editor(&mut state.doc, &state.session);
                    update_statusbar(&mut state.doc, &state.session);
                    state.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                state.mouse_pos = (position.x as f32, position.y as f32);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: winit::event::MouseButton::Left, .. } => {
                let (mx, my) = state.mouse_pos;
                if let Some(ref lb) = state.last_layout.clone() {
                    // Menu item click (dropdown open)?
                    if any_menu_open(&state.doc) {
                        if let Some(action) = hit_test_menu_item(&state.doc.root, lb, mx, my) {
                            close_all_menus(&mut state.doc);
                            execute_menu_action(&action, &mut state.session);
                            sync_doc_to_session(&mut state.doc, &state.session);
                            state.window.request_redraw();
                            return;
                        }
                        close_all_menus(&mut state.doc);
                        state.window.request_redraw();
                        return;
                    }
                    // Menu header click?
                    if let Some(menu_id) = hit_test_menu_header(&state.doc.root, lb, mx, my) {
                        open_menu(&mut state.doc, &menu_id);
                        state.window.request_redraw();
                        return;
                    }
                    // Tab click?
                    if let Some(idx) = hit_test_tab(&state.doc.root, lb, mx, my) {
                        state.session.set_active(idx);
                        sync_doc_to_session(&mut state.doc, &state.session);
                        state.window.request_redraw();
                    }
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
                    state.window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }
    }
}

fn build_stylesheet() -> Stylesheet {
    parse_stylesheet(MAIN_CSS)
}

// Build the initial retained document. Called once; subsequent updates use
// the targeted mutation functions below.
fn build_initial_document(session: &Session) -> Node {
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
    update_editor_node(&mut root, session);
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

// --- Targeted document mutation functions ---

fn update_editor_node(root: &mut Node, session: &Session) {
    let buf = session.active();
    let cursor = buf.cursor;
    let Some(editor) = root.get_element_by_id("editor") else { return };
    editor.clear_children();
    for i in 0..buf.line_count() {
        let text = buf.line(i).to_string();
        let mut line_node = Node::element("div").with_class("line");
        if i == cursor.line { line_node = line_node.with_class("cursor-line"); }
        line_node = line_node.with_child(Node::text(if text.is_empty() { " ".into() } else { text }));
        editor.append_child(line_node);
    }
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
fn update_editor(doc: &mut Document, session: &Session) {
    update_editor_node(&mut doc.root, session);
    doc.mark_dirty();
}

fn update_statusbar(doc: &mut Document, session: &Session) {
    update_statusbar_node(&mut doc.root, session);
    doc.mark_dirty();
}

fn sync_doc_to_session(doc: &mut Document, session: &Session) {
    update_tab_bar_node(&mut doc.root, session);
    update_editor_node(&mut doc.root, session);
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
    session.active_mut().insert("// vomvom — custom rendering engine\n\nmod render;\n\nfn main() {\n    // build scene, run event loop\n    println!(\"hello world\");\n}");
    let mut doc = Document::new(build_initial_document(&session));
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
