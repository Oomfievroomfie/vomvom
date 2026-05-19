// Shared headless GL context setup for screenshot and replay paths.

use std::num::NonZeroU32;
use std::sync::Arc;

use femtovg::{renderer::OpenGl, Canvas};
use glutin::{
    config::ConfigTemplateBuilder,
    context::{ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext},
    display::GetGlDisplay,
    prelude::*,
    surface::{Surface, SurfaceAttributesBuilder, WindowSurface},
};
use glutin_winit::DisplayBuilder;
use raw_window_handle::HasWindowHandle;
use winit::{
    dpi::PhysicalSize,
    event_loop::EventLoop,
    window::WindowAttributes,
};

pub struct HeadlessGl {
    pub canvas: Canvas<OpenGl>,
    pub gl_surface: Surface<WindowSurface>,
    pub gl_context: PossiblyCurrentContext,
    pub width: u32,
    pub height: u32,
}

pub fn setup(width: u32, height: u32) -> (HeadlessGl, EventLoop<()>) {
    let event_loop = EventLoop::new().unwrap();

    let win_attrs = WindowAttributes::default()
        .with_title("vomvom-headless")
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
        .expect("headless_gl: failed to create window");

    let window = Arc::new(window.unwrap());
    let raw_handle = window.window_handle().unwrap();

    let ctx_attrs = ContextAttributesBuilder::new().build(Some(raw_handle.as_raw()));
    let fallback = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::Gles(None))
        .build(Some(raw_handle.as_raw()));

    let gl_display = gl_config.display();
    let not_current = unsafe {
        gl_display
            .create_context(&gl_config, &ctx_attrs)
            .or_else(|_| gl_display.create_context(&gl_config, &fallback))
            .expect("headless_gl: failed to create GL context")
    };

    let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
        raw_handle.as_raw(),
        NonZeroU32::new(width.max(1)).unwrap(),
        NonZeroU32::new(height.max(1)).unwrap(),
    );
    let gl_surface: Surface<WindowSurface> = unsafe {
        gl_display
            .create_window_surface(&gl_config, &surface_attrs)
            .expect("headless_gl: failed to create GL surface")
    };
    let gl_context = not_current.make_current(&gl_surface).unwrap();

    let renderer = unsafe {
        OpenGl::new_from_function_cstr(|s| gl_display.get_proc_address(s) as *const _)
            .expect("headless_gl: failed to create femtovg renderer")
    };

    let canvas = Canvas::new(renderer).expect("headless_gl: failed to create canvas");

    let hgl = HeadlessGl { canvas, gl_surface, gl_context, width, height };
    (hgl, event_loop)
}
