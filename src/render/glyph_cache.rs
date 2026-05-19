// Swash-based glyph cache with hinting.
// Rasterizes glyphs to an Rgba8 atlas managed as a femtovg image.
// Glyphs are stored as premultiplied white-with-alpha so Paint::image composites correctly.

use std::collections::HashMap;
use std::ops::Range;
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
    /// Prefers fonts with a GSUB table (needed for Arabic shaping).
    fn find(&mut self, ch: char, size_px: f32) -> Option<FallbackGlyph> {
        let ids: Vec<fontdb::ID> = self.db.faces().map(|f| f.id).collect();
        // First pass: prefer fonts with GSUB table.
        for &id in &ids {
            if let Some(g) = self.glyph_from_face(id, ch, size_px) {
                if self.has_gsub(id) {
                    return Some(g);
                }
            }
        }
        // Second pass: accept any font with the glyph.
        for &id in &ids {
            if let Some(g) = self.glyph_from_face(id, ch, size_px) {
                return Some(g);
            }
        }
        None
    }

    fn has_gsub(&mut self, id: fontdb::ID) -> bool {
        if !self.bytes_cache.contains_key(&id) {
            if let Some(bytes) = self.db.with_face_data(id, |data, _| Arc::new(data.to_vec())) {
                self.bytes_cache.insert(id, bytes);
            } else {
                return false;
            }
        }
        let face_index = match self.db.face(id) {
            Some(f) => f.index as u32,
            None => return false,
        };
        let bytes = match self.bytes_cache.get(&id) {
            Some(b) => b.clone(),
            None => return false,
        };
        rustybuzz::Face::from_slice(&bytes, face_index)
            .map(|f| f.raw_face().table(rustybuzz::ttf_parser::Tag::from_bytes(b"GSUB")).is_some())
            .unwrap_or(false)
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
        if adv < 0.0 {
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
/// Zero-advance characters (combining diacritics etc.) pass through unchanged.
fn round_to_e(raw: f32, e_width: f32) -> f32 {
    if raw == 0.0 { return 0.0; }
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
        face_index: usize,
    ) -> Option<CachedGlyph> {
        let size_rounded = size_px.round() as u16;
        let key = GlyphKey { font_index, glyph_id, size_px: size_rounded };

        if let Some(&cached) = self.glyphs.get(&key) {
            return cached;
        }

        let font_ref = FontRef::from_index(font_data, face_index)?;
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
                cache.get_or_rasterize(&fb.bytes, fb.font_index, fb.glyph_id, size_px, hint, fb.face_index);
                glyphs.push((fb.glyph_id, adv, Some(fb.bytes), fb.font_index));
            } else {
                // Truly unknown: reserve space, render nothing.
                glyphs.push((0u16, e_width, None, font_index));
            }
        } else {
            let adv = glyph_metrics.advance_width(gid);
            cache.get_or_rasterize(font_data, font_index, gid as u16, size_px, hint, 0);
            glyphs.push((gid, adv, None, font_index));
        }
    }
    glyphs
}

/// Measure the total pixel width of a text string using BiDi shaping.
pub fn measure_text_width(font_data: &'static [u8], text: &str, size_px: f32) -> f32 {
    measure_shaped_width(font_data, text, size_px)
}

// ─── BiDi / shaping types ────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BiDiDir { Ltr, Rtl }

#[derive(Clone, Debug)]
pub struct ShapedGlyphInfo {
    pub glyph_id: u16,
    pub x_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
    /// Byte offset into the run's own substring (run.text_range slice).
    pub cluster: u32,
}

#[derive(Clone, Debug)]
pub struct ShapedRun {
    pub glyphs: Vec<ShapedGlyphInfo>,
    pub direction: BiDiDir,
    /// None = primary font (use caller's font_data); Some = fallback bytes.
    pub font_bytes: Option<Arc<Vec<u8>>>,
    pub font_face_index: usize,
    pub font_index: u8,
    /// Byte range of this run in the original full-line string.
    pub text_range: Range<usize>,
    pub total_advance: f32,
}

// ─── shape_line ──────────────────────────────────────────────────────────────

/// Shape a full line of text using unicode-bidi + rustybuzz.
/// Returns runs in visual left-to-right order.
pub fn shape_line(
    cache: &mut GlyphCache,
    font_data: &'static [u8],
    font_index: u8,
    text: &str,
    size_px: f32,
    hint: bool,
) -> Vec<ShapedRun> {
    if text.is_empty() { return vec![]; }
    shape_line_inner(Some(cache), font_data, font_index, text, size_px, hint)
}

fn shape_line_inner(
    mut cache: Option<&mut GlyphCache>,
    font_data: &'static [u8],
    font_index: u8,
    text: &str,
    size_px: f32,
    hint: bool,
) -> Vec<ShapedRun> {
    use unicode_bidi::BidiInfo;

    let bidi = BidiInfo::new(text, None);
    if bidi.paragraphs.is_empty() { return vec![]; }
    let para = &bidi.paragraphs[0];
    let (run_levels, visual_run_ranges) = bidi.visual_runs(para, para.range.clone());

    let primary_font_ref = match FontRef::from_index(font_data, 0) {
        Some(f) => f,
        None => return vec![],
    };
    let primary_charmap = primary_font_ref.charmap();
    // e_width is the advance of 'e' in the primary font — used to snap fallback advances to grid.
    let e_width = primary_font_ref.glyph_metrics(&[]).scale(size_px)
        .advance_width(primary_charmap.map('e'));

    let mut output: Vec<ShapedRun> = Vec::new();

    // Collect all sub-runs (font-boundary splits within each BiDi run).
    struct RawRun {
        text_range: Range<usize>,
        direction: BiDiDir,
        font_bytes: Option<Arc<Vec<u8>>>,
        font_face_index: usize,
        font_index: u8,
    }

    let mut raw_runs: Vec<RawRun> = Vec::new();

    for vrun_range in &visual_run_ranges {
        // Get the level at the start of this run to determine direction.
        let level = run_levels.get(vrun_range.start).copied()
            .unwrap_or(unicode_bidi::LTR_LEVEL);
        let dir = if level.is_rtl() { BiDiDir::Rtl } else { BiDiDir::Ltr };
        let run_text = &text[vrun_range.clone()];

        // Compute font-boundary sub-runs within this BiDi run.
        // Walk chars in logical order; switch sub-run when font changes.
        let mut sub_runs: Vec<RawRun> = Vec::new();

        let mut sub_start_byte = vrun_range.start; // absolute in `text`
        let mut cur_font_bytes: Option<Arc<Vec<u8>>> = None;
        let mut cur_face_index: usize = 0;
        let mut cur_font_index: u8 = font_index;
        let mut first = true;

        // We need to walk chars with byte offsets.
        let mut char_iter = run_text.char_indices().peekable();
        while let Some((rel_offset, ch)) = char_iter.next() {
            let abs_offset = vrun_range.start + rel_offset;
            let next_abs = abs_offset + ch.len_utf8() as usize;

            let (new_bytes, new_face, new_fidx) = if primary_charmap.map(ch) != 0 {
                (None, 0usize, font_index)
            } else {
                // Fallback font lookup.
                let fb_opt = fallback_db().lock().ok().and_then(|mut db| db.find(ch, size_px));
                match fb_opt {
                    Some(fb) => (Some(fb.bytes), fb.face_index, fb.font_index),
                    None => (None, 0usize, font_index), // use primary even if missing
                }
            };

            let same_font = !first && cur_font_bytes == new_bytes && cur_face_index == new_face && cur_font_index == new_fidx;

            if !first && !same_font {
                // Flush current sub-run.
                sub_runs.push(RawRun {
                    text_range: sub_start_byte..abs_offset,
                    direction: dir,
                    font_bytes: cur_font_bytes.clone(),
                    font_face_index: cur_face_index,
                    font_index: cur_font_index,
                });
                sub_start_byte = abs_offset;
            }

            cur_font_bytes = new_bytes;
            cur_face_index = new_face;
            cur_font_index = new_fidx;
            first = false;

            // On last char, flush.
            if char_iter.peek().is_none() {
                sub_runs.push(RawRun {
                    text_range: sub_start_byte..next_abs,
                    direction: dir,
                    font_bytes: cur_font_bytes.clone(),
                    font_face_index: cur_face_index,
                    font_index: cur_font_index,
                });
            }
        }

        if sub_runs.is_empty() { continue; }

        // RTL: reverse sub-run order for visual output.
        if dir == BiDiDir::Rtl {
            sub_runs.reverse();
        }

        raw_runs.extend(sub_runs);
    }

    // Now shape each raw run and build ShapedRun.
    // We need to call get_or_rasterize which requires &mut GlyphCache.
    // We'll pass cache as Option<&mut GlyphCache> using unsafe transmute to extend lifetime
    // temporarily — actually let's just do it the safe way by using a raw pointer.
    // The safe way: iterate raw_runs, shape each one, collect glyphs,
    // then do a second pass for rasterization.

    for raw in raw_runs {
        let sub_text = &text[raw.text_range.clone()];
        if sub_text.is_empty() { continue; }

        let (fb_data_slice, fb_face_idx): (&[u8], usize) = match &raw.font_bytes {
            Some(b) => (b.as_slice(), raw.font_face_index),
            None => (font_data, 0),
        };

        let rb_face = match rustybuzz::Face::from_slice(fb_data_slice, fb_face_idx as u32) {
            Some(f) => f,
            None => continue,
        };
        let upem = rb_face.units_per_em() as f32;
        let scale = size_px / upem;

        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(sub_text);
        if raw.direction == BiDiDir::Rtl {
            buf.set_direction(rustybuzz::Direction::RightToLeft);
        } else {
            buf.set_direction(rustybuzz::Direction::LeftToRight);
        }

        let shaped = rustybuzz::shape(&rb_face, &[], buf);
        let infos = shaped.glyph_infos();
        let positions = shaped.glyph_positions();

        let mut glyphs: Vec<ShapedGlyphInfo> = Vec::with_capacity(infos.len());
        let mut total_advance = 0.0_f32;

        let is_fallback = raw.font_bytes.is_some();
        for (info, pos) in infos.iter().zip(positions.iter()) {
            let x_adv_raw = pos.x_advance as f32 * scale;
            // Snap advances to the primary-font 'e' grid, same as the old layout_text path.
            // Zero-advance glyphs (combining diacritics) pass through unchanged.
            let x_adv = if is_fallback {
                round_to_e(x_adv_raw, e_width)
            } else {
                x_adv_raw
            };
            let x_off = pos.x_offset as f32 * scale;
            let y_off = pos.y_offset as f32 * scale;
            glyphs.push(ShapedGlyphInfo {
                glyph_id: info.glyph_id as u16,
                x_advance: x_adv,
                x_offset: x_off,
                y_offset: y_off,
                cluster: info.cluster,
            });
            total_advance += x_adv;
        }

        // Rasterize glyphs if we have a cache.
        if let Some(ref mut c) = cache.as_deref_mut() {
            c.ensure_atlas_nocanvas();
            for g in &glyphs {
                c.get_or_rasterize_no_canvas(
                    fb_data_slice, raw.font_index, g.glyph_id, size_px, hint, fb_face_idx,
                );
            }
        }

        output.push(ShapedRun {
            glyphs,
            direction: raw.direction,
            font_bytes: raw.font_bytes,
            font_face_index: raw.font_face_index,
            font_index: raw.font_index,
            text_range: raw.text_range,
            total_advance,
        });
    }

    output
}

// ─── measure_shaped_width ────────────────────────────────────────────────────

/// Measure total pixel width of text using BiDi shaping (no rasterization).
pub fn measure_shaped_width(font_data: &'static [u8], text: &str, size_px: f32) -> f32 {
    if text.is_empty() { return 0.0; }
    let runs = shape_line_inner(None, font_data, 0, text, size_px, false);
    runs.iter().map(|r| r.total_advance).sum()
}

/// Shape a line without a GlyphCache (no rasterization). For cursor math.
pub fn shape_line_no_cache(font_data: &'static [u8], font_index: u8, text: &str, size_px: f32) -> Vec<ShapedRun> {
    if text.is_empty() { return vec![]; }
    shape_line_inner(None, font_data, font_index, text, size_px, false)
}

// ─── col_to_x_in_shaped_line ─────────────────────────────────────────────────

/// Convert a logical char offset to a pixel x within the shaped line.
/// Returned x is relative to pen_x=0 at the start of the line.
pub fn col_to_x_in_shaped_line(runs: &[ShapedRun], col: usize, text: &str) -> f32 {
    let target_byte = text.char_indices().nth(col).map(|(i, _)| i).unwrap_or(text.len());
    let mut pen_x = 0.0_f32;

    for run in runs {
        if target_byte < run.text_range.start || target_byte > run.text_range.end {
            pen_x += run.total_advance;
            continue;
        }
        // target_byte is within this run.
        let run_target = target_byte - run.text_range.start;

        if run.direction == BiDiDir::Ltr {
            for g in &run.glyphs {
                if g.cluster as usize >= run_target {
                    return pen_x;
                }
                pen_x += g.x_advance;
            }
            // target is past all glyphs → end of run
        } else {
            // RTL: glyphs in visual order, clusters decrease.
            // Find the glyph whose cluster == run_target.
            for (i, g) in run.glyphs.iter().enumerate() {
                if g.cluster as usize == run_target {
                    // For RTL, cursor is at the right side of this glyph (logical start = visual right).
                    // Accumulate advances for all glyphs before this one.
                    let mut x = pen_x;
                    for g2 in &run.glyphs[..i] {
                        x += g2.x_advance;
                    }
                    return x + g.x_advance;
                }
            }
            // Not found exactly — find the glyph with the nearest cluster >= run_target in RTL.
            // In RTL, clusters decrease as pen_x increases; find last glyph with cluster > run_target.
            let mut best_x = pen_x + run.total_advance;
            let mut x = pen_x;
            for g in &run.glyphs {
                if g.cluster as usize >= run_target {
                    best_x = x + g.x_advance;
                }
                x += g.x_advance;
            }
            return best_x;
        }
    }
    pen_x
}

// ─── x_to_col_in_shaped_line ─────────────────────────────────────────────────

/// Convert a pixel x coordinate to the closest logical char offset.
pub fn x_to_col_in_shaped_line(runs: &[ShapedRun], target_x: f32, text: &str) -> usize {
    let mut pen_x = 0.0_f32;

    for run in runs {
        if target_x < pen_x || target_x > pen_x + run.total_advance + 1.0 {
            if target_x <= pen_x + run.total_advance {
                // fall through to glyph walk below
            } else {
                pen_x += run.total_advance;
                continue;
            }
        }

        // Walk glyphs in this run.
        let mut glyph_pen = pen_x;
        for (i, g) in run.glyphs.iter().enumerate() {
            let glyph_right = glyph_pen + g.x_advance;
            if target_x < glyph_right || i + 1 == run.glyphs.len() {
                // Click is within this glyph.
                let full_byte = run.text_range.start + g.cluster as usize;
                let left_half = target_x < glyph_pen + g.x_advance / 2.0;

                if run.direction == BiDiDir::Ltr {
                    if left_half {
                        return byte_offset_to_col(text, full_byte);
                    } else {
                        // Next char: find next glyph's cluster or end of run.
                        let next_byte = if i + 1 < run.glyphs.len() {
                            run.text_range.start + run.glyphs[i + 1].cluster as usize
                        } else {
                            run.text_range.end
                        };
                        return byte_offset_to_col(text, next_byte);
                    }
                } else {
                    // RTL: right half → char at cluster; left half → "next" logical (lower cluster = prev glyph).
                    if !left_half {
                        return byte_offset_to_col(text, full_byte);
                    } else {
                        let prev_byte = if i > 0 {
                            run.text_range.start + run.glyphs[i - 1].cluster as usize
                        } else {
                            run.text_range.end
                        };
                        return byte_offset_to_col(text, prev_byte);
                    }
                }
            }
            glyph_pen += g.x_advance;
        }
        pen_x += run.total_advance;
    }

    // Click is past all runs — return char count.
    text.chars().count()
}

fn byte_offset_to_col(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].chars().count()
}

// ─── GlyphCache helpers for shape_line ───────────────────────────────────────

impl GlyphCache {
    /// Ensure atlas bytes are allocated (no canvas needed, canvas upload happens at flush).
    pub(crate) fn ensure_atlas_nocanvas(&mut self) {
        // atlas_data is always allocated in new(); nothing to do.
    }

    /// Rasterize a glyph and store in atlas without touching the femtovg canvas.
    /// The atlas texture is uploaded later via flush().
    pub(crate) fn get_or_rasterize_no_canvas(
        &mut self,
        font_data: &[u8],
        font_index: u8,
        glyph_id: u16,
        size_px: f32,
        hint: bool,
        face_index: usize,
    ) {
        let size_rounded = size_px.round() as u16;
        let key = GlyphKey { font_index, glyph_id, size_px: size_rounded };
        if self.glyphs.contains_key(&key) { return; }

        let result = (|| -> Option<CachedGlyph> {
            let font_ref = FontRef::from_index(font_data, face_index)?;
            let mut scaler = self.scale_ctx
                .builder(font_ref)
                .size(size_rounded as f32)
                .hint(hint)
                .build();
            let image = Render::new(&[Source::Outline])
                .format(Format::Alpha)
                .render(&mut scaler, glyph_id)?;
            if image.content != Content::Mask || image.data.is_empty() { return None; }
            let p = image.placement;
            let w = p.width as usize;
            let h = p.height as usize;
            if w == 0 || h == 0 { return None; }
            let (ax, ay) = self.alloc_shelf(w, h)?;
            let exp = 1.0_f32 / 1.5;
            for row in 0..h {
                for col in 0..w {
                    let a = image.data[row * w + col];
                    let mut af = a as f32 / 255.0;
                    af = af.powf(exp) * 255.0;
                    let a = af.round() as u8;
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
                advance: 0.0,
            })
        })();
        self.glyphs.insert(key, result);
    }
}
