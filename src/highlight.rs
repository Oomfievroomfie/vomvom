// Simple syntax highlighter: tokenizes a line into (text, class) pairs.
// "class" is a CSS class name like "tok-kw", "tok-str", or "" for plain text.

pub fn tokenize_line<'a>(line: &'a str, lang: Lang) -> Vec<(&'a str, &'static str)> {
    match lang {
        Lang::Rust => tokenize_rust(line),
        Lang::Unknown => vec![(line, "")],
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Lang {
    Rust,
    Unknown,
}

pub fn lang_from_path(path: Option<&str>) -> Lang {
    let ext = path
        .and_then(|p| std::path::Path::new(p).extension())
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        "rs" => Lang::Rust,
        _ => Lang::Unknown,
    }
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn",
    "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in",
    "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "union", "unsafe", "use", "where", "while",
];

fn tokenize_rust(line: &str) -> Vec<(&str, &'static str)> {
    let mut result = Vec::new();
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Line comment
        if i + 1 < len && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            result.push((&line[i..], "tok-comment"));
            break;
        }

        // String literal (double-quoted, no raw strings)
        if bytes[i] == b'"' {
            let start = i;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' {
                    i += 2;
                } else if bytes[i] == b'"' {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
            result.push((&line[start..i], "tok-str"));
            continue;
        }

        // Char literal
        if bytes[i] == b'\'' {
            let start = i;
            i += 1;
            // lifetime or char?
            let is_lifetime = i < len && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'_')
                && (i + 1 >= len || bytes[i + 1] != b'\'');
            if is_lifetime {
                while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                result.push((&line[start..i], "tok-lifetime"));
            } else {
                while i < len {
                    if bytes[i] == b'\\' {
                        i += 2;
                    } else if bytes[i] == b'\'' {
                        i += 1;
                        break;
                    } else {
                        i += 1;
                    }
                }
                result.push((&line[start..i], "tok-str"));
            }
            continue;
        }

        // Number literal
        if bytes[i].is_ascii_digit() || (bytes[i] == b'0' && i + 1 < len && (bytes[i+1] == b'x' || bytes[i+1] == b'b' || bytes[i+1] == b'o')) {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.') {
                i += 1;
            }
            result.push((&line[start..i], "tok-num"));
            continue;
        }

        // Identifier or keyword
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let start = i;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &line[start..i];
            // Check for macro call (word followed by '!')
            let is_macro = i < len && bytes[i] == b'!';
            // Check for type-like (starts uppercase)
            let class = if RUST_KEYWORDS.contains(&word) {
                "tok-kw"
            } else if is_macro {
                i += 1; // consume the '!'
                "tok-macro"
            } else if word.starts_with(|c: char| c.is_ascii_uppercase()) {
                "tok-type"
            } else {
                ""
            };
            let end = i;
            result.push((&line[start..end], class));
            continue;
        }

        // Punctuation / symbols: consume a single char as plain or operator
        let start = i;
        i += 1;
        // Absorb runs of operator chars
        while i < len && !bytes[i].is_ascii_alphanumeric() && bytes[i] != b'_'
            && bytes[i] != b'"' && bytes[i] != b'\'' && bytes[i] != b'/'
        {
            i += 1;
        }
        result.push((&line[start..i], "tok-punct"));
    }

    // Merge adjacent tokens of the same class to reduce node count.
    let mut merged: Vec<(&str, &'static str)> = Vec::new();
    for (text, class) in result {
        if text.is_empty() { continue; }
        if let Some(last) = merged.last_mut() {
            if last.1 == class {
                // Extend the slice: both are subslices of `line`, so we can reconstruct.
                let last_start = last.0.as_ptr() as usize - line.as_ptr() as usize;
                let this_start = text.as_ptr() as usize - line.as_ptr() as usize;
                let this_end = this_start + text.len();
                if last_start + last.0.len() == this_start {
                    *last = (&line[last_start..this_end], class);
                    continue;
                }
            }
        }
        merged.push((text, class));
    }

    merged
}
