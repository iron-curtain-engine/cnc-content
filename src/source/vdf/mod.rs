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

/// Maximum input size accepted by the VDF parser (2 MB).
///
/// Real Steam VDF files are a few KB at most. 2 MB is generous enough for
/// any legitimate `libraryfolders.vdf` or `appmanifest_*.acf` while
/// preventing OOM from a maliciously inflated file.
const MAX_VDF_INPUT_SIZE: usize = 2 * 1024 * 1024;

/// Maximum nesting depth for VDF sections.
///
/// The parser is recursive — for every `{` it calls itself. Without a depth
/// limit, a crafted VDF file with thousands of nested `{` blocks would cause
/// a stack overflow. Real Steam VDF files nest at most 3-4 levels deep.
const MAX_VDF_DEPTH: usize = 64;

/// Parses a VDF text into a top-level section.
///
/// Returns `None` for empty or oversized input. Rejects inputs exceeding
/// [`MAX_VDF_INPUT_SIZE`] (2 MB) and nesting deeper than [`MAX_VDF_DEPTH`]
/// (64 levels).
pub fn parse(input: &str) -> Option<HashMap<String, VdfValue>> {
    // Reject oversized input to prevent OOM via token vector allocation.
    if input.len() > MAX_VDF_INPUT_SIZE {
        return None;
    }

    let tokens = tokenize(input);
    let mut iter = tokens.iter().peekable();
    let mut map = HashMap::new();

    while iter.peek().is_some() {
        if let Some((k, v)) = parse_pair(&mut iter, 0) {
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
    depth: usize,
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
            // Guard against stack overflow from deeply nested VDF sections.
            // A crafted file with thousands of `{` blocks would recurse
            // unboundedly without this check.
            if depth >= MAX_VDF_DEPTH {
                return None;
            }
            iter.next(); // consume '{'
            let mut section = HashMap::new();
            loop {
                match iter.peek() {
                    Some(Token::CloseBrace) => {
                        iter.next(); // consume '}'
                        break;
                    }
                    Some(_) => {
                        if let Some((k, v)) = parse_pair(iter, depth + 1) {
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
mod tests;
