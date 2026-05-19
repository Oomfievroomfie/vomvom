// Paint pass — walks LayoutBox tree and issues femtovg draw calls.

use std::collections::HashMap;
use std::sync::Arc;
use femtovg::{Canvas, renderer::OpenGl, Paint, Path, FontId};
use crate::render::style::{Color, Display, Overflow};
use crate::render::tree::{Node, NodeContent};
use crate::render::layout::{LayoutBox, Rect};
use crate::render::glyph_cache::{GlyphCache, layout_text, os_font_for_char};
use swash;

pub struct PaintContext<'a> {
    pub canvas: &'a mut Canvas<OpenGl>,
    pub glyph_cache: &'a mut GlyphCache,
    pub sans_data: &'static [u8],
    pub mono_data: &'static [u8],
    pub hint: bool,
    pub use_femtovg: bool,
    pub femtovg_fonts: Option<(FontId, FontId)>, // (sans, mono)
    // Cache of dynamically loaded fallback FontIds keyed by (Arc ptr, face_index).
    pub fallback_femtovg_cache: HashMap<(usize, u32), FontId>,
}

impl<'a> PaintContext<'a> {
    fn font_data_for(&self, family: &str) -> (&'static [u8], u8) {
        if family == "monospace" { (self.mono_data, 1) } else { (self.sans_data, 0) }
    }

    fn font_id_for(&self, family: &str) -> Option<FontId> {
        let (sans, mono) = self.femtovg_fonts?;
        if family == "monospace" { Some(mono) } else { Some(sans) }
    }

    fn draw_text_femtovg(&mut self, x: f32, y: f32, text: &str, color: Color, font_size: f32, family: &str) {
        let Some(primary_font_id) = self.font_id_for(family) else { return };
        let tint = color.to_femtovg();

        let primary_data = if family == "monospace" { self.mono_data } else { self.sans_data };
        let font_ref = match swash::FontRef::from_index(primary_data, 0) {
            Some(f) => f,
            None => return,
        };
        let charmap = font_ref.charmap();
        let metrics = font_ref.glyph_metrics(&[]).scale(font_size);
        let e_width = metrics.advance_width(charmap.map('e'));

        // Draw primary-font chars in batched runs for efficiency.
        // Fallback-font chars are drawn one at a time at our computed pen_x so that
        // our rounded advance widths control positioning, not femtovg's internal shaping.
        let mut pen_x = x;
        let mut primary_run_start_x = x;
        let mut primary_run = String::new();

        let flush_primary = |canvas: &mut Canvas<OpenGl>, run: &mut String, rx: f32| {
            if run.is_empty() { return; }
            let mut paint = Paint::color(tint);
            paint.set_font(&[primary_font_id]);
            paint.set_font_size(font_size);
            let _ = canvas.fill_text(rx, y, run.as_str(), &paint);
            run.clear();
        };

        for ch in text.chars() {
            let gid = charmap.map(ch);
            if gid != 0 {
                // Primary font — batch into run.
                let adv = metrics.advance_width(gid);
                primary_run.push(ch);
                pen_x += adv;
            } else {
                // Fallback font — flush primary run first, then draw this char individually.
                flush_primary(self.canvas, &mut primary_run, primary_run_start_x);
                primary_run_start_x = pen_x; // will be updated below

                let fb = os_font_for_char(ch);
                let fb_font_id = match fb {
                    Some((ref bytes, face_idx)) => {
                        let key = (Arc::as_ptr(bytes) as usize, face_idx);
                        *self.fallback_femtovg_cache.entry(key).or_insert_with(|| {
                            self.canvas.add_font_mem(bytes).unwrap_or(primary_font_id)
                        })
                    }
                    None => primary_font_id,
                };
                let raw_adv = fb.as_ref().and_then(|(bytes, fi)| {
                    swash::FontRef::from_index(bytes.as_ref(), *fi as usize).map(|fr| {
                        let gid2 = fr.charmap().map(ch);
                        fr.glyph_metrics(&[]).scale(font_size).advance_width(gid2)
                    })
                }).unwrap_or(e_width);
                let adv = (raw_adv / e_width).round().max(1.0) * e_width;

                // Draw single fallback char at our pen_x.
                let mut paint = Paint::color(tint);
                paint.set_font(&[fb_font_id]);
                paint.set_font_size(font_size);
                let _ = self.canvas.fill_text(pen_x, y, &ch.to_string(), &paint);
                pen_x += adv;
                primary_run_start_x = pen_x;
            }
        }
        flush_primary(self.canvas, &mut primary_run, primary_run_start_x);
    }

    fn draw_text(&mut self, x: f32, y: f32, text: &str, color: Color, font_size: f32, family: &str) {
        if text.is_empty() { return; }
        if self.use_femtovg {
            self.draw_text_femtovg(x, y, text, color, font_size, family);
            return;
        }
        let (font_data, font_index) = self.font_data_for(family);

        self.glyph_cache.ensure_atlas(self.canvas);
        let glyphs = layout_text(self.glyph_cache, font_data, font_index, text, font_size, self.hint);
        self.glyph_cache.flush(self.canvas);

        let atlas_id = match self.glyph_cache.atlas {
            Some(id) => id,
            None => return,
        };
        let atlas_f = crate::render::glyph_cache::ATLAS_SIZE as f32;

        //let mut pen_x = x.round();
        let mut pen_x = x;
        let tint = femtovg::Color::rgbaf(color.r, color.g, color.b, color.a);

        for (glyph_id, advance, _fallback_bytes, glyph_font_index) in &glyphs {
            if let Some(g) = self.glyph_cache.get_cached(*glyph_id, *glyph_font_index, font_size) {
                if g.width > 0 && g.height > 0 {
                    let gx = (pen_x + g.bearing_x as f32).round();
                    let gy = (y - g.bearing_y as f32).round();
                    let gw = g.width as f32;
                    let gh = g.height as f32;

                    // Paint::image_tint(id, cx, cy, img_w, img_h, angle, tint):
                    //   cx, cy = world coords of the atlas image's top-left corner
                    //   img_w, img_h = full atlas size in world coords
                    // The paint samples tex coords proportional to position within [cx..cx+img_w, cy..cy+img_h].
                    // To map glyph at atlas (atlas_x, atlas_y) to screen (gx, gy):
                    //   cx = gx - atlas_x, cy = gy - atlas_y
                    let cx = gx - g.atlas_x as f32;
                    let cy = gy - g.atlas_y as f32;

                    // Atlas stores premultiplied white glyphs; image_tint multiplies by text color.
                    let paint = Paint::image_tint(atlas_id, cx, cy, atlas_f, atlas_f, 0.0, tint)
                        .with_anti_alias(false);
                    let mut path = Path::new();
                    path.rect(gx, gy, gw, gh);
                    self.canvas.fill_path(&path, &paint);
                }
            }
            pen_x += advance;
        }
    }
}

/// Paint the full tree: normal pass then a global overlay for all absolutely-positioned nodes.
/// Absolute nodes are collected across the entire tree (depth-first) and painted last, sorted
/// by z-index, so they always appear above all non-absolute content regardless of tree depth.
pub fn paint_tree_root(ctx: &mut PaintContext, node: &Node, lb: &LayoutBox) {
    paint_tree(ctx, node, lb);
    let mut overlay: Vec<(&Node, &LayoutBox)> = Vec::new();
    collect_absolute_overlay(node, lb, &mut overlay);
    overlay.sort_by_key(|(n, _)| n.style.z_index);
    for (abs_node, abs_lb) in overlay {
        paint_tree(ctx, abs_node, abs_lb);
    }
}

fn collect_absolute_overlay<'a>(node: &'a Node, lb: &'a LayoutBox, out: &mut Vec<(&'a Node, &'a LayoutBox)>) {
    use crate::render::style::Position;
    for (i, child_node) in node.children().iter().enumerate() {
        let Some(child_lb) = lb.children.get(i) else { continue };
        if child_node.style.position == Position::Absolute {
            out.push((child_node, child_lb));
            collect_absolute_overlay(child_node, child_lb, out);
        } else {
            collect_absolute_overlay(child_node, child_lb, out);
        }
    }
}

pub fn paint_tree(ctx: &mut PaintContext, node: &Node, lb: &LayoutBox) {
    if node.style.display == Display::None {
        return;
    }

    let s = &node.style;
    let opacity = s.opacity;

    ctx.canvas.save();
    ctx.canvas.set_global_alpha(opacity);

    // Clip to border-box if overflow is hidden
    let clip = s.overflow_x == Overflow::Hidden || s.overflow_y == Overflow::Hidden;
    if clip {
        let mut clip_path = Path::new();
        rounded_rect(&mut clip_path, lb.border_box, s.border.radius.top_left);
        ctx.canvas.scissor(lb.border_box.x, lb.border_box.y, lb.border_box.w, lb.border_box.h);
    }

    // Background
    if s.background_color.a > 0.0 {
        let mut path = Path::new();
        if s.border.radius.is_zero() {
            path.rect(lb.border_box.x, lb.border_box.y, lb.border_box.w, lb.border_box.h);
        } else {
            rounded_rect(&mut path, lb.border_box, s.border.radius.top_left);
        }
        let paint = Paint::color(s.background_color.to_femtovg());
        ctx.canvas.fill_path(&path, &paint);
    }

    // Border
    if s.border.width > 0.0 && s.border.color.a > 0.0 {
        let hw = s.border.width / 2.0;
        let br = Rect::new(
            lb.border_box.x + hw,
            lb.border_box.y + hw,
            lb.border_box.w - s.border.width,
            lb.border_box.h - s.border.width,
        );
        let mut path = Path::new();
        if s.border.radius.is_zero() {
            path.rect(br.x, br.y, br.w, br.h);
        } else {
            rounded_rect(&mut path, br, (s.border.radius.top_left - hw).max(0.0));
        }
        let mut paint = Paint::color(s.border.color.to_femtovg());
        paint.set_line_width(s.border.width);
        ctx.canvas.stroke_path(&path, &paint);
    }

    // Text content
    if let NodeContent::Text(text) = &node.content {
        if !text.is_empty() {
            ctx.draw_text(
                lb.border_box.x,
                lb.border_box.y + s.font_size,
                text,
                s.color,
                s.font_size,
                &s.font_family,
            );
        }
    }

    // Children (sorted by z-index), skipping absolutely-positioned ones (painted in overlay pass).
    use crate::render::style::Position;
    let mut order: Vec<usize> = (0..lb.children.len())
        .filter(|&i| node.children()[i].style.position != Position::Absolute)
        .collect();
    order.sort_by_key(|&i| lb.children[i].z_index);

    for i in order {
        let child_node = &node.children()[i];
        let child_lb = &lb.children[i];
        paint_tree(ctx, child_node, child_lb);
    }

    ctx.canvas.restore();
}

fn rounded_rect(path: &mut Path, r: Rect, radius: f32) {
    path.rounded_rect(r.x, r.y, r.w, r.h, radius);
}

/// Draw a red 1px stroke rect outline.
fn debug_rect(canvas: &mut Canvas<OpenGl>, r: Rect) {
    if r.w <= 0.0 || r.h <= 0.0 { return; }
    let mut path = Path::new();
    path.rect(r.x, r.y, r.w, r.h);
    let paint = Paint::color(femtovg::Color::rgba(255, 0, 0, 200));
    canvas.stroke_path(&path, &paint);
}

/// Recursively draw red border_box outlines over every layout box.
pub fn paint_debug_boxes(canvas: &mut Canvas<OpenGl>, lb: &LayoutBox) {
    debug_rect(canvas, lb.border_box);
    for child in &lb.children {
        paint_debug_boxes(canvas, child);
    }
}
