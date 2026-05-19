// Swash-based glyph cache with hinting.
// Rasterizes glyphs to an Rgba8 atlas managed as a femtovg image.
// Glyphs are stored as premultiplied white-with-alpha so Paint::image composites correctly.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use femtovg::{Canvas, ImageFlags, ImageId, ImageSource, PixelFormat, renderer::OpenGl};
use imgref::Img;
use rgb::RGBA8;
use swash::{
    FontRef,
    scale::{ScaleContext, Render, Source, image::Content},
};
use zeno::Format;

// --- OS font fallback ---

/// Info about a glyph found in a fallback font.
pub struct FallbackGlyph {
    pub glyph_id: u16,
    pub font_index: u8,       // stable index >= 2 assigned by FallbackFontDb
    pub bytes: Arc<Vec<u8>>,  // raw font file bytes
    pub face_index: usize,    // index within a font collection
    pub advance: f32,
}

struct FallbackFontDb {
    db: fontdb::Database,
    bytes_cache: HashMap<fontdb::ID, Arc<Vec<u8>>>,
    // Stable font_index (>= 2) assigned per fontdb::ID for use in GlyphKey.
    index_map: HashMap<fontdb::ID, u8>,
    next_index: u8,
}

impl FallbackFontDb {
    fn new() -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        FallbackFontDb {
            db,
            bytes_cache: HashMap::new(),
            index_map: HashMap::new(),
            next_index: 2, // 0=sans, 1=mono are reserved
        }
    }

    /// Find `ch` in a system font. Returns full glyph info for rasterization, or None.
    fn find(&mut self, ch: char, size_px: f32) -> Option<FallbackGlyph> {
        let ids: Vec<fontdb::ID> = self.db.faces().map(|f| f.id).collect();
        for id in ids {
            if let Some(g) = self.glyph_from_face(id, ch, size_px) {
                return Some(g);
            }
        }
        None
    }

    fn glyph_from_face(&mut self, id: fontdb::ID, ch: char, size_px: f32) -> Option<FallbackGlyph> {
        let face_index = self.db.face(id)?.index as usize;
        if !self.bytes_cache.contains_key(&id) {
            let bytes = self.db.with_face_data(id, |data, _| Arc::new(data.to_vec()))?;
            self.bytes_cache.insert(id, bytes);
        }
        let bytes = self.bytes_cache.get(&id)?.clone();
        let font_ref = FontRef::from_index(bytes.as_ref(), face_index)?;
        let gid = font_ref.charmap().map(ch);
        if gid == 0 {
            return None;
        }
        let adv = font_ref.glyph_metrics(&[]).scale(size_px).advance_width(gid);
        if adv <= 0.0 {
            return None;
        }
        // Assign a stable font_index for this fontdb face.
        let font_index = *self.index_map.entry(id).or_insert_with(|| {
            let idx = self.next_index;
            self.next_index = self.next_index.saturating_add(1);
            idx
        });
        Some(FallbackGlyph { glyph_id: gid as u16, font_index, bytes, face_index, advance: adv })
    }
}

/// Round `raw` to the nearest positive multiple of `e_width`.
fn round_to_e(raw: f32, e_width: f32) -> f32 {
    let multiples = (raw / e_width).round().max(1.0);
    multiples * e_width
}

static FALLBACK_DB: OnceLock<Mutex<FallbackFontDb>> = OnceLock::new();

fn fallback_db() -> &'static Mutex<FallbackFontDb> {
    FALLBACK_DB.get_or_init(|| Mutex::new(FallbackFontDb::new()))
}

/// Find the OS font that covers `ch` and return its bytes + face_index for loading into femtovg.
/// Returns None if no system font covers the character.
pub fn os_font_for_char(ch: char) -> Option<(Arc<Vec<u8>>, u32)> {
    let mut db = fallback_db().lock().ok()?;
    let ids: Vec<fontdb::ID> = db.db.faces().map(|f| f.id).collect();
    for id in ids {
        let face_index = db.db.face(id)?.index;
        if !db.bytes_cache.contains_key(&id) {
            let bytes = db.db.with_face_data(id, |data, _| Arc::new(data.to_vec()))?;
            db.bytes_cache.insert(id, bytes);
        }
        let bytes = db.bytes_cache.get(&id)?.clone();
        let font_ref = FontRef::from_index(bytes.as_ref(), face_index as usize)?;
        if font_ref.charmap().map(ch) != 0 {
            return Some((bytes, face_index as u32));
        }
    }
    None
}

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
            
            // Exponent bias to improve AA "pop" quality.
            // Future: check: for dark-on-light users this might need to be 1.5 instead of 1.0/1.5.
            let exp = 1.0/1.5;
            
            for row in 0..h {
                for col in 0..w {
                    let a = img.data[row * w + col];
                    let mut a = a as f32;
                    a = a / 255.0;
                    a = a.powf(exp);
                    a = a * 255.0;
                    let a = a.round() as u8;
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

/// One shaped glyph: glyph_id, advance, font bytes (None = primary font), font_index.
pub type ShapedGlyph = (u16, f32, Option<Arc<Vec<u8>>>, u8);

/// Simple horizontal text layout. Returns one ShapedGlyph per character.
/// Characters missing from the primary font are looked up in OS fallback fonts and
/// rasterized from there; their advance is rounded to a positive multiple of the
/// primary font's 'e' width so they snap to a clean grid relative to the base font.
pub fn layout_text(
    cache: &mut GlyphCache,
    font_data: &'static [u8],
    font_index: u8,
    text: &str,
    size_px: f32,
    hint: bool,
) -> Vec<ShapedGlyph> {
    let font_ref = match FontRef::from_index(font_data, 0) {
        Some(f) => f,
        None => return vec![],
    };
    let charmap = font_ref.charmap();
    let glyph_metrics = font_ref.glyph_metrics(&[]).scale(size_px);
    let e_width = glyph_metrics.advance_width(charmap.map('e'));

    let mut glyphs = Vec::with_capacity(text.len());
    for ch in text.chars() {
        let gid = charmap.map(ch);
        if gid == 0 {
            // Try OS fallback fonts.
            let fallback = fallback_db().lock().ok().and_then(|mut db| db.find(ch, size_px));
            if let Some(fb) = fallback {
                let adv = round_to_e(fb.advance, e_width);
                cache.get_or_rasterize(&fb.bytes, fb.font_index, fb.glyph_id, size_px, hint);
                glyphs.push((fb.glyph_id, adv, Some(fb.bytes), fb.font_index));
            } else {
                // Truly unknown: reserve space, render nothing.
                glyphs.push((0u16, e_width, None, font_index));
            }
        } else {
            let adv = glyph_metrics.advance_width(gid);
            cache.get_or_rasterize(font_data, font_index, gid as u16, size_px, hint);
            glyphs.push((gid, adv, None, font_index));
        }
    }
    glyphs
}

/// Measure the total pixel width of a text string.
/// Characters missing from the primary font use a fallback advance rounded to a
/// positive multiple of the primary font's 'e' width.
pub fn measure_text_width(font_data: &'static [u8], text: &str, size_px: f32) -> f32 {
    let font_ref = match FontRef::from_index(font_data, 0) {
        Some(f) => f,
        None => return 0.0,
    };
    let charmap = font_ref.charmap();
    let glyph_metrics = font_ref.glyph_metrics(&[]).scale(size_px);
    let e_width = glyph_metrics.advance_width(charmap.map('e'));
    text.chars().map(|ch| {
        let gid = charmap.map(ch);
        if gid == 0 {
            let raw = fallback_db().lock().ok()
                .and_then(|mut db| db.find(ch, size_px))
                .map(|fb| fb.advance)
                .unwrap_or(e_width);
            round_to_e(raw, e_width)
        } else {
            glyph_metrics.advance_width(gid)
        }
    }).sum()
}
