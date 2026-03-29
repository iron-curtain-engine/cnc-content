// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2025–present Iron Curtain contributors

//! Minimal Valve Data Format (VDF/KeyValues) parser.
//!
//! Parses Steam's `libraryfolders.vdf` and `appmanifest_*.acf` files.
//! Only handles the subset needed: quoted strings and nested braces.

use std::collections::HashMap;

/// A VDF value — either a string or a nested key-value section.
#[derive(Debug, Clone)]
pub enum VdfValue {
    String(String),
    Section(HashMap<String, VdfValue>),
}

impl VdfValue {
    /// Returns the string value if this is a `String` variant.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            VdfValue::String(s) => Some(s),
            VdfValue::Section(_) => None,
        }
    }

    /// Returns the section if this is a `Section` variant.
    pub fn as_section(&self) -> Option<&HashMap<String, VdfValue>> {
        match self {
            VdfValue::String(_) => None,
            VdfValue::Section(m) => Some(m),
        }
    }

    /// Looks up a key in a section.
    #[allow(dead_code)]
    pub fn get(&self, key: &str) -> Option<&VdfValue> {
        self.as_section()?.get(key)
    }
}

/// Parses a VDF text into a top-level section.
pub fn parse(input: &str) -> Option<HashMap<String, VdfValue>> {
    let tokens = tokenize(input);
    let mut iter = tokens.iter().peekable();
    let mut map = HashMap::new();

    while iter.peek().is_some() {
        if let Some((k, v)) = parse_pair(&mut iter) {
            map.insert(k, v);
        } else {
            break;
        }
    }

    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

#[derive(Debug)]
enum Token {
    QuotedString(String),
    OpenBrace,
    CloseBrace,
}

fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                let mut s = String::new();
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        i += 1; // skip backslash, take next char literally
                        s.push(bytes[i] as char);
                    } else {
                        s.push(bytes[i] as char);
                    }
                    i += 1;
                }
                tokens.push(Token::QuotedString(s));
                if i < bytes.len() {
                    i += 1; // skip closing quote
                }
            }
            b'{' => {
                tokens.push(Token::OpenBrace);
                i += 1;
            }
            b'}' => {
                tokens.push(Token::CloseBrace);
                i += 1;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                // Line comment — skip to end of line.
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            _ => {
                i += 1; // whitespace or other — skip
            }
        }
    }

    tokens
}

fn parse_pair(
    iter: &mut std::iter::Peekable<std::slice::Iter<Token>>,
) -> Option<(String, VdfValue)> {
    // Expect a key (quoted string).
    let key = match iter.next()? {
        Token::QuotedString(s) => s.clone(),
        _ => return None,
    };

    // Next is either a value string or an open brace (section).
    match iter.peek()? {
        Token::QuotedString(_) => {
            if let Token::QuotedString(s) = iter.next()? {
                Some((key, VdfValue::String(s.clone())))
            } else {
                None
            }
        }
        Token::OpenBrace => {
            iter.next(); // consume '{'
            let mut section = HashMap::new();
            loop {
                match iter.peek() {
                    Some(Token::CloseBrace) => {
                        iter.next(); // consume '}'
                        break;
                    }
                    Some(_) => {
                        if let Some((k, v)) = parse_pair(iter) {
                            section.insert(k, v);
                        } else {
                            break;
                        }
                    }
                    None => break,
                }
            }
            Some((key, VdfValue::Section(section)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_libraryfolders_vdf() {
        let input = r#"
"libraryfolders"
{
    "0"
    {
        "path"		"C:\\Program Files (x86)\\Steam"
        "label"		""
        "apps"
        {
            "228980"		"178994"
            "2229840"		"0"
        }
    }
    "1"
    {
        "path"		"D:\\SteamLibrary"
        "label"		""
        "apps"
        {
            "1213210"		"12345678"
        }
    }
}
"#;
        let root = parse(input).expect("parse failed");
        let lf = root.get("libraryfolders").unwrap().as_section().unwrap();

        let lib0 = lf.get("0").unwrap().as_section().unwrap();
        assert_eq!(
            lib0.get("path").unwrap().as_str().unwrap(),
            "C:\\Program Files (x86)\\Steam"
        );

        let apps0 = lib0.get("apps").unwrap().as_section().unwrap();
        assert!(apps0.contains_key("2229840"));

        let lib1 = lf.get("1").unwrap().as_section().unwrap();
        assert_eq!(
            lib1.get("path").unwrap().as_str().unwrap(),
            "D:\\SteamLibrary"
        );
    }

    #[test]
    fn parse_appmanifest_acf() {
        let input = r#"
"AppState"
{
    "appid"		"2229840"
    "Universe"		"1"
    "name"		"Command & Conquer Red Alert"
    "installdir"		"CnCRedalert"
}
"#;
        let root = parse(input).expect("parse failed");
        let state = root.get("AppState").unwrap().as_section().unwrap();
        assert_eq!(state.get("appid").unwrap().as_str().unwrap(), "2229840");
        assert_eq!(
            state.get("installdir").unwrap().as_str().unwrap(),
            "CnCRedalert"
        );
    }

    // ── Edge cases ───────────────────────────────────────────────────

    #[test]
    fn parse_empty_input_returns_none() {
        assert!(parse("").is_none());
    }

    #[test]
    fn parse_whitespace_only_returns_none() {
        assert!(parse("   \n\t\n  ").is_none());
    }

    #[test]
    fn parse_single_key_value() {
        let root = parse(r#""key" "value""#).expect("parse failed");
        assert_eq!(root.get("key").unwrap().as_str().unwrap(), "value");
    }

    #[test]
    fn parse_empty_section() {
        let root = parse(r#""section" { }"#).expect("parse failed");
        let section = root.get("section").unwrap().as_section().unwrap();
        assert!(section.is_empty());
    }

    #[test]
    fn parse_nested_sections() {
        let input = r#"
"outer"
{
    "inner"
    {
        "deep"
        {
            "key"  "value"
        }
    }
}
"#;
        let root = parse(input).expect("parse failed");
        let outer = root.get("outer").unwrap().as_section().unwrap();
        let inner = outer.get("inner").unwrap().as_section().unwrap();
        let deep = inner.get("deep").unwrap().as_section().unwrap();
        assert_eq!(deep.get("key").unwrap().as_str().unwrap(), "value");
    }

    #[test]
    fn parse_line_comments() {
        let input = r#"
// This is a comment
"key"  "value"
// Another comment
"section"
{
    // Comment inside section
    "nested"  "data"
}
"#;
        let root = parse(input).expect("parse failed");
        assert_eq!(root.get("key").unwrap().as_str().unwrap(), "value");
        let section = root.get("section").unwrap().as_section().unwrap();
        assert_eq!(section.get("nested").unwrap().as_str().unwrap(), "data");
    }

    #[test]
    fn parse_escape_sequences() {
        // Backslash escapes: \" should produce a literal quote? Actually
        // VDF uses \\ for literal backslash (as in Windows paths). The
        // tokenizer takes the char after \ literally.
        let input = r#""path"  "C:\\Games\\Steam""#;
        let root = parse(input).expect("parse failed");
        assert_eq!(
            root.get("path").unwrap().as_str().unwrap(),
            "C:\\Games\\Steam"
        );
    }

    #[test]
    fn parse_empty_string_value() {
        let root = parse(r#""key"  """#).expect("parse failed");
        assert_eq!(root.get("key").unwrap().as_str().unwrap(), "");
    }

    #[test]
    fn parse_multiple_top_level_pairs() {
        let input = r#"
"a"  "1"
"b"  "2"
"c"  "3"
"#;
        let root = parse(input).expect("parse failed");
        assert_eq!(root.get("a").unwrap().as_str().unwrap(), "1");
        assert_eq!(root.get("b").unwrap().as_str().unwrap(), "2");
        assert_eq!(root.get("c").unwrap().as_str().unwrap(), "3");
    }

    #[test]
    fn vdf_value_as_str_on_section_is_none() {
        let val = VdfValue::Section(HashMap::new());
        assert!(val.as_str().is_none());
    }

    #[test]
    fn vdf_value_as_section_on_string_is_none() {
        let val = VdfValue::String("hello".to_string());
        assert!(val.as_section().is_none());
    }

    #[test]
    fn parse_unclosed_section_is_lenient() {
        // Unclosed brace — parser should return what it parsed without panic.
        let input = r#""section" { "key" "value""#;
        let result = parse(input);
        // It may or may not succeed, but must not panic.
        let _ = result;
    }

    #[test]
    fn parse_unclosed_quote_is_lenient() {
        // Unclosed quote — parser should not panic.
        let input = r#""key"  "unclosed"#;
        let result = parse(input);
        let _ = result;
    }
}
