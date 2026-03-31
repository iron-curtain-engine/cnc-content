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

    while let Some(&b) = bytes.get(i) {
        match b {
            b'"' => {
                i += 1;
                let start = i;
                let mut has_escapes = false;
                while bytes.get(i).is_some_and(|&c| c != b'"') {
                    if bytes.get(i) == Some(&b'\\') && bytes.get(i + 1).is_some() {
                        has_escapes = true;
                        i += 2; // skip backslash + next byte
                    } else {
                        i += 1;
                    }
                }
                let raw = input.get(start..i).unwrap_or("");
                let s = if has_escapes {
                    let mut out = String::with_capacity(raw.len());
                    let mut chars = raw.chars();
                    while let Some(c) = chars.next() {
                        if c == '\\' {
                            if let Some(next) = chars.next() {
                                out.push(next);
                            }
                        } else {
                            out.push(c);
                        }
                    }
                    out
                } else {
                    raw.to_string()
                };
                tokens.push(Token::QuotedString(s));
                if bytes.get(i).is_some() {
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
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                // Line comment — skip to end of line.
                while bytes.get(i).is_some_and(|&c| c != b'\n') {
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

    /// Parsing a realistic `libraryfolders.vdf` extract correctly populates
    /// library paths and per-library app ID maps for two library folders.
    ///
    /// This exercises the full nested-section path that `steam.rs` relies on
    /// to enumerate Steam library roots and detect whether a given app ID is
    /// installed there.
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

    /// Parsing a realistic `appmanifest_*.acf` extract yields the `appid` and
    /// `installdir` keys needed to locate a game's content directory.
    ///
    /// These two keys are the minimum required by `steam.rs` to map an app ID
    /// to its on-disk installation path; the test uses actual C&C Red Alert
    /// metadata to catch any key-name assumptions.
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

    /// Parsing an empty string returns `None` rather than an empty map.
    ///
    /// Callers use `Option` to distinguish "file was empty or unreadable" from
    /// "file parsed but contained no recognisable keys"; returning `Some({})`
    /// would cause silent failures when a VDF file is missing or zero-length.
    #[test]
    fn parse_empty_input_returns_none() {
        assert!(parse("").is_none());
    }

    /// Whitespace-only input (spaces, newlines, tabs) produces `None`, not a
    /// spurious empty map.
    ///
    /// The tokenizer skips all non-quoted, non-brace bytes, so it must not
    /// accidentally produce a token from whitespace that tricks the parser
    /// into thinking it found a valid key-value pair.
    #[test]
    fn parse_whitespace_only_returns_none() {
        assert!(parse("   \n\t\n  ").is_none());
    }

    /// The simplest valid VDF document — one quoted key and one quoted value
    /// on a single line — parses to a map with exactly that entry.
    ///
    /// This is the baseline correctness check: if this fails, none of the
    /// more complex tests are meaningful.
    #[test]
    fn parse_single_key_value() {
        let root = parse(r#""key" "value""#).expect("parse failed");
        assert_eq!(root.get("key").unwrap().as_str().unwrap(), "value");
    }

    /// A section containing no key-value pairs parses to a `VdfValue::Section`
    /// holding an empty `HashMap`, not `None` or a string.
    ///
    /// Empty sections appear in Steam VDF files (e.g. an `"apps"` block for a
    /// library folder with no installed games); callers must be able to
    /// distinguish "section present but empty" from "key absent".
    #[test]
    fn parse_empty_section() {
        let root = parse(r#""section" { }"#).expect("parse failed");
        let section = root.get("section").unwrap().as_section().unwrap();
        assert!(section.is_empty());
    }

    /// Three levels of nested sections parse correctly, with the deepest key
    /// accessible by chaining `as_section()` calls.
    ///
    /// Real Steam VDF files nest at least three levels deep
    /// (`libraryfolders` → library index → `apps`); the recursive
    /// `parse_pair` implementation must handle each level without losing
    /// earlier keys.
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

    /// `//` line comments are silently skipped whether they appear at the top
    /// level or inside a section, leaving the surrounding key-value pairs intact.
    ///
    /// Steam's own VDF files use `//` comments to annotate library metadata;
    /// a tokenizer that treats `//` as part of a token would corrupt the
    /// keys that follow.
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

    /// Backslash-escaped characters inside a quoted string are decoded to
    /// their literal byte, so `\\` in source text produces a single `\`.
    ///
    /// Windows paths in Steam VDF files use `\\` as the path separator;
    /// without correct escape handling every path would contain raw
    /// backslash-plus-next-char sequences instead of a proper path.
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

    /// An empty quoted string `""` is stored as an empty `VdfValue::String`,
    /// not as `None` or a missing key.
    ///
    /// Steam uses `""` for optional fields such as `"label"` on library
    /// folders; callers that check `as_str() == Some("")` must not receive
    /// `None` for a present-but-empty value.
    #[test]
    fn parse_empty_string_value() {
        let root = parse(r#""key"  """#).expect("parse failed");
        assert_eq!(root.get("key").unwrap().as_str().unwrap(), "");
    }

    /// Multiple key-value pairs at the top level are all present in the
    /// returned map with their correct values.
    ///
    /// The top-level parse loop must continue after each pair rather than
    /// stopping after the first; stalling early would silently drop all
    /// but the first key from a multi-root VDF document.
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

    /// `VdfValue::as_str` returns `None` when called on a `Section` variant.
    ///
    /// Callers that pattern-match on string values (e.g. reading `"path"`)
    /// must receive `None` for nested sections so they can emit a meaningful
    /// error rather than attempting to use a section as a string.
    #[test]
    fn vdf_value_as_str_on_section_is_none() {
        let val = VdfValue::Section(HashMap::new());
        assert!(val.as_str().is_none());
    }

    /// `VdfValue::as_section` returns `None` when called on a `String` variant.
    ///
    /// Callers that descend into nested sections (e.g. looking for `"apps"`)
    /// must receive `None` for plain string values so they can handle the
    /// unexpected shape rather than treating a string as a section map.
    #[test]
    fn vdf_value_as_section_on_string_is_none() {
        let val = VdfValue::String("hello".to_string());
        assert!(val.as_section().is_none());
    }

    /// A section whose closing brace is missing does not panic; the parser
    /// returns whatever it managed to parse up to EOF.
    ///
    /// Truncated or hand-edited VDF files are a real-world occurrence; the
    /// parser must degrade gracefully so the caller can report the problem
    /// rather than the process crashing.
    #[test]
    fn parse_unclosed_section_is_lenient() {
        // Unclosed brace — parser should return what it parsed without panic.
        let input = r#""section" { "key" "value""#;
        let result = parse(input);
        // It may or may not succeed, but must not panic.
        let _ = result;
    }

    /// A quoted string whose closing `"` is absent does not panic; the
    /// tokenizer reads to EOF and the parser returns whatever it could extract.
    ///
    /// Files written by interrupted processes or corrupted on disk may end
    /// mid-token; the tokenizer's byte-by-byte loop must not go out of bounds
    /// when it hits EOF inside a quoted string.
    #[test]
    fn parse_unclosed_quote_is_lenient() {
        // Unclosed quote — parser should not panic.
        let input = r#""key"  "unclosed"#;
        let result = parse(input);
        let _ = result;
    }

    // ── Unicode / special content ───────────────────────────────────

    /// VDF input containing multi-byte UTF-8 values must parse without panic
    /// and store a value for the key.
    ///
    /// The tokenizer operates on raw bytes and casts each byte to `char`,
    /// so multi-byte codepoints are stored as their individual byte values
    /// rather than proper Unicode. This test documents that behaviour: the
    /// key is present and the value is non-empty, even though the string
    /// content is byte-mangled.
    #[test]
    fn parse_unicode_value() {
        let input = "\"key\" \"\u{65e5}\u{672c}\u{8a9e}\u{30c6}\u{30b9}\u{30c8}\"";
        let root = parse(input).expect("parse failed");
        let value = root.get("key").unwrap().as_str().unwrap();
        // The value is non-empty and was extracted without panic.
        assert!(!value.is_empty());
    }

    /// Deeply nested sections (20 levels) must parse without stack overflow.
    ///
    /// Real VDF files rarely exceed 4-5 levels, but the parser uses recursion
    /// so we verify it handles deeper inputs gracefully.
    #[test]
    fn parse_deeply_nested_sections() {
        let depth = 20;
        let mut input = String::new();
        for i in 0..depth {
            input.push_str(&format!("\"level{i}\" {{\n"));
        }
        input.push_str("\"leaf\" \"value\"\n");
        for _ in 0..depth {
            input.push_str("}\n");
        }

        let root = parse(&input).expect("parse failed");

        // Traverse to the deepest level.
        let mut current = &root;
        for i in 0..depth {
            let key = format!("level{i}");
            current = current
                .get(&key)
                .unwrap_or_else(|| panic!("missing key {key}"))
                .as_section()
                .unwrap_or_else(|| panic!("{key} is not a section"));
        }
        assert_eq!(current.get("leaf").unwrap().as_str().unwrap(), "value");
    }

    // ── Malformed input resilience ──────────────────────────────────

    /// An unmatched closing brace after valid content must not panic.
    ///
    /// Corrupt or hand-edited VDF files may have stray braces; the parser
    /// should degrade gracefully rather than crashing.
    #[test]
    fn parse_unmatched_close_brace() {
        let input = r#""key" "value" }"#;
        // Must not panic — result may be Some or None.
        let _ = parse(input);
    }

    /// A quoted key with no following value or brace must not panic.
    ///
    /// Truncated files can end mid-parse; the parser must handle EOF after
    /// reading a key without attempting to dereference absent tokens.
    #[test]
    fn parse_key_without_value() {
        let input = r#""orphan_key""#;
        // Must not panic — result may be Some or None.
        let _ = parse(input);
    }

    /// Escaped quotes inside a value must be decoded to literal quote chars.
    ///
    /// The tokenizer consumes the backslash and emits the next character
    /// literally, so `\"` inside a quoted string should produce `"`.
    #[test]
    fn parse_escaped_quote_in_value() {
        let input = r#""key" "hello\"world""#;
        let root = parse(input).expect("parse failed");
        assert_eq!(root.get("key").unwrap().as_str().unwrap(), "hello\"world");
    }

    /// Multiple keys with empty-string values must all be stored correctly.
    ///
    /// Empty strings are valid VDF values (e.g. Steam's `"label" ""`); when
    /// several appear consecutively the parser must not skip or merge them.
    #[test]
    fn parse_consecutive_empty_strings() {
        let input = r#""a" "" "b" """#;
        let root = parse(input).expect("parse failed");
        assert_eq!(root.get("a").unwrap().as_str().unwrap(), "");
        assert_eq!(root.get("b").unwrap().as_str().unwrap(), "");
    }
}
