//! Translate native mentions and basic rich text without turning arbitrary Board text into Slack pings.
use super::*;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd, utils::TextMergeStream};
fn uid(s: &str) -> bool {
    (2..=32).contains(&s.len())
        && matches!(s.as_bytes()[0], b'U' | b'W')
        && s.bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}
fn http(s: &str) -> bool {
    (s.starts_with("https://") || s.starts_with("http://"))
        && !s
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '<' | '>' | '|'))
}
pub(super) fn incoming(source: &str, text: &str) -> (String, BTreeMap<String, AuthorInfo>) {
    let mut out = String::new();
    let mut names = BTreeMap::new();
    let mut body = text;
    let mut code = 0;
    while !body.is_empty() {
        if body.starts_with('`') {
            let n = body.bytes().take_while(|c| *c == b'`').count();
            out.push_str(&body[..n]);
            body = &body[n..];
            if code == 0 {
                code = n
            } else if code == n {
                code = 0
            }
            continue;
        }
        if code == 0
            && let Some(tail) = body.strip_prefix('<')
            && let Some(end) = tail.find(['>', '<', '\n'])
            && tail.as_bytes()[end] == b'>'
        {
            let (value, label) = tail[..end]
                .split_once('|')
                .map_or((&tail[..end], None), |(v, l)| (v, Some(l)));
            if let Some(id) = value.strip_prefix('@').filter(|id| uid(id)) {
                out.push_str(&format!("@[source:{source}/{id}]"));
                names.insert(
                    id.into(),
                    AuthorInfo {
                        name: label.filter(|x| !x.is_empty()).unwrap_or(id).to_owned(),
                        avatar: None,
                    },
                );
            } else if http(value) {
                if let Some(label) = label {
                    out.push_str(&format!(
                        "[{}]({})",
                        unescape(label).replace(']', "\\]"),
                        unescape(value).replace(')', "%29")
                    ));
                } else {
                    out.push_str(&unescape(value));
                }
            } else if let Some(id) = value.strip_prefix('#') {
                out.push('#');
                out.push_str(label.unwrap_or(id));
            } else if let Some(id) = value.strip_prefix('!') {
                out.push('@');
                out.push_str(label.unwrap_or(id));
            } else {
                out.push_str(&unescape(&body[..end + 2]));
            }
            body = &tail[end + 1..];
            continue;
        }
        if body.starts_with("&amp;") || body.starts_with("&lt;") || body.starts_with("&gt;") {
            let n = body.find(';').unwrap() + 1;
            out.push_str(&unescape(&body[..n]));
            body = &body[n..];
            continue;
        }
        let c = body.chars().next().unwrap();
        if code == 0 && matches!(c, '*' | '~') {
            out.push(c);
        }
        out.push(c);
        body = &body[c.len_utf8()..];
    }
    (out, names)
}
fn mentions(src: &str, t: &str) -> String {
    let needle = format!("@[source:{src}/");
    let mut out = String::new();
    let mut rest = t;
    while let Some((before, after)) = rest.split_once(&needle) {
        out.push_str(&escape(before));
        if let Some((id, tail)) = after.split_once(']')
            && uid(id)
        {
            out.push_str(&format!("<@{id}>"));
            rest = tail;
        } else {
            out.push_str(&needle);
            rest = after;
        }
    }
    out.push_str(&escape(rest));
    out
}
pub(super) fn outgoing(source: &str, text: &str) -> String {
    let mut out = String::new();
    let mut code = false;
    let mut links = Vec::new();
    let mut list = Vec::new();
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    for ev in TextMergeStream::new(Parser::new_ext(text, options)) {
        match ev {
            Event::Start(Tag::CodeBlock(_)) => {
                out.push_str("```\n");
                code = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                out.push_str("```\n\n");
                code = false;
            }
            Event::Code(t) => {
                out.push('`');
                out.push_str(&escape(&t));
                out.push('`');
            }
            Event::Text(t) => {
                out.push_str(&if code {
                    escape(&t)
                } else {
                    mentions(source, &t)
                });
            }
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::Start(Tag::Strong | Tag::Heading { .. }) | Event::End(TagEnd::Strong) => {
                out.push('*')
            }
            Event::End(TagEnd::Heading(_)) => out.push_str("*\n\n"),
            Event::Start(Tag::Emphasis) | Event::End(TagEnd::Emphasis) => out.push('_'),
            Event::Start(Tag::Strikethrough) | Event::End(TagEnd::Strikethrough) => out.push('~'),
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                let ok = http(&dest_url);
                links.push(ok);
                if ok {
                    out.push('<');
                    out.push_str(&escape(&dest_url));
                    out.push('|');
                }
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                if links.pop() == Some(true) {
                    out.push('>');
                }
            }
            Event::Start(Tag::List(n)) => list.push(n),
            Event::End(TagEnd::List(_)) => {
                list.pop();
                out.push('\n');
            }
            Event::Start(Tag::Item) => {
                if let Some(Some(n)) = list.last_mut() {
                    out.push_str(&format!("{n}. "));
                    *n += 1;
                } else {
                    out.push_str("- ");
                }
            }
            Event::End(TagEnd::Item) => out.push('\n'),
            Event::Start(Tag::BlockQuote(_)) => out.push_str("> "),
            Event::End(TagEnd::Paragraph) => out.push_str("\n\n"),
            Event::InlineHtml(t) | Event::Html(t) => out.push_str(&escape(&t)),
            Event::Rule => out.push_str("---\n"),
            _ => {}
        }
    }
    out.trim_end().to_owned()
}
