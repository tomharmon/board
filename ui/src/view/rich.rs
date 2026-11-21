use super::*;
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

fn structured(id: i64, index: usize, b: &Block, raw: bool) -> El {
    let mut panel = col()
        .key(format!("block-{id}-{index}"))
        .w_full()
        .gap(6.0)
        .py(6.0)
        .pl(12.0)
        .border_sides([0.0, 0.0, 0.0, 2.0], c("#729481"))
        .child(
            row()
                .w_full()
                .center_y()
                .gap(8.0)
                .child(icon(&glyphs::FILE_CODE, 16.0).color(c("#376345")))
                .child(
                    mono(format!("{}@{}", b.schema, b.version), "#376345")
                        .grow(1.0)
                        .min_w(0.0)
                        .truncate(),
                )
                .child(
                    tool(
                        &format!("raw-{id}-{index}"),
                        if raw { "Show fields" } else { "Show JSON" },
                        &glyphs::FILE_TEXT,
                    )
                    .on_press(move |s: &mut State| {
                        if !s.raw_blocks.remove(&(id, index)) {
                            s.raw_blocks.insert((id, index));
                        }
                    }),
                ),
        );
    if raw {
        panel = panel.child(
            mono(
                serde_json::to_string_pretty(&b.data).expect("JSON value"),
                INK,
            )
            .w_full()
            .wrap_anywhere()
            .selectable(true),
        );
    } else if let Some(fields) = b.data.as_object() {
        if fields.is_empty() {
            panel = panel.child(mono("{}", MUTED));
        }
        for (name, value) in fields {
            let display = value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string());
            panel = panel.child(
                row()
                    .w_full()
                    .gap(10.0)
                    .child(mono(name, MUTED).w(82.0).wrap_anywhere())
                    .child(
                        label(display, 14.0, 460.0, INK)
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

pub(super) fn body(s: &State, m: &Message) -> El {
    if m.deleted || m.text.is_empty() {
        return label(message_text(m), 15.0, 420.0, MUTED)
            .w_full()
            .wrap_anywhere();
    }
    let style = MarkdownStyle::neutral(false)
        .text(c(INK))
        .muted(c(MUTED))
        .link(c(ACCENT))
        .rule(c(EDGE))
        .body_font(TextStyle::new(15.0, 420.0).lh(21.0))
        .heading_font(1, TextStyle::new(21.0, 650.0))
        .heading_font(2, TextStyle::new(19.0, 620.0))
        .code_block(c(PANEL), c(EDGE));
    let md = if m.blocks.is_empty() {
        Markdown::new(&m.text)
    } else {
        let (src, tags) = mark_fences(m);
        let blocks: Vec<_> = m
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (b.clone(), s.raw_blocks.contains(&(m.id, i))))
            .collect();
        let id = m.id;
        Markdown::new(src).code_frame(move |code, frame| {
            let Some(i) = frame.language.and_then(|tag| tags.get(tag)).copied() else {
                return code;
            };
            structured(id, i, &blocks[i].0, blocks[i].1)
        })
    };
    // No URL opener or image resolver: message content cannot make network or file requests.
    md.tasks_read_only()
        .selectable(true)
        .wrap_code(true)
        .scroll_wide(true)
        .render(&style)
        .w_full()
        .min_w(0.0)
}
