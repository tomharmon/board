use crate::errors::Kind::Invalid;
use anyhow::{Result, ensure};
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag};
use std::{borrow::Cow, ops::Range, sync::LazyLock};

// Data fences are recognized in prose even between raw HTML tags. Mask only
// the tag opener in the parser's structural input; read all data from the
// original offsets, and let CommonMark handle quote/list/container nesting.
fn structural(text: &str) -> Cow<'_, str> {
    if !text.contains('<') {
        return Cow::Borrowed(text);
    }
    let mut bytes = text.as_bytes().to_vec();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let i = line.len() - line.trim_start_matches(' ').len();
        if i <= 3 && line.as_bytes().get(i) == Some(&b'<') {
            bytes[at + i] = b'x';
        }
        at += line.len();
    }
    Cow::Owned(String::from_utf8(bytes).expect("only ASCII bytes were changed"))
}

pub(crate) struct Fence<'a> {
    pub name: &'a str,
    pub data: &'a str,
}
pub(crate) fn fences(text: &str) -> Result<Vec<Fence<'_>>> {
    let projected = structural(text);
    let mut depth = 0;
    let mut fences = Vec::new();
    for (event, range) in Parser::new(&projected).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0
                    && let Tag::CodeBlock(CodeBlockKind::Fenced(_)) = tag
                {
                    let raw = &text[range];
                    let first = raw.lines().next().unwrap_or("").trim_start();
                    let ch = first.chars().next().unwrap_or('`');
                    let ticks = first.chars().take_while(|c| *c == ch).count();
                    let info = first
                        .get(ticks..)
                        .unwrap_or("")
                        .split_whitespace()
                        .next()
                        .unwrap_or("");
                    if let Some(name) = info.strip_prefix("json#") {
                        let body = raw.split_once('\n').map(|(_, s)| s).unwrap_or("");
                        let mut end = 0;
                        let mut closed = false;
                        for line in body.split_inclusive('\n') {
                            let line_text = line.trim_end_matches(['\n', '\r']);
                            let unindented = line_text.trim_start_matches(' ');
                            let mark = unindented.trim_end_matches([' ', '\t']);
                            if line_text.len() - unindented.len() <= 3
                                && mark.len() >= ticks
                                && mark.chars().all(|c| c == ch)
                            {
                                closed = true;
                                break;
                            }
                            end += line.len();
                        }
                        ensure!(
                            closed,
                            Invalid.error("invalid_block", "data fence does not close")
                        );
                        fences.push(Fence {
                            name,
                            data: &body[..end],
                        });
                    }
                }
                depth += 1;
            }
            Event::End(_) => depth -= 1,
            _ => {}
        }
    }
    Ok(fences)
}

pub(crate) struct Mention<'a> {
    pub value: &'a str,
    pub exact: bool,
    pub range: Range<usize>,
}
// Mention markup is lexical: paired triple-backticks can start mid-paragraph,
// unlike a CommonMark block. A typed data fence is never mention markup,
// regardless of its marker. Preserve byte offsets for safe rewrites of the input.
fn mention_prose(text: &str) -> String {
    let mut bytes = text.as_bytes().to_vec();
    let mut triples = text.match_indices("```").map(|(offset, _)| offset);
    while let (Some(start), Some(end)) = (triples.next(), triples.next()) {
        bytes[start..end + 3].fill(b' ');
    }
    let mut opened = None;
    for i in 0..bytes.len() {
        match bytes[i] {
            b'\n' => opened = None,
            b'`' => {
                if let Some(start) = opened.take() {
                    bytes[start..=i].fill(b' ');
                } else {
                    opened = Some(i);
                }
            }
            _ => {}
        }
    }
    if text.contains("json#") {
        let projected = structural(text);
        let mut depth = 0;
        for (event, range) in Parser::new(&projected).into_offset_iter() {
            match event {
                Event::Start(tag) => {
                    if depth == 0
                        && let Tag::CodeBlock(CodeBlockKind::Fenced(info)) = tag
                        && info
                            .split_whitespace()
                            .next()
                            .is_some_and(|s| s.starts_with("json#"))
                    {
                        bytes[range].fill(b' ');
                    }
                    depth += 1;
                }
                Event::End(_) => depth -= 1,
                _ => {}
            }
        }
    }
    String::from_utf8(bytes).expect("only complete spans were replaced")
}
pub(crate) fn tokens(text: &str) -> Vec<Mention<'_>> {
    if !text.contains('@') {
        return Vec::new();
    }
    let prose = mention_prose(text);
    static TOKEN: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"@(?:\[([^\]\s]+)\]|(?i-u:([a-z0-9](?:[a-z0-9._-]*[a-z0-9])?(?:/[a-z0-9](?:[a-z0-9._-]*[a-z0-9])?)?)))").expect("constant mention pattern")
    });
    TOKEN
        .captures_iter(&prose)
        .filter_map(|found| {
            let whole = found.get(0)?;
            let i = whole.start();
            if prose[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '@' | '.' | '-'))
            {
                return None;
            }
            if let Some(exact) = found.get(1) {
                return Some(Mention {
                    value: &text[exact.range()],
                    exact: true,
                    range: whole.range(),
                });
            }
            let rest = &prose[whole.end()..];
            let next = rest.chars().next();
            if next.is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '-'))
                || rest
                    .strip_prefix(':')
                    .is_some_and(|s| s.starts_with(|c: char| !c.is_whitespace()))
                || rest
                    .strip_prefix('.')
                    .is_some_and(|s| s.starts_with(|c: char| c.is_ascii_alphanumeric()))
            {
                return None;
            }
            Some(Mention {
                value: &text[found.get(2)?.range()],
                exact: false,
                range: whole.range(),
            })
        })
        .collect()
}
pub(crate) fn term(value: &str) -> String {
    if value.contains(':') {
        value.into()
    } else {
        value.to_lowercase()
    }
}
pub(crate) fn query(value: &str) -> String {
    let v = value.trim_start_matches('@');
    term(
        v.strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .unwrap_or(v),
    )
}
pub(crate) fn matches(term: &str, query: &str) -> bool {
    term == query || term.split_once('/').is_some_and(|(p, _)| p == query)
}

pub(crate) fn redact_exact(text: &str, actor: &str) -> String {
    let mut out = String::new();
    let mut at = 0;
    for m in tokens(text)
        .into_iter()
        .filter(|m| m.exact && m.value != actor)
    {
        out.push_str(&text[at..m.range.start]);
        out.push_str("@…");
        at = m.range.end;
    }
    out.push_str(&text[at..]);
    out
}
