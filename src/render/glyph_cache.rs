// Swash-based glyph cache with hinting.
// Rasterizes glyphs to an Rgba8 atlas managed as a femtovg image.
// Glyphs are stored as premultiplied white-with-alpha so Paint::image composites correctly.

use std::collections::HashMap;
use femtovg::{Canvas, ImageFlags, ImageId, ImageSource, PixelFormat, renderer::OpenGl};
use imgref::Img;
use rgb::RGBA8;
use swash::{
    FontRef,
    scale::{ScaleContext, Render, Source, image::Content},
};
use zeno::Format;

pub const ATLAS_SIZE: usize = 1024;
const GLYPH_PAD: usize = 1;

/// Position of a cached glyph in the atlas.
#[derive(Clone, Copy)]
pub struct CachedGlyph {
    /// Top-left in atlas texture (pixels).
    pub atlas_x: u16,
    pub atlas_y: u16,
    /// Rendered bitmap size.
    pub width: u16,
    pub height: u16,
    /// Pen-relative offset to top-left of bitmap (pixels, Y-down).
    pub bearing_x: i16,
    pub bearing_y: i16,
    /// Advance width in pixels.
    pub advance: f32,
}

#[derive(Hash, PartialEq, Eq, Clone, Copy)]
struct GlyphKey {
    font_index: u8,   // 0 = proportional, 1 = mono
    glyph_id: u16,
    size_px: u16,     // font_size rounded to nearest integer
}

/// Manages a Gray8 glyph atlas and swash rasterization.
pub struct GlyphCache {
    scale_ctx: ScaleContext,
    glyphs: HashMap<GlyphKey, Option<CachedGlyph>>,
    // Simple shelf packer state.
    shelf_x: usize,
    shelf_y: usize,
    shelf_h: usize,
    pub atlas: Option<ImageId>,
    // Raw atlas bytes for partial updates.
    atlas_data: Vec<u8>,
    dirty: bool,
}

impl GlyphCache {
    pub fn new() -> Self {
        GlyphCache {
            scale_ctx: ScaleContext::new(),
            glyphs: HashMap::new(),
            shelf_x: 0,
            shelf_y: 0,
            shelf_h: 0,
            atlas: None,
            atlas_data: vec![0u8; ATLAS_SIZE * ATLAS_SIZE * 4],
            dirty: false,
        }
    }

    /// Ensure the femtovg atlas image exists.
    pub fn ensure_atlas(&mut self, canvas: &mut Canvas<OpenGl>) {
        if self.atlas.is_none() {
            let id = canvas
                .create_image_empty(ATLAS_SIZE, ATLAS_SIZE, PixelFormat::Rgba8, ImageFlags::PREMULTIPLIED)
                .expect("failed to create glyph atlas");
            self.atlas = Some(id);
        }
    }

    /// Look up or rasterize a glyph. Returns None for whitespace / missing glyphs.
    pub fn get_or_rasterize(
        &mut self,
        font_data: &[u8],
        font_index: u8,
        glyph_id: u16,
        size_px: f32,
        hint: bool,
    ) -> Option<CachedGlyph> {
        let size_rounded = size_px.round() as u16;
        let key = GlyphKey { font_index, glyph_id, size_px: size_rounded };

        if let Some(&cached) = self.glyphs.get(&key) {
            return cached;
        }

        let font_ref = FontRef::from_index(font_data, 0)?;
        let mut scaler = self.scale_ctx
            .builder(font_ref)
            .size(size_rounded as f32)
            .hint(hint)
            .build();

        let image = Render::new(&[Source::Outline])
            .format(Format::Alpha)
            .render(&mut scaler, glyph_id);

        let result = image.and_then(|img| {
            if img.content != Content::Mask || img.data.is_empty() {
                return None;
            }
            let p = img.placement;
            let w = p.width as usize;
            let h = p.height as usize;
            if w == 0 || h == 0 {
                return None;
            }
            let (ax, ay) = self.alloc_shelf(w, h)?;
            // Blit glyph bitmap into atlas as premultiplied RGBA (white × alpha).
            for row in 0..h {
                for col in 0..w {
                    let a = img.data[row * w + col];
                    let base = ((ay + row) * ATLAS_SIZE + (ax + col)) * 4;
                    self.atlas_data[base]     = a;
                    self.atlas_data[base + 1] = a;
                    self.atlas_data[base + 2] = a;
                    self.atlas_data[base + 3] = a;
                }
            }
            self.dirty = true;
            Some(CachedGlyph {
                atlas_x: ax as u16,
                atlas_y: ay as u16,
                width: w as u16,
                height: h as u16,
                bearing_x: p.left as i16,
                bearing_y: p.top as i16,
                advance: 0.0, // filled in by caller from shaping metrics
            })
        });

        self.glyphs.insert(key, result);
        result
    }

    /// Allocate a slot on the shelf packer. Returns (x, y) or None if atlas is full.
    fn alloc_shelf(&mut self, w: usize, h: usize) -> Option<(usize, usize)> {
        let padded_w = w + GLYPH_PAD;
        let padded_h = h + GLYPH_PAD;

        if self.shelf_x + padded_w > ATLAS_SIZE {
            // Advance to next shelf.
            self.shelf_y += self.shelf_h;
            self.shelf_x = 0;
            self.shelf_h = 0;
        }

        if self.shelf_y + padded_h > ATLAS_SIZE {
            return None; // Atlas full.
        }

        let x = self.shelf_x;
        let y = self.shelf_y;
        self.shelf_x += padded_w;
        self.shelf_h = self.shelf_h.max(padded_h);
        Some((x, y))
    }

    /// Look up a previously rasterized glyph without rasterizing.
    pub fn get_cached(&self, glyph_id: u16, font_index: u8, size_px: f32) -> Option<CachedGlyph> {
        let key = GlyphKey { font_index, glyph_id, size_px: size_px.round() as u16 };
        self.glyphs.get(&key).copied().flatten()
    }


    /// Upload dirty atlas data to GPU.
    pub fn flush(&mut self, canvas: &mut Canvas<OpenGl>) {
        if !self.dirty {
            return;
        }
        if let Some(id) = self.atlas {
            let rgba_slice: &[RGBA8] = unsafe {
                std::slice::from_raw_parts(self.atlas_data.as_ptr() as *const RGBA8, ATLAS_SIZE * ATLAS_SIZE)
            };
            let img = Img::new(rgba_slice, ATLAS_SIZE, ATLAS_SIZE);
            let src = ImageSource::from(img);
            let _ = canvas.update_image(id, src, 0, 0);
            self.dirty = false;
        }
    }
}

/// Per-font slot data needed for text layout and rendering.
pub struct FontSlot {
    pub data: &'static [u8],
    pub index: u8,
}

/// Simple horizontal text layout: returns a list of (glyph_id, advance).
/// Uses swash charmap to map chars to glyph IDs, then queries scaler for advance widths.
pub fn layout_text(
    cache: &mut GlyphCache,
    font_data: &'static [u8],
    font_index: u8,
    text: &str,
    size_px: f32,
    hint: bool,
) -> Vec<(u16, f32)> {
    let font_ref = match FontRef::from_index(font_data, 0) {
        Some(f) => f,
        None => return vec![],
    };
    let charmap = font_ref.charmap();
    let glyph_metrics = font_ref.glyph_metrics(&[]).scale(size_px);

    let mut glyphs = Vec::with_capacity(text.len());
    for ch in text.chars() {
        let gid = charmap.map(ch);
        let adv = glyph_metrics.advance_width(gid);
        glyphs.push((gid, adv));
        // Pre-rasterize so atlas is ready before draw.
        cache.get_or_rasterize(font_data, font_index, gid as u16, size_px, hint);
    }
    glyphs
}

/// Measure the total pixel width of a text string.
pub fn measure_text_width(font_data: &'static [u8], text: &str, size_px: f32) -> f32 {
    let font_ref = match FontRef::from_index(font_data, 0) {
        Some(f) => f,
        None => return 0.0,
    };
    let charmap = font_ref.charmap();
    let glyph_metrics = font_ref.glyph_metrics(&[]).scale(size_px);
    text.chars().map(|ch| {
        glyph_metrics.advance_width(charmap.map(ch))
    }).sum()
}
