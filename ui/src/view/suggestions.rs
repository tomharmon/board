use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(super) enum Field {
    Delivery,
    Filter,
}
impl Field {
    fn key(self) -> &'static str {
        match self {
            Self::Delivery => "compose-labels",
            Self::Filter => "filter-labels",
        }
    }
    fn editor(self, s: &State) -> &rune::TextEdit {
        match self {
            Self::Delivery => &s.delivery.labels,
            Self::Filter => &s.browse.labels,
        }
    }
}

/// Suggest only labels from the currently visible page, like the inspector's
/// known-label set. Re-read the source message on acceptance: a second process
/// may have revoked the grant, deleted it, or changed the label since rendering.
pub(super) fn labels(s: &State, field: Field) -> Option<El> {
    let edit = field.editor(s);
    let end = edit.focus;
    let start = edit.text[..end].rfind('\n').map_or(0, |n| n + 1);
    let line_end = edit.text[end..]
        .find('\n')
        .map_or(edit.text.len(), |n| end + n);
    let partial = edit.text[start..end].trim();
    if partial.is_empty() || edit.preedit().is_some() {
        return None;
    }
    let key_value = partial.split_once('=');
    let mut found = BTreeMap::new();
    let candidates = s
        .thread
        .iter()
        .chain(&s.activity_messages)
        .chain(s.threads.iter().map(|t| &t.message))
        .chain(s.threads.iter().filter_map(|t| t.last.as_ref()))
        .chain(&s.desk.messages);
    for message in candidates.filter(|m| !m.deleted) {
        for (key, value) in &message.labels {
            if key.starts_with("board.") || value.contains(['\n', '\r']) || key.contains('=') {
                continue;
            }
            let replacement = if let Some((k, v)) = key_value {
                let (already, word) = if matches!(field, Field::Filter) {
                    v.rsplit_once('|').unwrap_or(("", v))
                } else {
                    ("", v)
                };
                if k != key
                    || !value.starts_with(word)
                    || (!already.is_empty() && already.split('|').any(|v| v == value))
                {
                    continue;
                }
                if already.is_empty() {
                    format!("{key}={value}")
                } else {
                    format!("{key}={already}|{value}")
                }
            } else if key.starts_with(partial) {
                format!("{key}={value}")
            } else {
                continue;
            };
            if replacement != edit.text[start..line_end] {
                found.entry(replacement).or_insert((message.id, key, value));
            }
        }
    }
    if found.is_empty() {
        return None;
    }
    let mut rows = row().w_full().flex_wrap().gap(4.0);
    for (i, (text, (id, k, v))) in found.into_iter().take(6).enumerate() {
        let (k, v, old) = (k.clone(), v.clone(), edit.text[start..line_end].to_owned());
        let key = format!("{}-suggest-{i}", field.key());
        rows = rows.child(
            click(
                row().key(key).px(6.0).py(3.0).border(1., c(EDGE)),
                &format!("Use {text}"),
            )
            .child(label(&text, 12., 400., MUTED).max_w(230.).truncate())
            .on_press_tx(move |s: &mut State, tx| {
                if !s.has_visible_label(id, &k, &v) {
                    s.error = Some("Label suggestion no longer available".into());
                    s.refresh();
                    return;
                }
                let edit = match field {
                    Field::Delivery => &mut s.delivery.labels,
                    Field::Filter => &mut s.browse.labels,
                };
                if edit.text.get(start..line_end) != Some(&old) {
                    return;
                }
                edit.set_selection(start, line_end);
                edit.insert(&text);
                if matches!(field, Field::Delivery) {
                    s.draft_changed();
                }
                tx.focus_when_shown(field.key(), true);
            }),
        );
    }
    Some(rows)
}
