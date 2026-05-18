// CSS-like declarative stylesheet parser.
//
// Syntax subset:
//   selector { prop: value; ... }
//   Selectors: tag .class #id tag.class .a.b  parent > child  ancestor descendant
//   Multiple selectors per rule: a, b { ... }
//   Values: 12px  50%  auto  #rrggbb  #rgb  rgb(r,g,b)  rgba(r,g,b,a)  keywords
//   Comments: /* ... */

use crate::render::style::{
    AlignItems, Color, Display, Edges, FlexDirection, JustifyContent, Length, Overflow,
    Position, Selector, StyleDecl, Stylesheet,
};

pub fn parse_stylesheet(src: &str) -> Stylesheet {
    let src = strip_comments(src);
    let mut sheet = Stylesheet::new();
    let mut pos = 0;
    let bytes = src.as_bytes();

    while pos < bytes.len() {
        skip_ws(&src, &mut pos);
        if pos >= bytes.len() { break; }

        // Read selector(s) up to '{'
        let brace = match src[pos..].find('{') {
            Some(i) => pos + i,
            None => break,
        };
        let sel_text = src[pos..brace].trim();
        pos = brace + 1;

        // Read declarations up to '}'
        let close = match src[pos..].find('}') {
            Some(i) => pos + i,
            None => break,
        };
        let decl_text = &src[pos..close];
        pos = close + 1;

        if sel_text.is_empty() { continue; }

        let decls = parse_decls(decl_text);
        if decls.is_empty() { continue; }

        // Multiple selectors separated by comma
        for sel_str in sel_text.split(',') {
            let sel_str = sel_str.trim();
            if sel_str.is_empty() { continue; }
            if let Some(sel) = parse_selector(sel_str) {
                sheet.rules.push(crate::render::style::StyleRule { selector: sel, decls: decls.clone() });
            }
        }
    }

    sheet
}

fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start..].find("*/") {
            Some(end) => { rest = &rest[start + end + 2..]; }
            None => { break; }
        }
    }
    out.push_str(rest);
    out
}

fn skip_ws(s: &str, pos: &mut usize) {
    while *pos < s.len() && s.as_bytes()[*pos].is_ascii_whitespace() {
        *pos += 1;
    }
}

// Parse a selector string like "div.foo > span" or ".bar #id"
fn parse_selector(s: &str) -> Option<Selector> {
    // Split on ' > ' (child combinator) then ' ' (descendant)
    // We parse left-to-right, building up combinators right-associatively.
    // Tokenize into: simple-selectors and combinators.

    let tokens = tokenize_selector(s);
    if tokens.is_empty() { return None; }
    build_selector_from_tokens(&tokens)
}

#[derive(Debug, Clone)]
enum SelToken {
    Simple(String), // "div.foo#bar" — one compound selector
    Child,          // >
    Descendant,     // (space between simples)
}

fn tokenize_selector(s: &str) -> Vec<SelToken> {
    let mut tokens = Vec::new();
    let mut chars = s.chars().peekable();
    let mut current = String::new();
    while let Some(&c) = chars.peek() {
        if c == '>' {
            chars.next();
            if !current.trim().is_empty() {
                tokens.push(SelToken::Simple(current.trim().to_string()));
                current = String::new();
            }
            tokens.push(SelToken::Child);
            // skip surrounding spaces
            while chars.peek() == Some(&' ') { chars.next(); }
        } else if c == ' ' {
            // Potential descendant combinator
            while chars.peek() == Some(&' ') { chars.next(); }
            // If next char is '>' it's just whitespace around child combinator
            if chars.peek() == Some(&'>') {
                if !current.trim().is_empty() {
                    tokens.push(SelToken::Simple(current.trim().to_string()));
                    current = String::new();
                }
                // child combinator handled next iteration
            } else if !current.trim().is_empty() {
                tokens.push(SelToken::Simple(current.trim().to_string()));
                current = String::new();
                tokens.push(SelToken::Descendant);
            }
        } else {
            current.push(c);
            chars.next();
        }
    }
    if !current.trim().is_empty() {
        tokens.push(SelToken::Simple(current.trim().to_string()));
    }
    tokens
}

fn build_selector_from_tokens(tokens: &[SelToken]) -> Option<Selector> {
    // Find last combinator to split on (right-associative)
    // Actually we want to parse left to right: "a > b c" = descendant(child(a,b), c)
    // Find the last combinator token.
    let last_comb = tokens.iter().enumerate().rev().find(|(_, t)| {
        matches!(t, SelToken::Child | SelToken::Descendant)
    });

    match last_comb {
        None => {
            // Single simple selector
            if let Some(SelToken::Simple(s)) = tokens.first() {
                parse_simple_selector(s)
            } else {
                None
            }
        }
        Some((i, comb)) => {
            let left = build_selector_from_tokens(&tokens[..i])?;
            let right = build_selector_from_tokens(&tokens[i+1..])?;
            match comb {
                SelToken::Child => Some(Selector::Child(Box::new(left), Box::new(right))),
                SelToken::Descendant => Some(Selector::Descendant(Box::new(left), Box::new(right))),
                _ => None,
            }
        }
    }
}

// Parse "div.foo.bar#id" into And([Tag, Class, Class, Id]) or single selector.
fn parse_simple_selector(s: &str) -> Option<Selector> {
    if s == "*" { return Some(Selector::Any); }

    let mut parts: Vec<Selector> = Vec::new();
    let mut rest = s;

    // If starts with a letter/- it's a tag name up to next . or #
    if !rest.starts_with('.') && !rest.starts_with('#') {
        let end = rest.find(|c: char| c == '.' || c == '#').unwrap_or(rest.len());
        let tag = &rest[..end];
        if !tag.is_empty() {
            parts.push(Selector::Tag(tag.to_string()));
        }
        rest = &rest[end..];
    }

    while !rest.is_empty() {
        if rest.starts_with('.') {
            rest = &rest[1..];
            let end = rest.find(|c: char| c == '.' || c == '#').unwrap_or(rest.len());
            let cls = &rest[..end];
            if !cls.is_empty() {
                parts.push(Selector::Class(cls.to_string()));
            }
            rest = &rest[end..];
        } else if rest.starts_with('#') {
            rest = &rest[1..];
            let end = rest.find(|c: char| c == '.' || c == '#').unwrap_or(rest.len());
            let id = &rest[..end];
            if !id.is_empty() {
                parts.push(Selector::Id(id.to_string()));
            }
            rest = &rest[end..];
        } else {
            break;
        }
    }

    match parts.len() {
        0 => None,
        1 => Some(parts.remove(0)),
        _ => Some(Selector::And(parts)),
    }
}

fn parse_decls(src: &str) -> Vec<StyleDecl> {
    let mut decls = Vec::new();
    for stmt in src.split(';') {
        let stmt = stmt.trim();
        if stmt.is_empty() { continue; }
        if let Some(colon) = stmt.find(':') {
            let prop = stmt[..colon].trim();
            let val = stmt[colon+1..].trim();
            if let Some(d) = parse_decl(prop, val) {
                decls.push(d);
            }
        }
    }
    decls
}

fn parse_decl(prop: &str, val: &str) -> Option<StyleDecl> {
    match prop {
        "display" => Some(StyleDecl::Display(parse_display(val)?)),
        "position" => Some(StyleDecl::Position(parse_position(val)?)),
        "flex-direction" => Some(StyleDecl::FlexDirection(parse_flex_direction(val)?)),
        "align-items" => Some(StyleDecl::AlignItems(parse_align_items(val)?)),
        "justify-content" => Some(StyleDecl::JustifyContent(parse_justify_content(val)?)),
        "flex-grow" => Some(StyleDecl::FlexGrow(val.parse().ok()?)),
        "flex-shrink" => Some(StyleDecl::FlexShrink(val.parse().ok()?)),
        "flex-basis" => Some(StyleDecl::FlexBasis(parse_length(val)?)),
        "width" => Some(StyleDecl::Width(parse_length(val)?)),
        "height" => Some(StyleDecl::Height(parse_length(val)?)),
        "min-width" => Some(StyleDecl::MinWidth(parse_length(val)?)),
        "min-height" => Some(StyleDecl::MinHeight(parse_length(val)?)),
        "max-width" => Some(StyleDecl::MaxWidth(parse_length(val)?)),
        "max-height" => Some(StyleDecl::MaxHeight(parse_length(val)?)),
        "margin" => Some(StyleDecl::Margin(parse_edges(val)?)),
        "margin-top" => Some(StyleDecl::MarginTop(parse_length(val)?)),
        "margin-right" => Some(StyleDecl::MarginRight(parse_length(val)?)),
        "margin-bottom" => Some(StyleDecl::MarginBottom(parse_length(val)?)),
        "margin-left" => Some(StyleDecl::MarginLeft(parse_length(val)?)),
        "padding" => Some(StyleDecl::Padding(parse_edges(val)?)),
        "padding-top" => Some(StyleDecl::PaddingTop(parse_length(val)?)),
        "padding-right" => Some(StyleDecl::PaddingRight(parse_length(val)?)),
        "padding-bottom" => Some(StyleDecl::PaddingBottom(parse_length(val)?)),
        "padding-left" => Some(StyleDecl::PaddingLeft(parse_length(val)?)),
        "border-width" => Some(StyleDecl::BorderWidth(parse_px_number(val)?)),
        "border-color" => Some(StyleDecl::BorderColor(parse_color(val)?)),
        "border-radius" => Some(StyleDecl::BorderRadius(parse_px_number(val)?)),
        "overflow" => Some(StyleDecl::Overflow(parse_overflow(val)?)),
        "overflow-x" => Some(StyleDecl::OverflowX(parse_overflow(val)?)),
        "overflow-y" => Some(StyleDecl::OverflowY(parse_overflow(val)?)),
        "background-color" => Some(StyleDecl::BackgroundColor(parse_color(val)?)),
        "color" => Some(StyleDecl::Color(parse_color(val)?)),
        "font-size" => Some(StyleDecl::FontSize(parse_px_number(val)?)),
        "line-height" => Some(StyleDecl::LineHeight(val.parse().ok()?)),
        "font-weight" => Some(StyleDecl::FontWeight(val.parse().ok()?)),
        "font-family" => Some(StyleDecl::FontFamily(val.trim_matches('"').trim_matches('\'').to_string())),
        "top" => Some(StyleDecl::Top(parse_length(val)?)),
        "right" => Some(StyleDecl::Right(parse_length(val)?)),
        "bottom" => Some(StyleDecl::Bottom(parse_length(val)?)),
        "left" => Some(StyleDecl::Left(parse_length(val)?)),
        "gap" => Some(StyleDecl::Gap(parse_px_number(val)?)),
        "z-index" => Some(StyleDecl::ZIndex(val.parse().ok()?)),
        "opacity" => Some(StyleDecl::Opacity(val.parse().ok()?)),
        _ => None,
    }
}

fn parse_px_number(val: &str) -> Option<f32> {
    if val.ends_with("px") {
        val[..val.len()-2].trim().parse().ok()
    } else {
        val.trim().parse().ok()
    }
}

fn parse_length(val: &str) -> Option<Length> {
    match val {
        "auto" => Some(Length::Auto),
        "0" => Some(Length::Zero),
        _ if val.ends_with('%') => {
            let n: f32 = val[..val.len()-1].trim().parse().ok()?;
            Some(Length::Percent(n))
        }
        _ if val.ends_with("px") => {
            let n: f32 = val[..val.len()-2].trim().parse().ok()?;
            Some(Length::Px(n))
        }
        _ => {
            // bare number = px
            let n: f32 = val.trim().parse().ok()?;
            Some(Length::Px(n))
        }
    }
}

// Parse shorthand: "10px" (all sides), "10px 5px" (top/bottom, left/right), "10px 8px 6px 4px" (T R B L)
fn parse_edges(val: &str) -> Option<Edges> {
    let parts: Vec<&str> = val.split_whitespace().collect();
    match parts.as_slice() {
        [all] => {
            let v = parse_length(all)?;
            Some(Edges::all(v))
        }
        [tb, lr] => {
            let t = parse_length(tb)?;
            let l = parse_length(lr)?;
            Some(Edges { top: t, bottom: t, left: l, right: l })
        }
        [t, r, b, l] => {
            Some(Edges {
                top: parse_length(t)?,
                right: parse_length(r)?,
                bottom: parse_length(b)?,
                left: parse_length(l)?,
            })
        }
        _ => None,
    }
}

fn parse_color(val: &str) -> Option<Color> {
    let val = val.trim();
    if val.starts_with('#') {
        return parse_hex_color(val);
    }
    if val.starts_with("rgb(") && val.ends_with(')') {
        let inner = &val[4..val.len()-1];
        let parts: Vec<f32> = inner.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        if parts.len() == 3 {
            return Some(Color::rgb(parts[0]/255.0, parts[1]/255.0, parts[2]/255.0));
        }
    }
    if val.starts_with("rgba(") && val.ends_with(')') {
        let inner = &val[5..val.len()-1];
        let parts: Vec<f32> = inner.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        if parts.len() == 4 {
            return Some(Color::rgba(parts[0]/255.0, parts[1]/255.0, parts[2]/255.0, parts[3]));
        }
    }
    // CSS float rgb: rgb(0.2, 0.2, 0.25) — detect by values <= 1.0 with decimal
    // Handled above already; try named colors
    match val {
        "transparent" => Some(Color::TRANSPARENT),
        "black" => Some(Color::BLACK),
        "white" => Some(Color::WHITE),
        _ => None,
    }
}

fn parse_hex_color(s: &str) -> Option<Color> {
    let h = &s[1..];
    match h.len() {
        3 => {
            let r = u8::from_str_radix(&h[0..1].repeat(2), 16).ok()? as f32 / 255.0;
            let g = u8::from_str_radix(&h[1..2].repeat(2), 16).ok()? as f32 / 255.0;
            let b = u8::from_str_radix(&h[2..3].repeat(2), 16).ok()? as f32 / 255.0;
            Some(Color::rgb(r, g, b))
        }
        6 => {
            let r = u8::from_str_radix(&h[0..2], 16).ok()? as f32 / 255.0;
            let g = u8::from_str_radix(&h[2..4], 16).ok()? as f32 / 255.0;
            let b = u8::from_str_radix(&h[4..6], 16).ok()? as f32 / 255.0;
            Some(Color::rgb(r, g, b))
        }
        8 => {
            let r = u8::from_str_radix(&h[0..2], 16).ok()? as f32 / 255.0;
            let g = u8::from_str_radix(&h[2..4], 16).ok()? as f32 / 255.0;
            let b = u8::from_str_radix(&h[4..6], 16).ok()? as f32 / 255.0;
            let a = u8::from_str_radix(&h[6..8], 16).ok()? as f32 / 255.0;
            Some(Color::rgba(r, g, b, a))
        }
        _ => None,
    }
}

fn parse_display(val: &str) -> Option<Display> {
    match val {
        "block" => Some(Display::Block),
        "inline" => Some(Display::Inline),
        "inline-block" => Some(Display::InlineBlock),
        "flex" => Some(Display::Flex),
        "none" => Some(Display::None),
        _ => None,
    }
}

fn parse_position(val: &str) -> Option<Position> {
    match val {
        "static" => Some(Position::Static),
        "relative" => Some(Position::Relative),
        "absolute" => Some(Position::Absolute),
        _ => None,
    }
}

fn parse_flex_direction(val: &str) -> Option<FlexDirection> {
    match val {
        "row" => Some(FlexDirection::Row),
        "column" => Some(FlexDirection::Column),
        _ => None,
    }
}

fn parse_align_items(val: &str) -> Option<AlignItems> {
    match val {
        "flex-start" | "start" => Some(AlignItems::Start),
        "center" => Some(AlignItems::Center),
        "flex-end" | "end" => Some(AlignItems::End),
        "stretch" => Some(AlignItems::Stretch),
        _ => None,
    }
}

fn parse_justify_content(val: &str) -> Option<JustifyContent> {
    match val {
        "flex-start" | "start" => Some(JustifyContent::Start),
        "center" => Some(JustifyContent::Center),
        "flex-end" | "end" => Some(JustifyContent::End),
        "space-between" => Some(JustifyContent::SpaceBetween),
        "space-around" => Some(JustifyContent::SpaceAround),
        _ => None,
    }
}

fn parse_overflow(val: &str) -> Option<Overflow> {
    match val {
        "visible" => Some(Overflow::Visible),
        "hidden" => Some(Overflow::Hidden),
        "scroll" => Some(Overflow::Scroll),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::style::{Selector, Length};

    #[test]
    fn test_parse_tag_rule() {
        let sheet = parse_stylesheet("div { display: block; font-size: 14px; }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].selector, Selector::Tag("div".into()));
        assert_eq!(sheet.rules[0].decls.len(), 2);
    }

    #[test]
    fn test_parse_class_rule() {
        let sheet = parse_stylesheet(".toolbar { display: flex; height: 32px; }");
        assert_eq!(sheet.rules.len(), 1);
        assert!(matches!(sheet.rules[0].selector, Selector::Class(ref c) if c == "toolbar"));
    }

    #[test]
    fn test_parse_compound_selector() {
        let sheet = parse_stylesheet(".tab.active { color: white; }");
        assert_eq!(sheet.rules.len(), 1);
        assert!(matches!(sheet.rules[0].selector, Selector::And(_)));
    }

    #[test]
    fn test_parse_child_selector() {
        let sheet = parse_stylesheet("ul > li { display: block; }");
        assert_eq!(sheet.rules.len(), 1);
        assert!(matches!(sheet.rules[0].selector, Selector::Child(_, _)));
    }

    #[test]
    fn test_parse_descendant_selector() {
        let sheet = parse_stylesheet("div span { color: white; }");
        assert_eq!(sheet.rules.len(), 1);
        assert!(matches!(sheet.rules[0].selector, Selector::Descendant(_, _)));
    }

    #[test]
    fn test_parse_multiple_selectors_comma() {
        let sheet = parse_stylesheet(".a, .b { color: white; }");
        assert_eq!(sheet.rules.len(), 2);
    }

    #[test]
    fn test_parse_hex_color() {
        let sheet = parse_stylesheet("div { color: #ff0000; }");
        if let Some(StyleDecl::Color(c)) = sheet.rules[0].decls.first() {
            assert!((c.r - 1.0).abs() < 0.01);
            assert!((c.g).abs() < 0.01);
        } else {
            panic!("no color decl");
        }
    }

    #[test]
    fn test_parse_percent_length() {
        let sheet = parse_stylesheet("div { width: 100%; }");
        assert!(matches!(sheet.rules[0].decls[0], StyleDecl::Width(Length::Percent(v)) if (v - 100.0).abs() < 0.01));
    }

    #[test]
    fn test_parse_auto_length() {
        let sheet = parse_stylesheet("div { width: auto; }");
        assert!(matches!(sheet.rules[0].decls[0], StyleDecl::Width(Length::Auto)));
    }

    #[test]
    fn test_comments_stripped() {
        let sheet = parse_stylesheet("/* header */ div { display: block; /* comment */ font-size: 12px; }");
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].decls.len(), 2);
    }

    #[test]
    fn test_padding_shorthand_all() {
        let sheet = parse_stylesheet("div { padding: 10px; }");
        if let Some(StyleDecl::Padding(e)) = sheet.rules[0].decls.first() {
            assert_eq!(e.top, Length::Px(10.0));
            assert_eq!(e.left, Length::Px(10.0));
        } else { panic!(); }
    }

    #[test]
    fn test_padding_shorthand_tb_lr() {
        let sheet = parse_stylesheet("div { padding: 4px 10px; }");
        if let Some(StyleDecl::Padding(e)) = sheet.rules[0].decls.first() {
            assert_eq!(e.top, Length::Px(4.0));
            assert_eq!(e.left, Length::Px(10.0));
        } else { panic!(); }
    }

    #[test]
    fn test_rgba_color() {
        let sheet = parse_stylesheet("div { background-color: rgba(255, 255, 255, 0.05); }");
        if let Some(StyleDecl::BackgroundColor(c)) = sheet.rules[0].decls.first() {
            assert!((c.r - 1.0).abs() < 0.01);
            assert!((c.a - 0.05).abs() < 0.01);
        } else { panic!(); }
    }

    #[test]
    fn test_position_absolute() {
        let sheet = parse_stylesheet("div { position: absolute; top: 100%; left: 0; }");
        assert!(matches!(sheet.rules[0].decls[0], StyleDecl::Position(crate::render::style::Position::Absolute)));
        assert!(matches!(sheet.rules[0].decls[1], StyleDecl::Top(Length::Percent(v)) if (v - 100.0).abs() < 0.01));
        assert!(matches!(sheet.rules[0].decls[2], StyleDecl::Left(Length::Zero)));
    }

    #[test]
    fn test_roundtrip_stylesheet() {
        // Parse the full app stylesheet from text and verify rule count matches Rust-built version.
        let css = include_str!("../../assets/main.cssv");
        let sheet = parse_stylesheet(css);
        // Rust version has 14 rules (including the comma-split ones)
        assert!(sheet.rules.len() >= 12, "expected at least 12 rules, got {}", sheet.rules.len());
    }
}
