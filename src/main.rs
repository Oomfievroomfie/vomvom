mod render;
mod screenshot;
mod session;

use std::num::NonZeroU32;
use std::sync::Arc;

use femtovg::{renderer::OpenGl, Canvas};
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
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

use render::layout::{layout, finalize_positions, Constraints};
use render::paint::{PaintContext, paint_tree};
use render::style::{
    AlignItems, Color, Display, Edges, FlexDirection, JustifyContent, Length, Selector,
    Stylesheet, StyleDecl,
};
use render::tree::{Node, apply_styles};
use render::glyph_cache::GlyphCache;

static SANS_BYTES: &[u8] = include_bytes!("../OpenSans-Medium.ttf");
static MONO_BYTES: &[u8] = include_bytes!("../FiraMono-Regular.ttf");

struct App {
    state: Option<AppState>,
}

struct AppState {
    window: Arc<Window>,
    canvas: Canvas<OpenGl>,
    gl_surface: Surface<WindowSurface>,
    gl_context: PossiblyCurrentContext,
    glyph_cache: GlyphCache,
    hint: bool,
    scene: Node,
    sheet: Stylesheet,
}

impl App {
    fn new() -> Self {
        App { state: None }
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

        let (scene, sheet) = build_demo_scene();

        self.state = Some(AppState {
            window,
            canvas,
            gl_surface,
            gl_context,
            glyph_cache: GlyphCache::new(),
            hint: true,
            scene,
            sheet,
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.state else { return };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                let size = state.window.inner_size();
                let w = size.width as f32;
                let h = size.height as f32;
                let scale = state.window.scale_factor() as f32;

                state.canvas.set_size(size.width, size.height, scale);
                state.canvas.clear_rect(0, 0, size.width, size.height,
                    femtovg::Color::rgbf(0.15, 0.15, 0.18));

                apply_styles(&mut state.scene, &state.sheet, &[], None);
                let mut measurer = render::femtovg_measurer::SwashMeasurer {
                    sans_data: SANS_BYTES,
                    mono_data: MONO_BYTES,
                };
                let mut lb = layout(&state.scene, Constraints::new(w, h), &mut measurer);
                finalize_positions(&mut lb);

                let mut ctx = PaintContext {
                    canvas: &mut state.canvas,
                    glyph_cache: &mut state.glyph_cache,
                    sans_data: SANS_BYTES,
                    mono_data: MONO_BYTES,
                    hint: state.hint,
                };
                paint_tree(&mut ctx, &state.scene, &lb);

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

fn build_demo_scene() -> (Node, Stylesheet) {
    let mut sheet = Stylesheet::new();

    // Root fills the window
    sheet.add(Selector::Tag("root".into()), vec![
        StyleDecl::Display(Display::Flex),
        StyleDecl::FlexDirection(FlexDirection::Column),
        StyleDecl::Width(Length::Percent(100.0)),
        StyleDecl::Height(Length::Percent(100.0)),
        StyleDecl::BackgroundColor(Color::rgb(0.15, 0.15, 0.18)),
    ]);

    // Toolbar
    sheet.add(Selector::Class("toolbar".into()), vec![
        StyleDecl::Display(Display::Flex),
        StyleDecl::FlexDirection(FlexDirection::Row),
        StyleDecl::AlignItems(AlignItems::Center),
        StyleDecl::Height(Length::Px(40.0)),
        StyleDecl::BackgroundColor(Color::rgb(0.2, 0.2, 0.25)),
        StyleDecl::Padding(Edges::uniform_px(6.0)),
        StyleDecl::Gap(8.0),
    ]);

    // Toolbar buttons
    sheet.add(Selector::Class("btn".into()), vec![
        StyleDecl::Display(Display::InlineBlock),
        StyleDecl::BackgroundColor(Color::rgb(0.3, 0.5, 0.8)),
        StyleDecl::Color(Color::WHITE),
        StyleDecl::Padding(Edges::uniform_px(6.0)),
        StyleDecl::BorderRadius(4.0),
        StyleDecl::FontSize(13.0),
    ]);

    // Editor body: sidebar + editor area
    sheet.add(Selector::Class("body".into()), vec![
        StyleDecl::Display(Display::Flex),
        StyleDecl::FlexDirection(FlexDirection::Row),
        StyleDecl::FlexGrow(1.0),
    ]);

    // Sidebar
    sheet.add(Selector::Class("sidebar".into()), vec![
        StyleDecl::Width(Length::Px(200.0)),
        StyleDecl::BackgroundColor(Color::rgb(0.18, 0.18, 0.22)),
        StyleDecl::Padding(Edges::uniform_px(12.0)),
    ]);

    // Sidebar file entries
    sheet.add(Selector::Class("file-entry".into()), vec![
        StyleDecl::Display(Display::Block),
        StyleDecl::Color(Color::rgb(0.75, 0.75, 0.85)),
        StyleDecl::FontSize(13.0),
        StyleDecl::Padding(Edges { top: Length::Px(3.0), bottom: Length::Px(3.0), left: Length::Zero, right: Length::Zero }),
    ]);
    sheet.add(Selector::And(vec![Selector::Class("file-entry".into()), Selector::Class("active".into())]), vec![
        StyleDecl::Color(Color::WHITE),
        StyleDecl::BackgroundColor(Color::rgba(0.3, 0.5, 0.9, 0.3)),
    ]);

    // Editor pane
    sheet.add(Selector::Class("editor".into()), vec![
        StyleDecl::FlexGrow(1.0),
        StyleDecl::BackgroundColor(Color::rgb(0.13, 0.13, 0.16)),
        StyleDecl::Padding(Edges::uniform_px(16.0)),
        StyleDecl::FontFamily("monospace".into()),
        StyleDecl::FontSize(14.0),
        StyleDecl::Color(Color::rgb(0.85, 0.85, 0.9)),
    ]);

    // Code lines
    sheet.add(Selector::Class("line".into()), vec![
        StyleDecl::Display(Display::Block),
        StyleDecl::LineHeight(1.6),
        StyleDecl::FontSize(14.0),
    ]);
    sheet.add(Selector::Class("keyword".into()), vec![
        StyleDecl::Color(Color::rgb(0.5, 0.7, 1.0)),
    ]);
    sheet.add(Selector::Class("string".into()), vec![
        StyleDecl::Color(Color::rgb(0.6, 0.9, 0.6)),
    ]);
    sheet.add(Selector::Class("comment".into()), vec![
        StyleDecl::Color(Color::rgb(0.5, 0.55, 0.6)),
    ]);

    // Status bar
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

    // Build tree
    let toolbar = Node::element("div")
        .with_class("toolbar")
        .with_child(Node::element("div").with_class("btn").with_child(Node::text("File")))
        .with_child(Node::element("div").with_class("btn").with_child(Node::text("Edit")))
        .with_child(Node::element("div").with_class("btn").with_child(Node::text("View")));

    let sidebar = Node::element("div")
        .with_class("sidebar")
        .with_child(Node::element("div").with_class("file-entry").with_class("active").with_child(Node::text("main.rs")))
        .with_child(Node::element("div").with_class("file-entry").with_child(Node::text("render/mod.rs")))
        .with_child(Node::element("div").with_class("file-entry").with_child(Node::text("render/style.rs")))
        .with_child(Node::element("div").with_class("file-entry").with_child(Node::text("render/layout.rs")))
        .with_child(Node::element("div").with_class("file-entry").with_child(Node::text("render/paint.rs")));

    let editor = Node::element("div")
        .with_class("editor")
        .with_child(Node::element("div").with_class("line").with_child(Node::text("// vomvom — custom rendering engine demo")))
        .with_child(Node::element("div").with_class("line").with_child(Node::text("")))
        .with_child(Node::element("div").with_class("line").with_child(Node::text("mod render;")))
        .with_child(Node::element("div").with_class("line").with_child(Node::text("")))
        .with_child(Node::element("div").with_class("line").with_child(Node::text("fn main() {")))
        .with_child(Node::element("div").with_class("line").with_child(Node::text("    // build scene, run event loop")))
        .with_child(Node::element("div").with_class("line").with_child(Node::text("}")));

    let body = Node::element("div")
        .with_class("body")
        .with_child(sidebar)
        .with_child(editor);

    let statusbar = Node::element("div")
        .with_class("statusbar")
        .with_child(Node::text("main.rs"))
        .with_child(Node::text("Ln 1, Col 1  UTF-8  Rust"));

    let root = Node::element("root")
        .with_child(toolbar)
        .with_child(body)
        .with_child(statusbar);

    (root, sheet)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--screenshot") {
        let path = args.get(pos + 1).map(|s| s.as_str()).unwrap_or("screenshots/screenshot.png");
        std::fs::create_dir_all("screenshots").unwrap();
        screenshot::save_screenshot(std::path::Path::new(path), 1024, 768);
        return;
    }

    let event_loop = EventLoop::new().unwrap();
    let mut app = App::new();
    event_loop.run_app(&mut app).unwrap();
}
