// Node tree — the document model fed into layout and paint.

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
