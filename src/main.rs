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
use render::tree::{Node, apply_styles};
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
    modifiers: Modifiers,
    mouse_pos: (f32, f32),
    last_layout: Option<(Node, LayoutBox)>,
    open_menu: Option<String>,
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
            modifiers: Modifiers::default(),
            mouse_pos: (0.0, 0.0),
            last_layout: None,
            open_menu: None,
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

                // Handle commands that need full session access first.
                match &key {
                    Key::Character(s) if ctrl && (s == "o" || s == "O") => {
                        open_file_dialog(&mut state.session);
                        state.window.request_redraw();
                        return;
                    }
                    Key::Character(s) if ctrl && (s == "s" || s == "S") => {
                        let _ = state.session.save_active();
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
                        if state.open_menu.is_some() {
                            state.open_menu = None;
                            state.window.request_redraw();
                        }
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
                if dirty { state.window.request_redraw(); }
            }
            WindowEvent::CursorMoved { position, .. } => {
                state.mouse_pos = (position.x as f32, position.y as f32);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: winit::event::MouseButton::Left, .. } => {
                let (mx, my) = state.mouse_pos;
                if let Some((ref scene, ref lb)) = state.last_layout.clone() {
                    // Menu item click (dropdown open)?
                    if state.open_menu.is_some() {
                        if let Some(action) = hit_test_menu_item(scene, lb, mx, my) {
                            state.open_menu = None;
                            execute_menu_action(&action, &mut state.session);
                            state.window.request_redraw();
                            return;
                        }
                        // Click outside menu: close it.
                        state.open_menu = None;
                        state.window.request_redraw();
                        return;
                    }
                    // Menu header click?
                    if let Some(menu) = hit_test_menu_header(scene, lb, mx, my) {
                        state.open_menu = Some(menu);
                        state.window.request_redraw();
                        return;
                    }
                    // Tab click?
                    if let Some(idx) = hit_test_tab(scene, lb, mx, my) {
                        state.session.set_active(idx);
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

                let mut scene = build_scene(&state.session, &state.sheet, state.open_menu.as_deref());
                apply_styles(&mut scene, &state.sheet, &[], None);
                let mut measurer = render::femtovg_measurer::SwashMeasurer {
                    sans_data: SANS_BYTES,
                    mono_data: MONO_BYTES,
                };
                let mut lb = layout(&scene, Constraints::new(w, h), &mut measurer);
                finalize_positions(&mut lb);
                state.last_layout = Some((scene.clone(), lb.clone()));

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
                paint_tree_root(&mut ctx, &scene, &lb);

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

fn build_scene(session: &Session, _sheet: &Stylesheet, open_menu: Option<&str>) -> Node {
    let buf = session.active();
    let cursor = buf.cursor;
    let line_count = buf.line_count();

    let path_label = buf.path.as_deref().unwrap_or("[untitled]");
    let modified_marker = if buf.is_modified { " ●" } else { "" };
    let status_left = format!("{}{}", path_label, modified_marker);
    let status_right = format!("Ln {}, Col {}  UTF-8", cursor.line + 1, cursor.col + 1);

    // Menu headers — clicking opens their dropdown.
    let menus = [("File", "file"), ("Edit", "edit")];
    let mut toolbar = Node::element("div").with_class("toolbar");
    for (label, id) in &menus {
        let mut header = Node::element("div")
            .with_class("menu-header")
            .with_attr("menu", *id);
        if open_menu == Some(id) { header = header.with_class("open"); }
        header = header.with_child(Node::text(*label));

        // Attach dropdown as absolutely-positioned child of the header.
        if open_menu == Some(id) {
            let dropdown = build_dropdown(id);
            header = header.with_child(dropdown);
        }
        toolbar = toolbar.with_child(header);
    }

    // Tab bar: one tab per buffer
    let mut tab_bar = Node::element("div").with_class("tab-bar");
    for (i, b) in session.buffers.iter().enumerate() {
        let label = b.path.as_deref()
            .and_then(|p| std::path::Path::new(p).file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("[untitled]")
            .to_string();
        let modified = if b.is_modified { " ●" } else { "" };
        let mut tab = Node::element("div").with_class("tab");
        if i == session.active_idx { tab = tab.with_class("active"); }
        tab = tab.with_attr("tab-idx", &i.to_string())
            .with_child(Node::text(format!("{}{}", label, modified)));
        tab_bar = tab_bar.with_child(tab);
    }

    let mut editor = Node::element("div").with_class("editor");
    for i in 0..line_count {
        let text = buf.line(i).to_string();
        let mut line_node = Node::element("div").with_class("line");
        if i == cursor.line { line_node = line_node.with_class("cursor-line"); }
        line_node = line_node.with_child(Node::text(if text.is_empty() { " ".into() } else { text }));
        editor = editor.with_child(line_node);
    }

    let statusbar = Node::element("div")
        .with_class("statusbar")
        .with_child(Node::text(status_left))
        .with_child(Node::text(status_right));

    Node::element("root")
        .with_child(toolbar)
        .with_child(tab_bar)
        .with_child(editor)
        .with_child(statusbar)
}

pub fn build_demo_scene() -> (Node, Stylesheet) {
    let sheet = build_stylesheet();
    let mut session = Session::open(":memory:").expect("in-memory session");
    session.active_mut().insert("// vomvom — custom rendering engine\n\nmod render;\n\nfn main() {\n    // build scene, run event loop\n    println!(\"hello world\");\n}");
    let scene = build_scene(&session, &sheet, Some("file"));
    (scene, sheet)
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

// Walk toolbar menu headers (children of toolbar = root child 0).
fn hit_test_menu_header(scene: &Node, lb: &LayoutBox, mx: f32, my: f32) -> Option<String> {
    use render::tree::NodeContent;
    let toolbar_lb = lb.children.first()?;
    let toolbar_node = scene.children().first()?;
    for (i, child_lb) in toolbar_lb.children.iter().enumerate() {
        let r = child_lb.border_box;
        if r.contains(mx, my) {
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

// Walk dropdown menu items. Dropdown is a child of the open menu header.
fn hit_test_menu_item(scene: &Node, lb: &LayoutBox, mx: f32, my: f32) -> Option<String> {
    use render::tree::NodeContent;
    let toolbar_lb = lb.children.first()?;
    let toolbar_node = scene.children().first()?;
    // Find the open menu header (the one with a dropdown child).
    for (i, header_lb) in toolbar_lb.children.iter().enumerate() {
        let header_node = &toolbar_node.children()[i];
        // The dropdown is the last child if present.
        let dropdown_node = header_node.children().last()?;
        if let NodeContent::Element { classes, .. } = &dropdown_node.content {
            if !classes.contains(&"dropdown".to_string()) { continue; }
        } else { continue; }
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

fn hit_test_toolbar(scene: &Node, lb: &LayoutBox, mx: f32, my: f32) -> Option<String> {
    use render::tree::NodeContent;
    // Walk toolbar children (first child of root = toolbar, its children = buttons)
    let toolbar_lb = lb.children.first()?;
    let root_children = scene.children();
    let toolbar_node = root_children.first()?;
    for (i, btn_lb) in toolbar_lb.children.iter().enumerate() {
        let r = btn_lb.border_box;
        if mx >= r.x && mx <= r.x + r.w && my >= r.y && my <= r.y + r.h {
            let btn_node = &toolbar_node.children()[i];
            if let NodeContent::Element { attrs, .. } = &btn_node.content {
                if let Some(action) = attrs.get("action") {
                    return Some(action.clone());
                }
            }
        }
    }
    None
}

fn hit_test_tab(scene: &Node, lb: &LayoutBox, mx: f32, my: f32) -> Option<usize> {
    use render::tree::NodeContent;
    // Tab bar is second child of root
    let tab_bar_lb = lb.children.get(1)?;
    let root_children = scene.children();
    let tab_bar_node = root_children.get(1)?;
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
