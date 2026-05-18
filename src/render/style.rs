// Style cascade and selector system

use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub enum Selector {
    Tag(String),
    Class(String),
    Id(String),
    Child(Box<Selector>, Box<Selector>),    // parent > child
    Descendant(Box<Selector>, Box<Selector>),
    And(Vec<Selector>),                     // compound: tag.class#id
    Any,                                    // *
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Display {
    Block,
    Inline,
    InlineBlock,
    Flex,
    InlineFlex,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FlexDirection {
    Row,
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AlignItems {
    Start,
    Center,
    End,
    Stretch,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JustifyContent {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    Px(f32),
    Percent(f32),
    Auto,
    Zero,
}

impl Length {
    pub fn resolve(&self, parent: f32) -> f32 {
        match self {
            Length::Px(v) => *v,
            Length::Percent(v) => parent * v / 100.0,
            Length::Auto => 0.0,
            Length::Zero => 0.0,
        }
    }
    pub fn is_auto(&self) -> bool {
        matches!(self, Length::Auto)
    }
}

impl Default for Length {
    fn default() -> Self {
        Length::Zero
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
    pub const BLACK: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
    pub const WHITE: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };

    pub fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Color { r, g, b, a }
    }

    pub fn rgb(r: f32, g: f32, b: f32) -> Self {
        Color { r, g, b, a: 1.0 }
    }

    pub fn to_femtovg(&self) -> femtovg::Color {
        femtovg::Color::rgbaf(self.r, self.g, self.b, self.a)
    }
}

impl Default for Color {
    fn default() -> Self {
        Color::TRANSPARENT
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Edges {
    pub top: Length,
    pub right: Length,
    pub bottom: Length,
    pub left: Length,
}

impl Edges {
    pub fn all(v: Length) -> Self {
        Edges { top: v, right: v, bottom: v, left: v }
    }
    pub fn uniform_px(v: f32) -> Self {
        Self::all(Length::Px(v))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Overflow {
    Visible,
    Hidden,
    Scroll,
}

impl Default for Overflow {
    fn default() -> Self {
        Overflow::Visible
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
}

impl Default for Position {
    fn default() -> Self {
        Position::Static
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BorderRadius {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_right: f32,
    pub bottom_left: f32,
}

impl BorderRadius {
    pub fn all(v: f32) -> Self {
        BorderRadius { top_left: v, top_right: v, bottom_right: v, bottom_left: v }
    }
    pub fn is_zero(&self) -> bool {
        self.top_left == 0.0 && self.top_right == 0.0
            && self.bottom_right == 0.0 && self.bottom_left == 0.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Border {
    pub width: f32,
    pub color: Color,
    pub radius: BorderRadius,
}

/// Computed style for a node — all properties have resolved defaults.
#[derive(Debug, Clone)]
pub struct ComputedStyle {
    pub display: Display,
    pub position: Position,
    pub flex_direction: FlexDirection,
    pub align_items: AlignItems,
    pub justify_content: JustifyContent,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Length,
    pub width: Length,
    pub height: Length,
    pub min_width: Length,
    pub min_height: Length,
    pub max_width: Length,
    pub max_height: Length,
    pub margin: Edges,
    pub padding: Edges,
    pub border: Border,
    pub overflow_x: Overflow,
    pub overflow_y: Overflow,
    pub background_color: Color,
    pub color: Color,
    pub font_size: f32,
    pub line_height: f32,
    pub font_weight: u16,
    pub font_family: String,
    pub top: Length,
    pub right: Length,
    pub bottom: Length,
    pub left: Length,
    pub gap: f32,
    pub z_index: i32,
    pub opacity: f32,
}

impl Default for ComputedStyle {
    fn default() -> Self {
        ComputedStyle {
            display: Display::Block,
            position: Position::Static,
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Stretch,
            justify_content: JustifyContent::Start,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Length::Auto,
            width: Length::Auto,
            height: Length::Auto,
            min_width: Length::Zero,
            min_height: Length::Zero,
            max_width: Length::Auto,
            max_height: Length::Auto,
            margin: Edges::default(),
            padding: Edges::default(),
            border: Border::default(),
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Visible,
            background_color: Color::TRANSPARENT,
            color: Color::BLACK,
            font_size: 14.0,
            line_height: 1.4,
            font_weight: 400,
            font_family: String::from("sans-serif"),
            top: Length::Auto,
            right: Length::Auto,
            bottom: Length::Auto,
            left: Length::Auto,
            gap: 0.0,
            z_index: 0,
            opacity: 1.0,
        }
    }
}

/// A single style declaration (property + value, before cascade).
#[derive(Debug, Clone)]
pub enum StyleDecl {
    Display(Display),
    Position(Position),
    FlexDirection(FlexDirection),
    AlignItems(AlignItems),
    JustifyContent(JustifyContent),
    FlexGrow(f32),
    FlexShrink(f32),
    FlexBasis(Length),
    Width(Length),
    Height(Length),
    MinWidth(Length),
    MinHeight(Length),
    MaxWidth(Length),
    MaxHeight(Length),
    Margin(Edges),
    MarginTop(Length),
    MarginRight(Length),
    MarginBottom(Length),
    MarginLeft(Length),
    Padding(Edges),
    PaddingTop(Length),
    PaddingRight(Length),
    PaddingBottom(Length),
    PaddingLeft(Length),
    BorderWidth(f32),
    BorderColor(Color),
    BorderRadius(f32),
    OverflowX(Overflow),
    OverflowY(Overflow),
    Overflow(Overflow),
    BackgroundColor(Color),
    Color(Color),
    FontSize(f32),
    LineHeight(f32),
    FontWeight(u16),
    FontFamily(String),
    Top(Length),
    Right(Length),
    Bottom(Length),
    Left(Length),
    Gap(f32),
    ZIndex(i32),
    Opacity(f32),
}

impl StyleDecl {
    pub fn apply_to(&self, s: &mut ComputedStyle) {
        match self {
            StyleDecl::Display(v) => s.display = *v,
            StyleDecl::Position(v) => s.position = *v,
            StyleDecl::FlexDirection(v) => s.flex_direction = *v,
            StyleDecl::AlignItems(v) => s.align_items = *v,
            StyleDecl::JustifyContent(v) => s.justify_content = *v,
            StyleDecl::FlexGrow(v) => s.flex_grow = *v,
            StyleDecl::FlexShrink(v) => s.flex_shrink = *v,
            StyleDecl::FlexBasis(v) => s.flex_basis = *v,
            StyleDecl::Width(v) => s.width = *v,
            StyleDecl::Height(v) => s.height = *v,
            StyleDecl::MinWidth(v) => s.min_width = *v,
            StyleDecl::MinHeight(v) => s.min_height = *v,
            StyleDecl::MaxWidth(v) => s.max_width = *v,
            StyleDecl::MaxHeight(v) => s.max_height = *v,
            StyleDecl::Margin(v) => s.margin = *v,
            StyleDecl::MarginTop(v) => s.margin.top = *v,
            StyleDecl::MarginRight(v) => s.margin.right = *v,
            StyleDecl::MarginBottom(v) => s.margin.bottom = *v,
            StyleDecl::MarginLeft(v) => s.margin.left = *v,
            StyleDecl::Padding(v) => s.padding = *v,
            StyleDecl::PaddingTop(v) => s.padding.top = *v,
            StyleDecl::PaddingRight(v) => s.padding.right = *v,
            StyleDecl::PaddingBottom(v) => s.padding.bottom = *v,
            StyleDecl::PaddingLeft(v) => s.padding.left = *v,
            StyleDecl::BorderWidth(v) => s.border.width = *v,
            StyleDecl::BorderColor(v) => s.border.color = *v,
            StyleDecl::BorderRadius(v) => s.border.radius = super::style::BorderRadius::all(*v),
            StyleDecl::OverflowX(v) => s.overflow_x = *v,
            StyleDecl::OverflowY(v) => s.overflow_y = *v,
            StyleDecl::Overflow(v) => { s.overflow_x = *v; s.overflow_y = *v; }
            StyleDecl::BackgroundColor(v) => s.background_color = *v,
            StyleDecl::Color(v) => s.color = *v,
            StyleDecl::FontSize(v) => s.font_size = *v,
            StyleDecl::LineHeight(v) => s.line_height = *v,
            StyleDecl::FontWeight(v) => s.font_weight = *v,
            StyleDecl::FontFamily(v) => s.font_family = v.clone(),
            StyleDecl::Top(v) => s.top = *v,
            StyleDecl::Right(v) => s.right = *v,
            StyleDecl::Bottom(v) => s.bottom = *v,
            StyleDecl::Left(v) => s.left = *v,
            StyleDecl::Gap(v) => s.gap = *v,
            StyleDecl::ZIndex(v) => s.z_index = *v,
            StyleDecl::Opacity(v) => s.opacity = *v,
        }
    }
}

/// Specificity: (id, class, tag) — higher wins, last wins ties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Specificity(pub u32, pub u32, pub u32);

impl Specificity {
    pub fn of(sel: &Selector) -> Self {
        match sel {
            Selector::Id(_) => Specificity(1, 0, 0),
            Selector::Class(_) => Specificity(0, 1, 0),
            Selector::Tag(_) => Specificity(0, 0, 1),
            Selector::Any => Specificity(0, 0, 0),
            Selector::And(parts) => {
                parts.iter().fold(Specificity(0, 0, 0), |acc, p| {
                    let s = Specificity::of(p);
                    Specificity(acc.0 + s.0, acc.1 + s.1, acc.2 + s.2)
                })
            }
            Selector::Child(a, b) | Selector::Descendant(a, b) => {
                let sa = Specificity::of(a);
                let sb = Specificity::of(b);
                Specificity(sa.0 + sb.0, sa.1 + sb.1, sa.2 + sb.2)
            }
        }
    }
}

/// One CSS-like rule: selector + declarations.
#[derive(Debug, Clone)]
pub struct StyleRule {
    pub selector: Selector,
    pub decls: Vec<StyleDecl>,
}

/// A stylesheet is an ordered list of rules.
#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    pub rules: Vec<StyleRule>,
}

impl Stylesheet {
    pub fn new() -> Self {
        Stylesheet { rules: Vec::new() }
    }

    pub fn add(&mut self, selector: Selector, decls: Vec<StyleDecl>) {
        self.rules.push(StyleRule { selector, decls });
    }
}

/// Node descriptor used during matching (read-only view of a node's identity).
#[derive(Debug, Clone)]
pub struct NodeDesc {
    pub tag: String,
    pub classes: HashSet<String>,
    pub id: Option<String>,
    /// Ancestor chain — index 0 is direct parent, last is root.
    pub ancestors: Vec<NodeDesc>,
}

impl NodeDesc {
    pub fn matches(&self, sel: &Selector) -> bool {
        match sel {
            Selector::Any => true,
            Selector::Tag(t) => &self.tag == t,
            Selector::Class(c) => self.classes.contains(c.as_str()),
            Selector::Id(i) => self.id.as_deref() == Some(i.as_str()),
            Selector::And(parts) => parts.iter().all(|p| self.matches(p)),
            Selector::Child(parent_sel, child_sel) => {
                if !self.matches(child_sel) {
                    return false;
                }
                self.ancestors.first().map_or(false, |p| p.matches(parent_sel))
            }
            Selector::Descendant(ancestor_sel, child_sel) => {
                if !self.matches(child_sel) {
                    return false;
                }
                self.ancestors.iter().any(|a| a.matches(ancestor_sel))
            }
        }
    }
}

/// Compute the final style for a node by applying matching rules in specificity order.
pub fn compute_style(sheet: &Stylesheet, node: &NodeDesc, inherited: Option<&ComputedStyle>) -> ComputedStyle {
    let mut base = inherited.cloned().unwrap_or_default();
    // Reset non-inherited properties to their defaults
    let defaults = ComputedStyle::default();
    base.display = defaults.display;
    base.position = defaults.position;
    base.width = defaults.width;
    base.height = defaults.height;
    base.min_width = defaults.min_width;
    base.min_height = defaults.min_height;
    base.max_width = defaults.max_width;
    base.max_height = defaults.max_height;
    base.margin = defaults.margin;
    base.padding = defaults.padding;
    base.border = defaults.border;
    base.background_color = defaults.background_color;
    base.overflow_x = defaults.overflow_x;
    base.overflow_y = defaults.overflow_y;
    base.top = defaults.top;
    base.right = defaults.right;
    base.bottom = defaults.bottom;
    base.left = defaults.left;
    base.flex_direction = defaults.flex_direction;
    base.align_items = defaults.align_items;
    base.justify_content = defaults.justify_content;
    base.flex_grow = defaults.flex_grow;
    base.flex_shrink = defaults.flex_shrink;
    base.flex_basis = defaults.flex_basis;
    base.gap = defaults.gap;
    base.z_index = defaults.z_index;
    base.opacity = defaults.opacity;

    // Collect matching rules with their specificity + source order
    let mut matched: Vec<(Specificity, usize, &StyleRule)> = sheet.rules.iter()
        .enumerate()
        .filter(|(_, rule)| node.matches(&rule.selector))
        .map(|(i, rule)| (Specificity::of(&rule.selector), i, rule))
        .collect();

    matched.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    for (_, _, rule) in matched {
        for decl in &rule.decls {
            decl.apply_to(&mut base);
        }
    }

    base
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(tag: &str, classes: &[&str], id: Option<&str>) -> NodeDesc {
        NodeDesc {
            tag: tag.into(),
            classes: classes.iter().map(|s| s.to_string()).collect(),
            id: id.map(|s| s.to_string()),
            ancestors: vec![],
        }
    }

    fn node_child(tag: &str, classes: &[&str], id: Option<&str>, ancestors: Vec<NodeDesc>) -> NodeDesc {
        NodeDesc {
            tag: tag.into(),
            classes: classes.iter().map(|s| s.to_string()).collect(),
            id: id.map(|s| s.to_string()),
            ancestors,
        }
    }

    #[test]
    fn test_tag_selector_matches() {
        let n = node("div", &[], None);
        assert!(n.matches(&Selector::Tag("div".into())));
        assert!(!n.matches(&Selector::Tag("span".into())));
    }

    #[test]
    fn test_class_selector_matches() {
        let n = node("div", &["foo", "bar"], None);
        assert!(n.matches(&Selector::Class("foo".into())));
        assert!(n.matches(&Selector::Class("bar".into())));
        assert!(!n.matches(&Selector::Class("baz".into())));
    }

    #[test]
    fn test_id_selector_matches() {
        let n = node("div", &[], Some("main"));
        assert!(n.matches(&Selector::Id("main".into())));
        assert!(!n.matches(&Selector::Id("other".into())));
    }

    #[test]
    fn test_and_selector() {
        let n = node("div", &["active"], None);
        let sel = Selector::And(vec![
            Selector::Tag("div".into()),
            Selector::Class("active".into()),
        ]);
        assert!(n.matches(&sel));
        let n2 = node("span", &["active"], None);
        assert!(!n2.matches(&sel));
    }

    #[test]
    fn test_child_selector() {
        let parent = node("ul", &[], None);
        let child = node_child("li", &[], None, vec![parent]);
        let sel = Selector::Child(
            Box::new(Selector::Tag("ul".into())),
            Box::new(Selector::Tag("li".into())),
        );
        assert!(child.matches(&sel));
    }

    #[test]
    fn test_descendant_selector() {
        let grandparent = node("section", &[], None);
        let parent = node_child("div", &[], None, vec![grandparent.clone()]);
        // child's ancestors list is flat: [parent, grandparent]
        let child = NodeDesc {
            tag: "p".into(),
            classes: HashSet::new(),
            id: None,
            ancestors: vec![parent, grandparent],
        };
        let sel = Selector::Descendant(
            Box::new(Selector::Tag("section".into())),
            Box::new(Selector::Tag("p".into())),
        );
        assert!(child.matches(&sel));
    }

    #[test]
    fn test_specificity_order() {
        let mut sheet = Stylesheet::new();
        sheet.add(Selector::Tag("div".into()), vec![StyleDecl::FontSize(10.0)]);
        sheet.add(Selector::Class("big".into()), vec![StyleDecl::FontSize(20.0)]);
        sheet.add(Selector::Id("hero".into()), vec![StyleDecl::FontSize(30.0)]);

        let n = NodeDesc {
            tag: "div".into(),
            classes: ["big".to_string()].into_iter().collect(),
            id: Some("hero".into()),
            ancestors: vec![],
        };
        let style = compute_style(&sheet, &n, None);
        assert_eq!(style.font_size, 30.0);
    }

    #[test]
    fn test_later_rule_wins_same_specificity() {
        let mut sheet = Stylesheet::new();
        sheet.add(Selector::Tag("div".into()), vec![StyleDecl::FontSize(10.0)]);
        sheet.add(Selector::Tag("div".into()), vec![StyleDecl::FontSize(20.0)]);

        let n = node("div", &[], None);
        let style = compute_style(&sheet, &n, None);
        assert_eq!(style.font_size, 20.0);
    }

    #[test]
    fn test_inherited_color() {
        let sheet = Stylesheet::new();
        let parent_style = ComputedStyle { color: Color::rgb(1.0, 0.0, 0.0), ..Default::default() };
        let n = node("span", &[], None);
        let style = compute_style(&sheet, &n, Some(&parent_style));
        assert_eq!(style.color, Color::rgb(1.0, 0.0, 0.0));
    }

    #[test]
    fn test_non_inherited_background_resets() {
        let sheet = Stylesheet::new();
        let parent_style = ComputedStyle {
            background_color: Color::rgb(0.0, 1.0, 0.0),
            ..Default::default()
        };
        let n = node("span", &[], None);
        let style = compute_style(&sheet, &n, Some(&parent_style));
        assert_eq!(style.background_color, Color::TRANSPARENT);
    }

    #[test]
    fn test_specificity_values() {
        assert_eq!(Specificity::of(&Selector::Id("x".into())), Specificity(1, 0, 0));
        assert_eq!(Specificity::of(&Selector::Class("x".into())), Specificity(0, 1, 0));
        assert_eq!(Specificity::of(&Selector::Tag("x".into())), Specificity(0, 0, 1));
        let compound = Selector::And(vec![
            Selector::Tag("div".into()),
            Selector::Class("foo".into()),
        ]);
        assert_eq!(Specificity::of(&compound), Specificity(0, 1, 1));
    }

    #[test]
    fn test_margin_shorthand_then_override() {
        let mut sheet = Stylesheet::new();
        sheet.add(Selector::Tag("div".into()), vec![
            StyleDecl::Margin(Edges::uniform_px(10.0)),
            StyleDecl::MarginTop(Length::Px(20.0)),
        ]);
        let n = node("div", &[], None);
        let style = compute_style(&sheet, &n, None);
        assert_eq!(style.margin.top, Length::Px(20.0));
        assert_eq!(style.margin.left, Length::Px(10.0));
    }
}
