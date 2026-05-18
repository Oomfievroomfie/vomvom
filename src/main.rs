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

use render::layout::{layout, finalize_positions, Constraints};
use render::paint::{PaintContext, paint_tree};
use render::style::{
    AlignItems, Color, Display, Edges, FlexDirection, JustifyContent, Length, Selector,
    Stylesheet, StyleDecl, Overflow,
};
use render::tree::{Node, apply_styles};
use render::glyph_cache::GlyphCache;
use session::Session;

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
                let buf = state.session.active_mut();
                let mut dirty = true;
                match &event.logical_key {
                    Key::Character(s) if ctrl && (s == "z" || s == "Z") => { buf.undo(); }
                    Key::Character(s) if ctrl && (s == "y" || s == "Y") => { buf.redo(); }
                    Key::Character(s) if ctrl && (s == "s" || s == "S") => {
                        let _ = state.session.save_active();
                        dirty = false;
                    }
                    // debug toggles
                    Key::Character(s) if ctrl && (s == "f" || s == "F") => {
                        state.use_femtovg = !state.use_femtovg;
                        dirty = true;
                    }
                    Key::Character(s) if ctrl && (s == "h" || s == "H") => {
                        state.hint = !state.hint;
                        state.glyph_cache = GlyphCache::new();
                        dirty = true;
                    }
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
            WindowEvent::RedrawRequested => {
                let _ = state.session.tick();

                let size = state.window.inner_size();
                let w = size.width as f32;
                let h = size.height as f32;
                let scale = state.window.scale_factor() as f32;

                state.canvas.set_size(size.width, size.height, scale);
                state.canvas.clear_rect(0, 0, size.width, size.height,
                    femtovg::Color::rgbf(0.15, 0.15, 0.18));

                let mut scene = build_scene(&state.session, &state.sheet);
                apply_styles(&mut scene, &state.sheet, &[], None);
                let mut measurer = render::femtovg_measurer::SwashMeasurer {
                    sans_data: SANS_BYTES,
                    mono_data: MONO_BYTES,
                };
                let mut lb = layout(&scene, Constraints::new(w, h), &mut measurer);
                finalize_positions(&mut lb);

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
                paint_tree(&mut ctx, &scene, &lb);

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
    let mut sheet = Stylesheet::new();

    sheet.add(Selector::Tag("root".into()), vec![
        StyleDecl::Display(Display::Flex),
        StyleDecl::FlexDirection(FlexDirection::Column),
        StyleDecl::Width(Length::Percent(100.0)),
        StyleDecl::Height(Length::Percent(100.0)),
        StyleDecl::BackgroundColor(Color::rgb(0.15, 0.15, 0.18)),
    ]);

    sheet.add(Selector::Class("statusbar".into()), vec![
        StyleDecl::Display(Display::Flex),
        StyleDecl::FlexDirection(FlexDirection::Row),
        StyleDecl::AlignItems(AlignItems::Center),
        StyleDecl::JustifyContent(JustifyContent::SpaceBetween),
        StyleDecl::Height(Length::Px(24.0)),
        StyleDecl::BackgroundColor(Color::rgb(0.2, 0.4, 0.7)),
        StyleDecl::Padding(Edges { left: Length::Px(10.0), right: Length::Px(10.0), top: Length::Zero, bottom: Length::Zero }),
        StyleDecl::Color(Color::WHITE),
        StyleDecl::FontSize(12.0),
    ]);

    sheet.add(Selector::Class("editor".into()), vec![
        StyleDecl::FlexGrow(1.0),
        StyleDecl::BackgroundColor(Color::rgb(0.13, 0.13, 0.16)),
        StyleDecl::Padding(Edges::uniform_px(16.0)),
        StyleDecl::FontFamily("monospace".into()),
        StyleDecl::FontSize(14.0),
        StyleDecl::Color(Color::rgb(0.85, 0.85, 0.9)),
        StyleDecl::OverflowY(Overflow::Hidden),
    ]);

    sheet.add(Selector::Class("line".into()), vec![
        StyleDecl::Display(Display::Block),
        StyleDecl::LineHeight(1.4),
        StyleDecl::FontSize(14.0),
    ]);

    sheet.add(Selector::Class("cursor-line".into()), vec![
        StyleDecl::BackgroundColor(Color::rgba(1.0, 1.0, 1.0, 0.05)),
    ]);

    sheet
}

fn build_scene(session: &Session, _sheet: &Stylesheet) -> Node {
    let buf = session.active();
    let cursor = buf.cursor;

    let line_count = buf.line_count();
    let path_label = buf.path.as_deref().unwrap_or("[untitled]");
    let modified_marker = if buf.is_modified { " ●" } else { "" };
    let status_left = format!("{}{}", path_label, modified_marker);
    let status_right = format!("Ln {}, Col {}  UTF-8", cursor.line + 1, cursor.col + 1);

    let mut editor = Node::element("div").with_class("editor");
    for i in 0..line_count {
        let text = buf.line(i).to_string();
        let mut line_node = Node::element("div").with_class("line");
        if i == cursor.line {
            line_node = line_node.with_class("cursor-line");
        }
        line_node = line_node.with_child(Node::text(if text.is_empty() { " ".into() } else { text }));
        editor = editor.with_child(line_node);
    }

    let statusbar = Node::element("div")
        .with_class("statusbar")
        .with_child(Node::text(status_left))
        .with_child(Node::text(status_right));

    Node::element("root")
        .with_child(editor)
        .with_child(statusbar)
}

pub fn build_demo_scene() -> (Node, Stylesheet) {
    let sheet = build_stylesheet();
    // Build a static demo using a temporary session so screenshot still works.
    let session = Session::open(":memory:").expect("in-memory session");
    let scene = build_scene(&session, &sheet);
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
