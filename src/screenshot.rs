// Headless screenshot: spin up a hidden window, render one frame, save PNG.

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;

use femtovg::{renderer::OpenGl, Canvas};
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

use crate::render::layout::{layout, finalize_positions, Constraints};
use crate::render::paint::{PaintContext, paint_tree};
use crate::render::tree::apply_styles;
use crate::render::glyph_cache::GlyphCache;
use crate::{build_demo_scene, SANS_BYTES, MONO_BYTES};

pub fn save_screenshot(path: &Path, width: u32, height: u32) {
    let event_loop = EventLoop::new().unwrap();

    let win_attrs = WindowAttributes::default()
        .with_title("vomvom-screenshot")
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
        .expect("failed to create window for screenshot");

    let window = Arc::new(window.unwrap());
    let raw_handle = window.window_handle().unwrap();

    let ctx_attrs = ContextAttributesBuilder::new().build(Some(raw_handle.as_raw()));
    let fallback = ContextAttributesBuilder::new()
        .with_context_api(ContextApi::Gles(None))
        .build(Some(raw_handle.as_raw()));

    let gl_display = gl_config.display();
    let _gl_context = unsafe {
        gl_display.create_context(&gl_config, &ctx_attrs)
            .or_else(|_| gl_display.create_context(&gl_config, &fallback))
            .expect("failed to create GL context")
    };

    let surface_attrs = SurfaceAttributesBuilder::<WindowSurface>::new().build(
        raw_handle.as_raw(),
        NonZeroU32::new(width.max(1)).unwrap(),
        NonZeroU32::new(height.max(1)).unwrap(),
    );
    let gl_surface: Surface<WindowSurface> = unsafe {
        gl_display.create_window_surface(&gl_config, &surface_attrs)
            .expect("failed to create GL surface")
    };

    let gl_context = _gl_context.make_current(&gl_surface).unwrap();

    let renderer = unsafe {
        OpenGl::new_from_function_cstr(|s| gl_display.get_proc_address(s) as *const _)
            .expect("failed to create femtovg renderer")
    };

    let mut canvas = Canvas::new(renderer).expect("failed to create canvas");
    canvas.set_size(width, height, 1.0);

    let (mut scene, sheet) = build_demo_scene();

    canvas.clear_rect(0, 0, width, height, femtovg::Color::rgbf(0.15, 0.15, 0.18));

    apply_styles(&mut scene, &sheet, &[], None);
    let mut measurer = crate::render::femtovg_measurer::SwashMeasurer {
        sans_data: SANS_BYTES,
        mono_data: MONO_BYTES,
    };
    let mut lb = layout(&scene, Constraints::new(width as f32, height as f32), &mut measurer);
    finalize_positions(&mut lb);

    let mut glyph_cache = GlyphCache::new();
    let mut ctx = PaintContext {
        canvas: &mut canvas,
        glyph_cache: &mut glyph_cache,
        sans_data: SANS_BYTES,
        mono_data: MONO_BYTES,
        hint: true,
    };
    paint_tree(&mut ctx, &scene, &lb);
    ctx.canvas.flush();

    let img = ctx.canvas.screenshot().expect("screenshot failed");
    let (w, h) = (img.width(), img.height());
    let pixels: Vec<u8> = img.pixels().flat_map(|p| [p.r, p.g, p.b, p.a]).collect();
    image::save_buffer(path, &pixels, w as u32, h as u32, image::ColorType::Rgba8)
        .expect("failed to save PNG");

    println!("Screenshot saved to {}", path.display());

    let _ = gl_context;
}
