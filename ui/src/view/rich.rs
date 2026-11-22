use super::*;
mod evidence;
use board_local::Block;
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag};
use rune_kit::{Markdown, MarkdownStyle};
use std::collections::{HashMap, HashSet};

// Mark only the fences the database validated, leaving quoted examples and
// ordinary code alone. Rune still parses the whole document, so reference links
// and other Markdown constructs continue to work across structured blocks.
fn mark_fences(m: &Message) -> (String, HashMap<String, usize>) {
    let mut depth = 0;
    let mut reserved = HashSet::new();
    let mut ranges = Vec::new();
    for (event, range) in Parser::new(&m.text).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if let Tag::CodeBlock(CodeBlockKind::Fenced(info)) = tag {
                    if let Some(language) =
                        info.split(|c: char| c.is_whitespace() || c == ',').next()
                    {
                        reserved.insert(language.to_owned());
                    }
                    if depth == 0
                        && let Some((name, version)) =
                            info.strip_prefix("json#").and_then(|v| v.split_once('@'))
                        && let Some(b) = m.blocks.get(ranges.len())
                        && name == b.schema
                        && version.parse::<u32>() == Ok(b.version)
                    {
                        let line = m.text[range.clone()].lines().next().unwrap_or("");
                        let fence = line.trim_start();
                        let delimiter = fence.as_bytes().first().copied();
                        let end = fence.bytes().take_while(|c| Some(*c) == delimiter).count();
                        ranges.push(
                            range.start + line.len() - fence.len() + end..range.start + line.len(),
                        );
                    }
                }
                depth += 1;
            }
            Event::End(_) => depth -= 1,
            _ => {}
        }
    }
    let mut tags = HashMap::new();
    let mut display = String::new();
    let mut from = 0;
    for (i, range) in ranges.into_iter().enumerate() {
        let mut tag = format!("board-data-{i}");
        while reserved.contains(&tag) {
            tag.push('x');
        }
        display.push_str(&m.text[from..range.start]);
        display.push_str(&tag);
        from = range.end;
        tags.insert(tag, i);
    }
    display.push_str(&m.text[from..]);
    (display, tags)
}

struct Display {
    block: Block,
    raw: bool,
    checks: HashSet<usize>,
    lines: usize,
}

fn structured(prefix: &str, id: i64, index: usize, display: &Display) -> El {
    let Display { block: b, raw, .. } = display;
    let mut panel = col()
        .key(format!("{prefix}block-{id}-{index}"))
        .w_full()
        .max_w(640.0)
        .gap(3.0)
        .pb(6.0)
        .border(1.0, c(EDGE))
        .child(
            row()
                .w_full()
                .h(26.0)
                .pl(10.0)
                .center_y()
                .gap(8.0)
                .fill(c(PANEL))
                .child(
                    label(format!("{}@{}", b.schema, b.version), 12., 600., INK)
                        .grow(1.0)
                        .min_w(0.0)
                        .truncate(),
                )
                .child(
                    tool(
                        &format!("{prefix}raw-{id}-{index}"),
                        if *raw { "Show fields" } else { "Show JSON" },
                        &glyphs::FILE_TEXT,
                    )
                    .on_press(move |s: &mut State| {
                        if !s.raw_blocks.remove(&(id, index)) {
                            s.raw_blocks.insert((id, index));
                        }
                    }),
                ),
        );
    if *raw {
        panel = panel.child(
            mono(
                serde_json::to_string_pretty(&b.data).expect("JSON value"),
                INK,
            )
            .w_full()
            .px(10.0)
            .wrap_anywhere()
            .selectable(true),
        );
    } else if let Some(evidence) = evidence::card(prefix, id, index, display) {
        panel = panel.child(evidence);
    } else if let Some(fields) = b.data.as_object() {
        if fields.is_empty() {
            panel = panel.child(mono("{}", MUTED).px(10.));
        }
        for (name, value) in fields {
            let display = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            panel = panel.child(
                row()
                    .w_full()
                    .px(10.0)
                    .gap(10.0)
                    .child(mono(name, MUTED).w(82.0).wrap_anywhere())
                    .child(
                        label(display, 12.0, 400.0, INK)
                            .grow(1.0)
                            .min_w(0.0)
                            .wrap_anywhere()
                            .selectable(true),
                    ),
            );
        }
    }
    panel
}

fn mention_label<'a>(s: &'a State, prefix: &str, id: &'a str) -> &'a str {
    if prefix == "swarm-"
        && let Some(run) = s.desk.swarm.run.as_ref()
    {
        if let Some(w) = run.workers.iter().find(|w| w.actor == id) {
            return &w.name;
        }
        if run.coordinator_actor == id {
            return id.split_once('/').map_or(id, |(_, profile)| profile);
        }
    }
    if let Some(p) = s.people.rows.iter().find(|p| p.id == id) {
        return &p.handle;
    }
    id
}

fn format_mentions<'a>(s: &State, prefix: &str, text: &'a str) -> std::borrow::Cow<'a, str> {
    if !text.contains("@[") {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut skip = Vec::new();
    for (event, range) in Parser::new(text).into_offset_iter() {
        if matches!(event, Event::Start(Tag::CodeBlock(_)) | Event::Code(_)) {
            skip.push(range);
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    let mut changed = false;
    let mut cursor = 0;
    while let Some(rel) = text[cursor..].find("@[") {
        let start = cursor + rel;
        cursor = start + 2;
        if skip.iter().any(|r| r.start <= start && start < r.end) {
            continue;
        }
        if text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '@' | '.' | '-'))
        {
            continue;
        }
        let Some(end_rel) = text[start + 2..].find(']') else {
            break;
        };
        let end = start + 2 + end_rel;
        let id = &text[start + 2..end];
        if id.is_empty() || id.chars().any(|c| c.is_whitespace() || c == '[') {
            continue;
        }
        out.push_str(&text[at..start]);
        out.push('@');
        out.push_str(mention_label(s, prefix, id));
        at = end + 1;
        cursor = at;
        changed = true;
    }
    if !changed {
        return std::borrow::Cow::Borrowed(text);
    }
    out.push_str(&text[at..]);
    std::borrow::Cow::Owned(out)
}

pub(super) fn body(s: &State, m: &Message) -> El {
    scoped_body(s, m, "")
}
pub(super) fn scoped_body(s: &State, m: &Message, prefix: &'static str) -> El {
    if m.deleted || m.text.is_empty() {
        return label(message_text(m), 14.0, 400.0, MUTED)
            .w_full()
            .wrap_anywhere();
    }
    let style = MarkdownStyle::neutral(dark())
        .text(c(DIM))
        .muted(c(MUTED))
        .link(c(ACCENT))
        .rule(c(EDGE))
        .body_font(font(13.6, 400.0).lh(21.0))
        .heading_font(1, font(14.0, 600.0).lh(21.0))
        .heading_font(2, font(14.0, 600.0).lh(21.0))
        .heading_font(3, font(14.0, 600.0).lh(21.0))
        .code_block_font(font(13., 400.).lh(19.5))
        .code_block_padding(10., 10.)
        .quote(c(EDGE), 2., 10., c(MUTED))
        .list(20., 1.)
        .inline_code_bg(c(PANEL))
        .strong_weight(600.)
        .code_block(c(PANEL), c(EDGE));
    let md = if m.blocks.is_empty() {
        Markdown::new(format_mentions(s, prefix, &m.text).into_owned())
    } else {
        let (src, tags) = mark_fences(m);
        let src = format_mentions(s, prefix, &src).into_owned();
        let blocks: Vec<_> = m
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| Display {
                block: b.clone(),
                raw: s.raw_blocks.contains(&(m.id, i)),
                checks: s
                    .expanded_checks
                    .iter()
                    .filter_map(|&(id, block, check)| (id == m.id && block == i).then_some(check))
                    .collect(),
                lines: *s.patch_lines.get(&(m.id, i)).unwrap_or(&evidence::CHUNK),
            })
            .collect();
        let id = m.id;
        Markdown::new(src).code_frame(move |code, frame| {
            let Some(i) = frame.language.and_then(|tag| tags.get(tag)).copied() else {
                return code;
            };
            structured(prefix, id, i, &blocks[i])
        })
    };
    // No URL opener or image resolver: message content cannot make network or file requests.
    let body = md
        .tasks_read_only()
        .selectable(true)
        .wrap_code(true)
        .scroll_wide(true)
        .render(&style)
        .w_full()
        .min_w(0.0);
    if m.notified.is_empty() && m.suppressed.is_empty() {
        return body;
    }
    let names = |workers: &[String]| {
        workers
            .iter()
            .map(|w| format!("@{w}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut out = col().w_full().min_w(0.).gap(5.).child(body);
    if !m.notified.is_empty() {
        out = out.child(
            mono(format!("Notified {}", names(&m.notified)), MUTED)
                .w_full()
                .wrap_anywhere(),
        );
    }
    if !m.suppressed.is_empty() {
        out = out.child(
            mono(
                format!(
                    "Addressed {} but not notified: run limit reached",
                    names(&m.suppressed)
                ),
                DIM,
            )
            .w_full()
            .wrap_anywhere(),
        );
    }
    out
}
