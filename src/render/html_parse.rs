// HTML-like declarative UI markup parser → Node tree.
//
// Syntax subset:
//   <tag class="a b" id="foo" attr="val">children</tag>
//   <tag />           self-closing
//   Text nodes between tags (whitespace-only text nodes are collapsed to nothing)
//   <!-- comments -->
//
// This is intentionally not full HTML — no entities, no implicit closing, etc.

use crate::render::tree::{Node, NodeContent};

pub fn parse_html(src: &str) -> Vec<Node> {
    let src = strip_html_comments(src);
    let mut pos = 0;
    parse_children(&src, &mut pos)
}

fn strip_html_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => { rest = &rest[start + end + 3..]; }
            None => { break; }
        }
    }
    out.push_str(rest);
    out
}

fn parse_children(src: &str, pos: &mut usize) -> Vec<Node> {
    let mut children = Vec::new();
    loop {
        skip_whitespace(src, pos);
        if *pos >= src.len() { break; }

        // Check for closing tag or end of input
        if src[*pos..].starts_with("</") { break; }

        if src[*pos..].starts_with('<') {
            match parse_element(src, pos) {
                Some(node) => children.push(node),
                None => break,
            }
        } else {
            // Text node — read until '<'
            let text = read_text(src, pos);
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                children.push(Node::text(trimmed));
            }
        }
    }
    children
}

fn parse_element(src: &str, pos: &mut usize) -> Option<Node> {
    if !src[*pos..].starts_with('<') { return None; }
    *pos += 1; // skip '<'

    // Tag name
    let tag_start = *pos;
    while *pos < src.len() && !src.as_bytes()[*pos].is_ascii_whitespace()
        && src.as_bytes()[*pos] != b'>' && src.as_bytes()[*pos] != b'/'
    {
        *pos += 1;
    }
    let tag = src[tag_start..*pos].to_string();
    if tag.is_empty() { return None; }

    let mut node = Node::element(&tag);

    // Parse attributes
    loop {
        skip_whitespace(src, pos);
        if *pos >= src.len() { break; }
        let b = src.as_bytes()[*pos];
        if b == b'>' {
            *pos += 1;
            break;
        }
        if b == b'/' {
            // Self-closing />
            *pos += 1;
            skip_whitespace(src, pos);
            if *pos < src.len() && src.as_bytes()[*pos] == b'>' { *pos += 1; }
            return Some(node);
        }
        // Attribute name
        let attr_start = *pos;
        while *pos < src.len() {
            let c = src.as_bytes()[*pos];
            if c == b'=' || c == b'>' || c == b'/' || c.is_ascii_whitespace() { break; }
            *pos += 1;
        }
        let attr_name = &src[attr_start..*pos];
        if attr_name.is_empty() { *pos += 1; continue; }

        skip_whitespace(src, pos);
        if *pos < src.len() && src.as_bytes()[*pos] == b'=' {
            *pos += 1;
            skip_whitespace(src, pos);
            let attr_val = read_attr_value(src, pos);
            apply_attr(&mut node, attr_name, &attr_val);
        } else {
            // Boolean attribute (no value)
            apply_attr(&mut node, attr_name, attr_name);
        }
    }

    // Parse children
    let child_nodes = parse_children(src, pos);
    for child in child_nodes {
        node = node.with_child(child);
    }

    // Consume closing tag </tag>
    skip_whitespace(src, pos);
    if *pos < src.len() && src[*pos..].starts_with("</") {
        *pos += 2;
        // skip tag name and '>'
        while *pos < src.len() && src.as_bytes()[*pos] != b'>' { *pos += 1; }
        if *pos < src.len() { *pos += 1; }
    }

    Some(node)
}

fn apply_attr(node: &mut Node, name: &str, val: &str) {
    if let NodeContent::Element { classes, id, attrs, .. } = &mut node.content {
        match name {
            "class" => {
                for cls in val.split_whitespace() {
                    if !cls.is_empty() { classes.push(cls.to_string()); }
                }
            }
            "id" => { *id = Some(val.to_string()); }
            other => { attrs.insert(other.to_string(), val.to_string()); }
        }
    }
}

fn read_attr_value(src: &str, pos: &mut usize) -> String {
    if *pos >= src.len() { return String::new(); }
    let quote = src.as_bytes()[*pos];
    if quote == b'"' || quote == b'\'' {
        *pos += 1;
        let start = *pos;
        while *pos < src.len() && src.as_bytes()[*pos] != quote { *pos += 1; }
        let val = src[start..*pos].to_string();
        if *pos < src.len() { *pos += 1; } // closing quote
        val
    } else {
        // Unquoted
        let start = *pos;
        while *pos < src.len() {
            let c = src.as_bytes()[*pos];
            if c.is_ascii_whitespace() || c == b'>' { break; }
            *pos += 1;
        }
        src[start..*pos].to_string()
    }
}

fn read_text(src: &str, pos: &mut usize) -> String {
    let start = *pos;
    while *pos < src.len() && src.as_bytes()[*pos] != b'<' { *pos += 1; }
    src[start..*pos].to_string()
}

fn skip_whitespace(src: &str, pos: &mut usize) {
    while *pos < src.len() && src.as_bytes()[*pos].is_ascii_whitespace() { *pos += 1; }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::tree::NodeContent;

    #[test]
    fn test_simple_element() {
        let nodes = parse_html("<div></div>");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].tag(), Some("div"));
    }

    #[test]
    fn test_element_with_class() {
        let nodes = parse_html(r#"<div class="toolbar"></div>"#);
        assert_eq!(nodes.len(), 1);
        if let NodeContent::Element { classes, .. } = &nodes[0].content {
            assert!(classes.contains(&"toolbar".to_string()));
        } else { panic!(); }
    }

    #[test]
    fn test_multiple_classes() {
        let nodes = parse_html(r#"<div class="tab active"></div>"#);
        if let NodeContent::Element { classes, .. } = &nodes[0].content {
            assert!(classes.contains(&"tab".to_string()));
            assert!(classes.contains(&"active".to_string()));
        } else { panic!(); }
    }

    #[test]
    fn test_text_node() {
        let nodes = parse_html("<div>Hello</div>");
        let children = nodes[0].children();
        assert_eq!(children.len(), 1);
        assert!(matches!(&children[0].content, NodeContent::Text(t) if t == "Hello"));
    }

    #[test]
    fn test_nested_elements() {
        let nodes = parse_html("<div><span>hi</span></div>");
        let inner = nodes[0].children();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].tag(), Some("span"));
    }

    #[test]
    fn test_self_closing() {
        let nodes = parse_html(r#"<div />"#);
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].children().len(), 0);
    }

    #[test]
    fn test_custom_attr() {
        let nodes = parse_html(r#"<div menu="file">File</div>"#);
        if let NodeContent::Element { attrs, .. } = &nodes[0].content {
            assert_eq!(attrs.get("menu").map(|s| s.as_str()), Some("file"));
        } else { panic!(); }
    }

    #[test]
    fn test_id_attr() {
        let nodes = parse_html(r#"<div id="main"></div>"#);
        if let NodeContent::Element { id, .. } = &nodes[0].content {
            assert_eq!(id.as_deref(), Some("main"));
        } else { panic!(); }
    }

    #[test]
    fn test_comment_stripped() {
        let nodes = parse_html("<!-- comment --><div></div>");
        assert_eq!(nodes.len(), 1);
    }

    #[test]
    fn test_sibling_elements() {
        let nodes = parse_html("<div></div><span></span>");
        assert_eq!(nodes.len(), 2);
    }

    #[test]
    fn test_whitespace_only_text_ignored() {
        let nodes = parse_html("<div>  \n  </div>");
        assert_eq!(nodes[0].children().len(), 0);
    }

    #[test]
    fn test_deep_nesting() {
        let nodes = parse_html("<a><b><c>text</c></b></a>");
        let b = &nodes[0].children()[0];
        let c = &b.children()[0];
        assert_eq!(c.tag(), Some("c"));
        assert!(matches!(&c.children()[0].content, NodeContent::Text(t) if t == "text"));
    }
}
