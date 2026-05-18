// Node tree — the document model fed into layout and paint.
//
// Node provides a DOM-like mutation API: add_class, remove_class, set_attr,
// set_text_content, append_child, clear_children. Document wraps the root
// node and provides id/class lookup so callers can get a &mut Node and
// mutate it in place — no tree rebuilds needed between frames.

use std::collections::{HashMap, HashSet};
use crate::render::style::{ComputedStyle, NodeDesc, Stylesheet, compute_style};

#[derive(Debug, Clone)]
pub enum NodeContent {
    Element {
        tag: String,
        classes: Vec<String>,
        id: Option<String>,
        attrs: HashMap<String, String>,
        children: Vec<Node>,
    },
    Text(String),
}

#[derive(Debug, Clone)]
pub struct Node {
    pub content: NodeContent,
    pub style: ComputedStyle,
}

impl Node {
    pub fn element(tag: impl Into<String>) -> Self {
        Node {
            content: NodeContent::Element {
                tag: tag.into(),
                classes: vec![],
                id: None,
                attrs: HashMap::new(),
                children: vec![],
            },
            style: ComputedStyle::default(),
        }
    }

    pub fn text(s: impl Into<String>) -> Self {
        Node {
            content: NodeContent::Text(s.into()),
            style: ComputedStyle::default(),
        }
    }

    pub fn with_class(mut self, class: impl Into<String>) -> Self {
        if let NodeContent::Element { classes, .. } = &mut self.content {
            classes.push(class.into());
        }
        self
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        if let NodeContent::Element { id: slot, .. } = &mut self.content {
            *slot = Some(id.into());
        }
        self
    }

    pub fn with_child(mut self, child: Node) -> Self {
        if let NodeContent::Element { children, .. } = &mut self.content {
            children.push(child);
        }
        self
    }

    pub fn with_attr(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        if let NodeContent::Element { attrs, .. } = &mut self.content {
            attrs.insert(key.into(), val.into());
        }
        self
    }

    pub fn tag(&self) -> Option<&str> {
        match &self.content {
            NodeContent::Element { tag, .. } => Some(tag),
            NodeContent::Text(_) => None,
        }
    }

    pub fn children(&self) -> &[Node] {
        match &self.content {
            NodeContent::Element { children, .. } => children,
            NodeContent::Text(_) => &[],
        }
    }

    pub fn children_mut(&mut self) -> &mut Vec<Node> {
        match &mut self.content {
            NodeContent::Element { children, .. } => children,
            NodeContent::Text(_) => panic!("text node has no children"),
        }
    }

    // --- DOM mutation API ---

    pub fn add_class(&mut self, class: impl Into<String>) {
        if let NodeContent::Element { classes, .. } = &mut self.content {
            let c = class.into();
            if !classes.contains(&c) { classes.push(c); }
        }
    }

    pub fn remove_class(&mut self, class: &str) {
        if let NodeContent::Element { classes, .. } = &mut self.content {
            classes.retain(|c| c != class);
        }
    }

    pub fn has_class(&self, class: &str) -> bool {
        match &self.content {
            NodeContent::Element { classes, .. } => classes.iter().any(|c| c == class),
            _ => false,
        }
    }

    pub fn set_attr(&mut self, key: impl Into<String>, val: impl Into<String>) {
        if let NodeContent::Element { attrs, .. } = &mut self.content {
            attrs.insert(key.into(), val.into());
        }
    }

    pub fn remove_attr(&mut self, key: &str) {
        if let NodeContent::Element { attrs, .. } = &mut self.content {
            attrs.remove(key);
        }
    }

    pub fn get_attr(&self, key: &str) -> Option<&str> {
        match &self.content {
            NodeContent::Element { attrs, .. } => attrs.get(key).map(|s| s.as_str()),
            _ => None,
        }
    }

    pub fn set_text_content(&mut self, text: impl Into<String>) {
        match &mut self.content {
            NodeContent::Text(t) => { *t = text.into(); }
            NodeContent::Element { children, .. } => {
                let s = text.into();
                children.clear();
                if !s.is_empty() {
                    children.push(Node::text(s));
                }
            }
        }
    }

    pub fn append_child(&mut self, child: Node) {
        if let NodeContent::Element { children, .. } = &mut self.content {
            children.push(child);
        }
    }

    pub fn clear_children(&mut self) {
        if let NodeContent::Element { children, .. } = &mut self.content {
            children.clear();
        }
    }

    // --- DOM query API (depth-first search) ---

    pub fn get_element_by_id(&mut self, id: &str) -> Option<&mut Node> {
        match &self.content {
            NodeContent::Element { id: node_id, .. } if node_id.as_deref() == Some(id) => {
                return Some(self);
            }
            _ => {}
        }
        if let NodeContent::Element { children, .. } = &mut self.content {
            for child in children.iter_mut() {
                if let Some(found) = child.get_element_by_id(id) {
                    return Some(found);
                }
            }
        }
        None
    }

    pub fn get_element_by_id_ref(&self, id: &str) -> Option<&Node> {
        match &self.content {
            NodeContent::Element { id: node_id, .. } if node_id.as_deref() == Some(id) => {
                return Some(self);
            }
            _ => {}
        }
        if let NodeContent::Element { children, .. } = &self.content {
            for child in children.iter() {
                if let Some(found) = child.get_element_by_id_ref(id) {
                    return Some(found);
                }
            }
        }
        None
    }

    pub fn node_desc(&self, ancestors: Vec<NodeDesc>) -> NodeDesc {
        match &self.content {
            NodeContent::Element { tag, classes, id, .. } => NodeDesc {
                tag: tag.clone(),
                classes: classes.iter().cloned().collect(),
                id: id.clone(),
                ancestors,
            },
            NodeContent::Text(_) => NodeDesc {
                tag: "#text".into(),
                classes: HashSet::new(),
                id: None,
                ancestors,
            },
        }
    }
}

/// Retained document: owns the root node, tracks dirty state.
/// Provides DOM-like access so callers mutate the tree in place rather than
/// rebuilding it. Call `apply_styles` before layout each frame.
pub struct Document {
    pub root: Node,
    pub dirty: bool,
}

impl Document {
    pub fn new(root: Node) -> Self {
        Document { root, dirty: true }
    }

    pub fn get_element_by_id(&mut self, id: &str) -> Option<&mut Node> {
        self.root.get_element_by_id(id)
    }

    pub fn get_element_by_id_ref(&self, id: &str) -> Option<&Node> {
        self.root.get_element_by_id_ref(id)
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}

/// Walk the tree and compute styles for every node in place.
pub fn apply_styles(node: &mut Node, sheet: &Stylesheet, ancestor_chain: &[NodeDesc], parent_style: Option<&ComputedStyle>) {
    let desc = node.node_desc(ancestor_chain.to_vec());
    node.style = compute_style(sheet, &desc, parent_style);
    let style_copy = node.style.clone();

    let mut new_chain = ancestor_chain.to_vec();
    new_chain.insert(0, desc);

    if let NodeContent::Element { children, .. } = &mut node.content {
        for child in children.iter_mut() {
            apply_styles(child, sheet, &new_chain, Some(&style_copy));
        }
    }
}
