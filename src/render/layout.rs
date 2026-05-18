// Layout engine — block, inline, flex
//
// Coordinate model:
//   layout() returns boxes in LOCAL space — border_box.x/y are relative to the
//   parent's content-area origin (i.e. 0,0 means top-left of parent content).
//   finalize_positions() does a single post-pass to convert everything to
//   absolute screen coords by recursively adding parent content origins.

use crate::render::style::{Display, FlexDirection, AlignItems, JustifyContent, Length, Position};
use crate::render::tree::{Node, NodeContent};

pub trait TextMeasurer {
    fn measure_width(&mut self, text: &str, font_size: f32, font_family: &str) -> f32;
}

/// Fallback measurer: approximate using character count × font_size × 0.6
pub struct ApproxMeasurer;
impl TextMeasurer for ApproxMeasurer {
    fn measure_width(&mut self, text: &str, font_size: f32, _font_family: &str) -> f32 {
        text.chars().count() as f32 * font_size * 0.6
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self { Rect { x, y, w, h } }
    pub fn right(&self) -> f32 { self.x + self.w }
    pub fn bottom(&self) -> f32 { self.y + self.h }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
}

/// Output of layout for one node.
#[derive(Debug, Clone)]
pub struct LayoutBox {
    /// Content area in local space (inside padding+border), relative to parent content origin.
    pub content: Rect,
    /// Border box in local space, relative to parent content origin.
    pub border_box: Rect,
    /// Scroll offset applied to children.
    pub scroll_x: f32,
    pub scroll_y: f32,
    /// Children in local space (relative to this node's content origin).
    pub children: Vec<LayoutBox>,
    pub z_index: i32,
}

impl LayoutBox {
    fn new(border_box: Rect, content: Rect, children: Vec<LayoutBox>, z_index: i32) -> Self {
        LayoutBox { content, border_box, scroll_x: 0.0, scroll_y: 0.0, children, z_index }
    }
    fn leaf(border_box: Rect, content: Rect) -> Self {
        Self::new(border_box, content, vec![], 0)
    }
}

/// Constraints passed down during layout.
#[derive(Debug, Clone, Copy)]
pub struct Constraints {
    pub available_w: f32,
    pub available_h: f32,
}

impl Constraints {
    pub fn new(w: f32, h: f32) -> Self { Constraints { available_w: w, available_h: h } }
}

/// Layout a node tree. Returns boxes in local (parent-content-relative) space.
/// Call finalize_positions() on the root afterwards to get absolute screen coords.
pub fn layout(node: &Node, constraints: Constraints, measurer: &mut dyn TextMeasurer) -> LayoutBox {
    match &node.content {
        NodeContent::Text(_) => layout_text(node, constraints, measurer),
        NodeContent::Element { .. } => layout_element(node, constraints, measurer),
    }
}

fn layout_text(node: &Node, constraints: Constraints, measurer: &mut dyn TextMeasurer) -> LayoutBox {
    let s = &node.style;
    let h = s.font_size * s.line_height;
    let text = match &node.content { NodeContent::Text(t) => t.as_str(), _ => "" };
    let w = measurer.measure_width(text, s.font_size, &s.font_family)
        .min(constraints.available_w.max(0.0).min(1_000_000.0));
    let r = Rect::new(0.0, 0.0, w, h);
    LayoutBox::leaf(r, r)
}

fn resolve_length(len: Length, parent: f32) -> Option<f32> {
    match len {
        Length::Auto => None,
        Length::Zero => Some(0.0),
        Length::Px(v) => Some(v),
        Length::Percent(v) => Some(parent * v / 100.0),
    }
}

fn clamp_size(v: f32, min: Length, max: Length, parent: f32) -> f32 {
    let min_v = resolve_length(min, parent).unwrap_or(0.0);
    let max_v = resolve_length(max, parent).unwrap_or(f32::INFINITY);
    v.max(min_v).min(max_v)
}

fn layout_element(node: &Node, constraints: Constraints, measurer: &mut dyn TextMeasurer) -> LayoutBox {
    let s = &node.style;

    if s.display == Display::None {
        return LayoutBox::leaf(Rect::default(), Rect::default());
    }

    let bw = s.border.width;
    let pt = s.padding.top.resolve(constraints.available_h);
    let pr = s.padding.right.resolve(constraints.available_w);
    let pb = s.padding.bottom.resolve(constraints.available_h);
    let pl = s.padding.left.resolve(constraints.available_w);

    let ml = s.margin.left.resolve(constraints.available_w);
    let mr = s.margin.right.resolve(constraints.available_w);
    let mt = s.margin.top.resolve(constraints.available_h);
    let mb = s.margin.bottom.resolve(constraints.available_h);

    let chrome_x = pl + pr + bw * 2.0;
    let chrome_y = pt + pb + bw * 2.0;

    // Block fills available width; inline-block/flex shrink-wraps when width is auto.
    let shrink_wrap = matches!(s.display, Display::InlineBlock | Display::Inline)
        && s.width == Length::Auto;
    let fill_w = constraints.available_w.min(1_000_000.0); // don't fill infinite space
    let inner_w = resolve_length(s.width, constraints.available_w)
        .unwrap_or(if shrink_wrap { 0.0 } else { fill_w - ml - mr - chrome_x })
        .max(0.0);
    let inner_w = clamp_size(inner_w, s.min_width, s.max_width, constraints.available_w);

    // Shrink-wrap elements pass infinite width to children so children can report natural size.
    let child_w = if shrink_wrap { constraints.available_w } else { inner_w };
    let content_constraints = Constraints::new(child_w, constraints.available_h - chrome_y);

    // Children are laid out in local space relative to this node's content rect.
    // Absolute children get placeholder slots; layout_absolute_children fills them in.
    let mut children = match s.display {
        Display::Flex => {
            let in_flow = layout_flex(node, content_constraints, s.flex_direction, s.align_items, s.justify_content, s.gap, measurer);
            // Re-merge: insert placeholder slots for absolute children at their original indices.
            let mut result = Vec::with_capacity(node.children().len());
            let mut in_flow_iter = in_flow.into_iter();
            for child in node.children() {
                if child.style.position == Position::Absolute {
                    result.push(LayoutBox::leaf(Rect::default(), Rect::default()));
                } else {
                    result.push(in_flow_iter.next().unwrap_or_else(|| LayoutBox::leaf(Rect::default(), Rect::default())));
                }
            }
            result
        }
        _ => layout_block_children(node, content_constraints, measurer),
    };

    // Height: explicit or shrink-wrap in-flow children (absolute children don't contribute).
    let inner_h = resolve_length(s.height, constraints.available_h)
        .unwrap_or_else(|| {
            node.children().iter().zip(children.iter())
                .filter(|(n, _)| n.style.position != Position::Absolute)
                .fold(0.0f32, |acc, (_, cb)| (cb.border_box.y + cb.border_box.h).max(acc))
        });
    let inner_h = clamp_size(inner_h, s.min_height, s.max_height, constraints.available_h);

    // Absolute children use the constrained viewport height, not the content-scroll height.
    let abs_containing_h = if s.height == Length::Auto && !shrink_wrap {
        (constraints.available_h - chrome_y).max(0.0)
    } else {
        inner_h
    };
    layout_absolute_children(node, inner_w, abs_containing_h, &mut children, measurer);

    // For shrink-wrap elements, width comes from in-flow children extents only.
    let inner_w = if shrink_wrap {
        let children_w = node.children().iter().zip(children.iter())
            .filter(|(n, _)| n.style.position != Position::Absolute)
            .fold(0.0f32, |acc, (_, cb)| (cb.border_box.x + cb.border_box.w).max(acc));
        clamp_size(children_w, s.min_width, s.max_width, constraints.available_w)
    } else {
        inner_w
    };

    // Border box is at (ml, mt) in parent-content space.
    let bx = ml;
    let by = mt;
    let border_box = Rect::new(bx, by, inner_w + chrome_x, inner_h + chrome_y);
    // Content area is inset by border+padding.
    let content = Rect::new(bx + pl + bw, by + pt + bw, inner_w, inner_h);

    LayoutBox::new(border_box, content, children, s.z_index)
}

fn layout_block_children(node: &Node, constraints: Constraints, measurer: &mut dyn TextMeasurer) -> Vec<LayoutBox> {
    let mut cursor_y = 0.0;
    let mut result = Vec::new();

    for child in node.children() {
        let cs = &child.style;
        if cs.position == Position::Absolute {
            result.push(LayoutBox::leaf(Rect::default(), Rect::default())); // placeholder
            continue;
        }
        let mb = cs.margin.bottom.resolve(constraints.available_h);

        let mut lb = layout(child, constraints, measurer);
        lb.border_box.y += cursor_y;
        lb.content.y += cursor_y;
        cursor_y = lb.border_box.y + lb.border_box.h + mb;
        result.push(lb);
    }

    result
}

fn layout_absolute_children(node: &Node, containing_w: f32, containing_h: f32, children_lbs: &mut Vec<LayoutBox>, measurer: &mut dyn TextMeasurer) {
    for (i, child) in node.children().iter().enumerate() {
        let cs = &child.style;
        if cs.position != Position::Absolute { continue; }

        let avail_w = resolve_length(cs.width, containing_w).unwrap_or(containing_w);
        let avail_h = resolve_length(cs.height, containing_h).unwrap_or(containing_h);
        let mut lb = layout(child, Constraints::new(avail_w, avail_h), measurer);

        let x = resolve_length(cs.left, containing_w)
            .or_else(|| resolve_length(cs.right, containing_w).map(|r| containing_w - lb.border_box.w - r))
            .unwrap_or(0.0);
        let y = resolve_length(cs.top, containing_h)
            .or_else(|| resolve_length(cs.bottom, containing_h).map(|b| containing_h - lb.border_box.h - b))
            .unwrap_or(0.0);

        lb.border_box.x = x;
        lb.border_box.y = y;
        lb.content.x = x + (lb.content.x - lb.border_box.x.min(lb.content.x));
        lb.content.y = y + (lb.content.y - lb.border_box.y.min(lb.content.y));

        // Recalculate content offset from border_box properly.
        let bw = cs.border.width;
        let pl = cs.padding.left.resolve(containing_w);
        let pt = cs.padding.top.resolve(containing_h);
        lb.content.x = x + pl + bw;
        lb.content.y = y + pt + bw;

        children_lbs[i] = lb;
    }
}

fn layout_flex(
    node: &Node,
    constraints: Constraints,
    direction: FlexDirection,
    align: AlignItems,
    justify: JustifyContent,
    gap: f32,
    measurer: &mut dyn TextMeasurer,
) -> Vec<LayoutBox> {
    // Absolute children are laid out separately by layout_element; skip them here.
    let children: Vec<&Node> = node.children().iter()
        .filter(|c| c.style.position != Position::Absolute)
        .collect();
    if children.is_empty() {
        return vec![];
    }

    let is_row = direction == FlexDirection::Row;
    let main_avail = if is_row { constraints.available_w } else { constraints.available_h };
    let cross_avail = if is_row { constraints.available_h } else { constraints.available_w };

    // First pass: measure natural sizes.
    let mut natural: Vec<f32> = Vec::with_capacity(children.len());
    let mut grow_sum = 0.0f32;
    let mut shrink_sum = 0.0f32;
    let mut fixed_total = 0.0f32;

    for child in &children {
        let cs = &child.style;
        // Measure each child's border-box size on the main axis.
        let basis = {
            let explicit_len = match cs.flex_basis {
                Length::Auto => if is_row { cs.width } else { cs.height },
                other => other,
            };
            match explicit_len {
                Length::Auto => {
                    // No explicit size. Items with flex_grow use basis=0.
                    // Items with no grow get intrinsic border-box size.
                    if cs.flex_grow > 0.0 {
                        0.0
                    } else {
                        let intrinsic = layout(*child, Constraints::new(f32::INFINITY, f32::INFINITY), measurer);
                        if is_row { intrinsic.border_box.w } else { intrinsic.border_box.h }
                    }
                }
                other => {
                    // Explicit content size — add padding+border to get border-box size.
                    let bw = cs.border.width * 2.0;
                    let (chrome, avail) = if is_row {
                        let p = cs.padding.left.resolve(main_avail) + cs.padding.right.resolve(main_avail);
                        (p + bw, main_avail)
                    } else {
                        let p = cs.padding.top.resolve(main_avail) + cs.padding.bottom.resolve(main_avail);
                        (p + bw, main_avail)
                    };
                    resolve_length(other, avail).unwrap_or(0.0) + chrome
                }
            }
        };
        let margin_main = if is_row {
            cs.margin.left.resolve(constraints.available_w) + cs.margin.right.resolve(constraints.available_w)
        } else {
            cs.margin.top.resolve(constraints.available_h) + cs.margin.bottom.resolve(constraints.available_h)
        };
        let slot = basis + margin_main;
        natural.push(slot);
        fixed_total += slot;
        grow_sum += cs.flex_grow;
        shrink_sum += cs.flex_shrink;
    }

    let gap_total = gap * (children.len() as f32 - 1.0).max(0.0);
    let free = main_avail - fixed_total - gap_total;

    let mut sizes: Vec<f32> = natural.clone();
    if free > 0.0 && grow_sum > 0.0 {
        for (i, child) in children.iter().enumerate() {
            sizes[i] += free * (child.style.flex_grow / grow_sum);
        }
    } else if free < 0.0 && shrink_sum > 0.0 {
        for (i, child) in children.iter().enumerate() {
            sizes[i] += free * (child.style.flex_shrink / shrink_sum);
        }
    }

    let total_size: f32 = sizes.iter().sum::<f32>() + gap_total;
    let start_offset = match justify {
        JustifyContent::Start => 0.0,
        JustifyContent::End => (main_avail - total_size).max(0.0),
        JustifyContent::Center => ((main_avail - total_size) / 2.0).max(0.0),
        JustifyContent::SpaceBetween | JustifyContent::SpaceAround => 0.0,
    };
    let extra_gap = match justify {
        JustifyContent::SpaceBetween if children.len() > 1 => {
            (main_avail - total_size).max(0.0) / (children.len() as f32 - 1.0)
        }
        JustifyContent::SpaceAround => {
            (main_avail - total_size).max(0.0) / children.len() as f32
        }
        _ => 0.0,
    };
    let around_offset = if matches!(justify, JustifyContent::SpaceAround) { extra_gap / 2.0 } else { 0.0 };

    let mut cursor = start_offset + around_offset;
    let mut result = Vec::with_capacity(children.len());

    for (i, child) in children.iter().enumerate() {
        let cs = &child.style;
        let main_size = sizes[i].max(0.0);

        let child_constraints = if is_row {
            let ml = cs.margin.left.resolve(constraints.available_w);
            let mr = cs.margin.right.resolve(constraints.available_w);
            Constraints::new((main_size - ml - mr).max(0.0), cross_avail)
        } else {
            let mt = cs.margin.top.resolve(constraints.available_h);
            let mb = cs.margin.bottom.resolve(constraints.available_h);
            Constraints::new(cross_avail, (main_size - mt - mb).max(0.0))
        };

        let mut lb = layout(child, child_constraints, measurer);

        // Position on main axis. border_box already includes margin offset from layout_element,
        // so we only add the cursor position here.
        if is_row {
            lb.border_box.x += cursor;
            lb.content.x += cursor;
        } else {
            lb.border_box.y += cursor;
            lb.content.y += cursor;
        }

        // Cross-axis alignment.
        let cross_size = if is_row { lb.border_box.h } else { lb.border_box.w };
        let cross_offset = match align {
            AlignItems::Start => 0.0,
            AlignItems::End => (cross_avail - cross_size).max(0.0),
            AlignItems::Center => ((cross_avail - cross_size) / 2.0).max(0.0),
            AlignItems::Stretch => 0.0,
        };
        if is_row {
            lb.border_box.y += cross_offset;
            lb.content.y += cross_offset;
        } else {
            lb.border_box.x += cross_offset;
            lb.content.x += cross_offset;
        }

        cursor += main_size + gap + extra_gap;
        result.push(lb);
    }

    result
}

/// Translate a single box (not its children) by an offset.
fn translate_one(lb: &mut LayoutBox, dx: f32, dy: f32) {
    lb.border_box.x += dx;
    lb.border_box.y += dy;
    lb.content.x += dx;
    lb.content.y += dy;
}

/// Recursively translate a box and all descendants.
pub fn translate(lb: &mut LayoutBox, dx: f32, dy: f32) {
    translate_one(lb, dx, dy);
    for child in &mut lb.children {
        translate(child, dx, dy);
    }
}

/// Convert local-space layout to absolute screen coords.
/// Each node's children are in local space relative to the node's content origin.
/// This walk translates each child by its parent's absolute content position,
/// converting the whole tree to absolute screen coords.
pub fn finalize_positions(lb: &mut LayoutBox) {
    let ox = lb.content.x;
    let oy = lb.content.y;
    for child in &mut lb.children {
        translate_one(child, ox, oy);  // only move this child, not its subtree
        finalize_positions(child);     // then recurse to handle child's children
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::style::*;
    use crate::render::tree::Node;

    fn block_node(w: f32, h: f32) -> Node {
        let mut n = Node::element("div");
        n.style.display = Display::Block;
        n.style.width = Length::Px(w);
        n.style.height = Length::Px(h);
        n
    }

    #[test]
    fn test_block_explicit_size() {
        let n = block_node(200.0, 100.0);
        let mut lb = layout(&n, Constraints::new(500.0, 500.0), &mut ApproxMeasurer);
        finalize_positions(&mut lb);
        assert_eq!(lb.border_box.w, 200.0);
        assert_eq!(lb.border_box.h, 100.0);
    }

    #[test]
    fn test_block_fills_width() {
        let mut n = Node::element("div");
        n.style.display = Display::Block;
        n.style.height = Length::Px(50.0);
        let mut lb = layout(&n, Constraints::new(400.0, 600.0), &mut ApproxMeasurer);
        finalize_positions(&mut lb);
        assert_eq!(lb.border_box.w, 400.0);
        assert_eq!(lb.border_box.h, 50.0);
    }

    #[test]
    fn test_block_children_stack_vertically() {
        let mut parent = Node::element("div");
        parent.style.display = Display::Block;
        parent.style.width = Length::Px(100.0);
        parent.children_mut().push(block_node(100.0, 30.0));
        parent.children_mut().push(block_node(100.0, 50.0));
        let mut lb = layout(&parent, Constraints::new(200.0, 500.0), &mut ApproxMeasurer);
        finalize_positions(&mut lb);
        assert_eq!(lb.children[0].border_box.y, 0.0);
        assert_eq!(lb.children[1].border_box.y, 30.0);
        assert_eq!(lb.border_box.h, 80.0);
    }

    #[test]
    fn test_padding_expands_border_box() {
        let mut n = Node::element("div");
        n.style.display = Display::Block;
        n.style.width = Length::Px(100.0);
        n.style.height = Length::Px(50.0);
        n.style.padding = Edges::uniform_px(10.0);
        let mut lb = layout(&n, Constraints::new(300.0, 300.0), &mut ApproxMeasurer);
        finalize_positions(&mut lb);
        assert_eq!(lb.border_box.w, 120.0);
        assert_eq!(lb.border_box.h, 70.0);
        assert_eq!(lb.content.x, 10.0);
        assert_eq!(lb.content.y, 10.0);
    }

    #[test]
    fn test_margin_offsets_position() {
        let mut n = Node::element("div");
        n.style.display = Display::Block;
        n.style.width = Length::Px(50.0);
        n.style.height = Length::Px(50.0);
        n.style.margin = Edges::uniform_px(20.0);
        let mut lb = layout(&n, Constraints::new(300.0, 300.0), &mut ApproxMeasurer);
        finalize_positions(&mut lb);
        assert_eq!(lb.border_box.x, 20.0);
        assert_eq!(lb.border_box.y, 20.0);
    }

    #[test]
    fn test_flex_row_distributes_evenly() {
        let mut parent = Node::element("div");
        parent.style.display = Display::Flex;
        parent.style.flex_direction = FlexDirection::Row;
        parent.style.width = Length::Px(300.0);
        parent.style.height = Length::Px(100.0);
        let mut c1 = Node::element("div");
        c1.style.flex_grow = 1.0;
        let mut c2 = Node::element("div");
        c2.style.flex_grow = 1.0;
        parent.children_mut().push(c1);
        parent.children_mut().push(c2);
        let mut lb = layout(&parent, Constraints::new(300.0, 100.0), &mut ApproxMeasurer);
        finalize_positions(&mut lb);
        assert!((lb.children[0].border_box.w - 150.0).abs() < 1.0);
        assert!((lb.children[1].border_box.w - 150.0).abs() < 1.0);
    }

    #[test]
    fn test_flex_row_gap() {
        let mut parent = Node::element("div");
        parent.style.display = Display::Flex;
        parent.style.flex_direction = FlexDirection::Row;
        parent.style.width = Length::Px(300.0);
        parent.style.height = Length::Px(100.0);
        parent.style.gap = 20.0;
        let mut c1 = Node::element("div");
        c1.style.flex_grow = 1.0;
        let mut c2 = Node::element("div");
        c2.style.flex_grow = 1.0;
        parent.children_mut().push(c1);
        parent.children_mut().push(c2);
        let mut lb = layout(&parent, Constraints::new(300.0, 100.0), &mut ApproxMeasurer);
        finalize_positions(&mut lb);
        // 300 - 20 gap = 280 / 2 = 140 each
        assert!((lb.children[0].border_box.w - 140.0).abs() < 1.0, "c0 w={}", lb.children[0].border_box.w);
        // c1 starts at content.x + 140 + 20
        let expected_x = lb.content.x + 140.0 + 20.0;
        assert!((lb.children[1].border_box.x - expected_x).abs() < 1.0, "c1 x={} expected={}", lb.children[1].border_box.x, expected_x);
    }

    #[test]
    fn test_display_none_zero_size() {
        let mut n = Node::element("div");
        n.style.display = Display::None;
        n.style.width = Length::Px(100.0);
        n.style.height = Length::Px(100.0);
        let mut lb = layout(&n, Constraints::new(300.0, 300.0), &mut ApproxMeasurer);
        finalize_positions(&mut lb);
        assert_eq!(lb.border_box.w, 0.0);
        assert_eq!(lb.border_box.h, 0.0);
    }

    #[test]
    fn test_min_width_enforced() {
        let mut n = Node::element("div");
        n.style.display = Display::Block;
        n.style.width = Length::Px(10.0);
        n.style.min_width = Length::Px(100.0);
        n.style.height = Length::Px(50.0);
        let mut lb = layout(&n, Constraints::new(300.0, 300.0), &mut ApproxMeasurer);
        finalize_positions(&mut lb);
        assert_eq!(lb.border_box.w, 100.0);
    }

    #[test]
    fn test_border_included_in_border_box() {
        let mut n = Node::element("div");
        n.style.display = Display::Block;
        n.style.width = Length::Px(100.0);
        n.style.height = Length::Px(50.0);
        n.style.border.width = 5.0;
        let mut lb = layout(&n, Constraints::new(300.0, 300.0), &mut ApproxMeasurer);
        finalize_positions(&mut lb);
        assert_eq!(lb.border_box.w, 110.0);
        assert_eq!(lb.border_box.h, 60.0);
    }

    #[test]
    fn test_percent_width() {
        let mut n = Node::element("div");
        n.style.display = Display::Block;
        n.style.width = Length::Percent(50.0);
        n.style.height = Length::Px(40.0);
        let mut lb = layout(&n, Constraints::new(400.0, 400.0), &mut ApproxMeasurer);
        finalize_positions(&mut lb);
        assert_eq!(lb.border_box.w, 200.0);
    }

    #[test]
    fn test_children_absolute_after_finalize() {
        // Child should be at parent content origin after finalize.
        let mut parent = Node::element("div");
        parent.style.display = Display::Block;
        parent.style.width = Length::Px(200.0);
        parent.style.padding = Edges::uniform_px(10.0);
        parent.children_mut().push(block_node(100.0, 50.0));
        let mut lb = layout(&parent, Constraints::new(300.0, 300.0), &mut ApproxMeasurer);
        finalize_positions(&mut lb);
        // Child border_box.x should equal parent content.x = 10
        assert_eq!(lb.children[0].border_box.x, 10.0);
        assert_eq!(lb.children[0].border_box.y, 10.0);
    }

    // --- Performance benchmarks ---

    fn make_deep_tree(depth: u32, width: u32) -> Node {
        if depth == 0 {
            return Node::text("leaf");
        }
        let mut n = Node::element("div");
        n.style.display = Display::Block;
        n.style.width = Length::Px(100.0);
        for _ in 0..width {
            n.children_mut().push(make_deep_tree(depth - 1, width));
        }
        n
    }

    #[test]
    fn perf_layout_deep_tree() {
        let tree = make_deep_tree(6, 4);
        let start = std::time::Instant::now();
        for _ in 0..100 {
            let mut lb = layout(&tree, Constraints::new(1920.0, 1080.0), &mut ApproxMeasurer);
            finalize_positions(&mut lb);
        }
        let elapsed = start.elapsed();
        println!("perf_layout_deep_tree: 100 passes in {:?} ({:.2}ms/pass)", elapsed, elapsed.as_millis() as f64 / 100.0);
        assert!(elapsed.as_millis() < 10_000, "layout too slow");
    }

    #[test]
    fn perf_layout_flat_flex() {
        let mut row = Node::element("div");
        row.style.display = Display::Flex;
        row.style.flex_direction = FlexDirection::Row;
        row.style.width = Length::Px(1920.0);
        row.style.height = Length::Px(40.0);
        for _ in 0..1000 {
            let mut c = Node::element("span");
            c.style.flex_grow = 1.0;
            row.children_mut().push(c);
        }
        let start = std::time::Instant::now();
        for _ in 0..1000 {
            let mut lb = layout(&row, Constraints::new(1920.0, 1080.0), &mut ApproxMeasurer);
            finalize_positions(&mut lb);
        }
        let elapsed = start.elapsed();
        println!("perf_layout_flat_flex: 1000 passes of 1000-child flex in {:?} ({:.2}ms/pass)", elapsed, elapsed.as_millis() as f64 / 1000.0);
        assert!(elapsed.as_millis() < 5_000, "flex layout too slow");
    }

    #[test]
    fn perf_style_cascade() {
        use crate::render::style::*;
        let mut sheet = Stylesheet::new();
        for i in 0..500 {
            sheet.add(Selector::Class(format!("c{}", i)), vec![StyleDecl::FontSize(i as f32)]);
        }
        let node = NodeDesc {
            tag: "div".into(),
            classes: (0..500).map(|i| format!("c{}", i)).collect(),
            id: None,
            ancestors: vec![],
        };
        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            let _ = compute_style(&sheet, &node, None);
        }
        let elapsed = start.elapsed();
        println!("perf_style_cascade: 10k compute_style calls in {:?} ({:.2}us/call)", elapsed, elapsed.as_micros() as f64 / 10_000.0);
        assert!(elapsed.as_millis() < 5_000, "style cascade too slow");
    }
}
