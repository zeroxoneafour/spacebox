//! Splits a TOML file into independently parseable pieces, so large saves can be parsed in
//! parallel and without holding one parse tree for the whole file (which for a big galaxy
//! takes over twenty times the file's size in memory).
//!
//! A table header (`[a.b.c]`) at the start of a line, outside any multi-line string or array,
//! starts a section, and a section's header names its full key path. So the sections of each
//! `[empire.X…]` or `[system.X…]` group, concatenated in file order, form a TOML document of
//! their own whose meaning is the same as in the whole file. Everything else (the `[import]`,
//! asset tables and `[game]`) goes into the head document.
//!
//! The split is only used when it is unambiguous; [`split`] returns `None` otherwise (arrays
//! of tables, a bare `[empire]`/`[system]` table, headers it cannot read), and the caller
//! parses the whole file instead.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;

/// Top-level tables whose entries are split into groups.
pub const GROUPED: [&str; 2] = ["empire", "system"];

pub struct Split {
    /// Everything outside the grouped tables (their lines blanked, so line numbers match).
    pub head: String,
    /// `(top-level table, entry name, byte ranges of its sections)`, in order of first
    /// appearance. Ranges that follow each other are merged, so a group written in one piece
    /// (the usual case) is a single slice of the file.
    pub groups: Vec<Group>,
}

pub struct Group {
    pub top: &'static str,
    pub name: String,
    pub ranges: Vec<Range<usize>>,
}

impl Group {
    /// The group's document: a slice of `text`, or its pieces joined.
    pub fn text<'a>(&self, text: &'a str) -> Cow<'a, str> {
        match self.ranges.as_slice() {
            [r] => Cow::Borrowed(&text[r.clone()]),
            rs => Cow::Owned(rs.iter().map(|r| &text[r.clone()]).collect()),
        }
    }
}

/// Lexical state carried from one line to the next.
#[derive(Clone, Copy, PartialEq)]
enum Open {
    Nothing,
    BasicMultiline,
    LiteralMultiline,
}

/// Splits `text`, or returns `None` if the file must be parsed as a whole.
pub fn split(text: &str) -> Option<Split> {
    let mut head = String::new();
    let mut groups: Vec<Group> = Vec::new();
    // Index into `groups` of the section being collected (`None`: the head).
    let mut current: Option<usize> = None;
    let mut index: HashMap<(&'static str, String), usize> = HashMap::new();
    let mut open = Open::Nothing;
    let mut depth = 0i32;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let range = at..at + line.len();
        at = range.end;
        let trimmed = line.trim_start();
        if open == Open::Nothing && depth == 0 && trimmed.starts_with('[') {
            if trimmed.starts_with("[[") {
                return None;
            }
            let keys = header_keys(trimmed)?;
            current = match keys.first().map(String::as_str) {
                Some(top) if GROUPED.contains(&top) => {
                    let top = GROUPED.iter().find(|g| **g == top).copied().unwrap();
                    // A bare `[empire]` table could define entries with dotted keys.
                    let name = keys.get(1)?.clone();
                    let next = groups.len();
                    let i = *index.entry((top, name.clone())).or_insert(next);
                    if i == next {
                        groups.push(Group {
                            top,
                            name,
                            ranges: Vec::new(),
                        });
                    }
                    Some(i)
                }
                _ => None,
            };
        } else {
            (open, depth) = scan(line, open, depth)?;
        }
        match current {
            Some(i) => {
                let ranges = &mut groups[i].ranges;
                match ranges.last_mut() {
                    Some(last) if last.end == range.start => last.end = range.end,
                    _ => ranges.push(range),
                }
                // Keep the head's line numbers right for error messages.
                head.push('\n');
            }
            None => head.push_str(line),
        }
    }
    if open != Open::Nothing || depth != 0 {
        return None;
    }
    Some(Split { head, groups })
}

/// Updates the multi-line string / array state across one line of key-value content.
fn scan(line: &str, mut open: Open, mut depth: i32) -> Option<(Open, i32)> {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match open {
            Open::BasicMultiline => {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if b[i..].starts_with(b"\"\"\"") {
                    i += 3;
                    // Up to two more quotes may belong to the string's content.
                    while i < b.len() && b[i] == b'"' {
                        i += 1;
                    }
                    open = Open::Nothing;
                    continue;
                }
                i += 1;
            }
            Open::LiteralMultiline => {
                if b[i..].starts_with(b"'''") {
                    i += 3;
                    while i < b.len() && b[i] == b'\'' {
                        i += 1;
                    }
                    open = Open::Nothing;
                    continue;
                }
                i += 1;
            }
            Open::Nothing => match b[i] {
                b'#' => break,
                b'"' if b[i..].starts_with(b"\"\"\"") => {
                    open = Open::BasicMultiline;
                    i += 3;
                }
                b'\'' if b[i..].starts_with(b"'''") => {
                    open = Open::LiteralMultiline;
                    i += 3;
                }
                b'"' => {
                    i += 1;
                    while i < b.len() && b[i] != b'"' {
                        i += if b[i] == b'\\' { 2 } else { 1 };
                    }
                    i += 1;
                }
                b'\'' => {
                    i += 1;
                    while i < b.len() && b[i] != b'\'' {
                        i += 1;
                    }
                    i += 1;
                }
                b'[' => {
                    depth += 1;
                    i += 1;
                }
                b']' => {
                    depth -= 1;
                    if depth < 0 {
                        return None;
                    }
                    i += 1;
                }
                _ => i += 1,
            },
        }
    }
    Some((open, depth))
}

/// The key path of a `[table.header]` line, or `None` if it isn't a simple, readable one.
fn header_keys(line: &str) -> Option<Vec<String>> {
    let b = line.as_bytes();
    let mut i = 1; // past '['
    let mut keys = Vec::new();
    loop {
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        let key = match *b.get(i)? {
            b'"' => {
                // Escapes in quoted header keys: leave those to the full parser.
                let end = i + 1 + line[i + 1..].find('"')?;
                let k = &line[i + 1..end];
                if k.contains('\\') {
                    return None;
                }
                i = end + 1;
                k.to_string()
            }
            b'\'' => {
                let end = i + 1 + line[i + 1..].find('\'')?;
                let k = line[i + 1..end].to_string();
                i = end + 1;
                k
            }
            _ => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-' || b[i] == b'_')
                {
                    i += 1;
                }
                if i == start {
                    return None;
                }
                line[start..i].to_string()
            }
        };
        keys.push(key);
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        match *b.get(i)? {
            b'.' => i += 1,
            b']' => {
                let rest = line[i + 1..].trim();
                return (rest.is_empty() || rest.starts_with('#')).then_some(keys);
            }
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_groups_and_keeps_the_head() {
        let text = "[import]\na = \"x.toml\"\n\n[game]\nday = 1\n\n[system.a]\nx = 1\n\
                    [system.a.planets.p]\ny = [\n  [1, 2],\n  [3],\n]\n[empire.e]\nz = \"[not.a.header]\"\n\
                    [system.\"b\".planets.q]\nw = 2\n[system.a.planets.p.tile-0]\nv = 3\n";
        let s = split(text).unwrap();
        assert!(s.head.contains("[game]") && !s.head.contains("[system"));
        assert_eq!(s.head.lines().count(), text.lines().count());
        let names: Vec<_> = s.groups.iter().map(|g| (g.top, g.name.as_str())).collect();
        assert_eq!(names, [("system", "a"), ("empire", "e"), ("system", "b")]);
        // The multi-line array's `[1, 2]` line stays inside system.a's text, which comes in
        // two pieces.
        let a = s.groups[0].text(text);
        assert_eq!(s.groups[0].ranges.len(), 2);
        assert!(a.contains("  [1, 2],") && a.contains("[system.a.planets.p.tile-0]"));
        for g in &s.groups {
            g.text(text).parse::<toml::Table>().unwrap();
        }
    }

    #[test]
    fn strings_and_comments_do_not_confuse_it() {
        let text = "[system.a]\nx = \"\"\"\n[system.b]\n\"\"\"\ny = '''\n[z]'''\n# [system.c]\n\
                    q = \"]\" # ]\n[system.d]\n";
        let s = split(text).unwrap();
        let names: Vec<_> = s.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["a", "d"]);
    }

    #[test]
    fn falls_back_when_unsure() {
        assert!(split("[[system]]\na = 1\n").is_none());
        assert!(split("[system]\na.b = 1\n").is_none());
        assert!(split("[system.\"a\\u0041\"]\n").is_none());
        assert!(split("x = [\n1,\n").is_none());
    }
}
