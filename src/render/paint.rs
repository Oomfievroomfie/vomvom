// Paint pass — walks LayoutBox tree and issues femtovg draw calls.

use std::collections::HashMap;
use femtovg::{Canvas, renderer::OpenGl, Paint, Path, FontId};
use crate::render::style::{Color, Display, Overflow};
use crate::render::tree::{Node, NodeContent};
use crate::render::layout::{LayoutBox, Rect};
use crate::render::glyph_cache::{GlyphCache, shape_line};

pub struct PaintContext<'a> {
    pub canvas: &'a mut Canvas<OpenGl>,
    pub glyph_cache: &'a mut GlyphCache,
    pub sans_data: &'static [u8],
    pub mono_data: &'static [u8],
    pub hint: bool,
    pub use_femtovg: bool,
    pub femtovg_fonts: Option<(FontId, FontId)>,
    pub fallback_femtovg_cache: HashMap<(usize, u32), FontId>,
    pub grid_snap: bool,
}

impl<'a> PaintContext<'a> {
    fn font_data_for(&self, family: &str) -> (&'static [u8], u8) {
        if family == "monospace" { (self.mono_data, 1) } else { (self.sans_data, 0) }
    }

    /// Draw a full logical line using shaped runs.
    /// `line_node` children are spans; `line_lb` is the line's layout box.
    fn draw_line(&mut self, line_lb: &LayoutBox, line_node: &Node, font_size: f32, family: &str) {
        // Concatenate all span texts.
        let mut full_text = String::new();
        let mut span_starts: Vec<usize> = Vec::new();
        let mut span_colors: Vec<Color> = Vec::new();
        for span_node in line_node.children() {
            span_starts.push(full_text.len());
            full_text.push_str(span_text_of_node(span_node));
            span_colors.push(span_node.style.color);
        }
        if full_text.is_empty() { return; }

        let (font_data, font_index) = self.font_data_for(family);

        self.glyph_cache.ensure_atlas(self.canvas);
        let runs = shape_line(self.glyph_cache, font_data, font_index, &full_text, font_size, self.hint, self.grid_snap);
        self.glyph_cache.flush(self.canvas);

        let atlas_id = match self.glyph_cache.atlas {
            Some(id) => id,
            None => return,
        };
        let atlas_f = crate::render::glyph_cache::ATLAS_SIZE as f32;

        // Determine the x origin of the line (first span's border_box.x).
        let line_origin_x = line_lb.children.first().map_or(line_lb.border_box.x, |s| s.border_box.x);
        let baseline_y = line_lb.border_box.y + font_size;
        let mut pen_x = 0.0_f32;

        for run in &runs {
            let font_data_for_run: &[u8] = run.font_bytes.as_deref()
                .map(|b| b.as_slice())
                .unwrap_or(font_data);

            for g in &run.glyphs {
                // Look up color per glyph using its absolute byte position in full_text.
                let abs_byte = run.text_range.start + g.cluster as usize;
                let si = span_starts.partition_point(|&s| s <= abs_byte).saturating_sub(1).min(span_starts.len().saturating_sub(1));
                let color = span_colors.get(si).copied().unwrap_or(Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 });
                let tint = femtovg::Color::rgbaf(color.r, color.g, color.b, color.a);

                if let Some(cg) = self.glyph_cache.get_cached(g.glyph_id, run.font_index, font_size) {
                    if cg.width > 0 && cg.height > 0 {
                        let gx = (line_origin_x + pen_x + g.x_offset + cg.bearing_x as f32).round();
                        let gy = (baseline_y - g.y_offset - cg.bearing_y as f32).round();
                        let gw = cg.width as f32;
                        let gh = cg.height as f32;
                        let cx = gx - cg.atlas_x as f32;
                        let cy = gy - cg.atlas_y as f32;
                        let paint = Paint::image_tint(atlas_id, cx, cy, atlas_f, atlas_f, 0.0, tint)
                            .with_anti_alias(false);
                        let mut path = Path::new();
                        path.rect(gx, gy, gw, gh);
                        self.canvas.fill_path(&path, &paint);
                    }
                }
                let _ = font_data_for_run; // used implicitly via glyph_cache key
                pen_x += g.x_advance;
            }
        }
    }

    fn draw_text(&mut self, x: f32, y: f32, text: &str, color: Color, font_size: f32, family: &str) {
        if text.is_empty() { return; }
        // Fallback: draw a single span's text as its own shaped line.
        // Used for non-editor text nodes (labels, menus, etc.).
        let (font_data, font_index) = self.font_data_for(family);

        self.glyph_cache.ensure_atlas(self.canvas);
        let runs = shape_line(self.glyph_cache, font_data, font_index, text, font_size, self.hint, self.grid_snap);
        self.glyph_cache.flush(self.canvas);

        let atlas_id = match self.glyph_cache.atlas { Some(id) => id, None => return };
        let atlas_f = crate::render::glyph_cache::ATLAS_SIZE as f32;

        let mut pen_x = x;
        let tint = femtovg::Color::rgbaf(color.r, color.g, color.b, color.a);

        for run in &runs {
            let _font_data_for_run: &[u8] = run.font_bytes.as_deref()
                .map(|b| b.as_slice())
                .unwrap_or(font_data);
            for g in &run.glyphs {
                if let Some(cg) = self.glyph_cache.get_cached(g.glyph_id, run.font_index, font_size) {
                    if cg.width > 0 && cg.height > 0 {
                        let gx = (pen_x + g.x_offset + cg.bearing_x as f32).round();
                        let gy = (y - g.y_offset - cg.bearing_y as f32).round();
                        let gw = cg.width as f32;
                        let gh = cg.height as f32;
                        let cx = gx - cg.atlas_x as f32;
                        let cy = gy - cg.atlas_y as f32;
                        let paint = Paint::image_tint(atlas_id, cx, cy, atlas_f, atlas_f, 0.0, tint)
                            .with_anti_alias(false);
                        let mut path = Path::new();
                        path.rect(gx, gy, gw, gh);
                        self.canvas.fill_path(&path, &paint);
                    }
                }
                pen_x += g.x_advance;
            }
        }
    }
}

fn span_text_of_node(span: &Node) -> &str {
    span.children().first()
        .and_then(|n| if let NodeContent::Text(t) = &n.content { Some(t.as_str()) } else { None })
        .unwrap_or("")
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

    // For editor line nodes (class="line" or "cursor-line"), draw all spans as one shaped line.
    if node.has_class("line") || node.has_class("cursor-line") {
        ctx.draw_line(lb, node, s.font_size, &s.font_family);
        ctx.canvas.restore();
        return;
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
