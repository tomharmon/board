use super::*;
use rune::{Key, KeyEvent};

/// Apply literal Markdown in the native editor's undo history. `false` leaves
/// the key (including composition, Shift+Enter, or forced send) to the caller.
pub fn markdown_key(edit: &mut TextEdit, key: &KeyEvent) -> bool {
    if edit.preedit().is_some() || key.mods.shift || key.mods.alt {
        return false;
    }
    for (ch, marker) in [('b', "**"), ('i', "_"), ('e', "`")] {
        if key.is_command(ch) {
            let (a, b) = edit.range();
            let selected = edit.selected();
            let wrapped = format!("{marker}{selected}{marker}");
            edit.undo_group(|e| {
                e.insert(&wrapped);
                e.set_selection(a + marker.len(), b + marker.len());
            });
            return true;
        }
    }
    if key.key != Key::Enter || key.mods.cmd || key.mods.ctrl || edit.anchor != edit.focus {
        return false;
    }
    let caret = edit.focus;
    let before = &edit.text[..caret];
    let start = before.rfind('\n').map_or(0, |n| n + 1);
    let line = &before[start..];
    let item = line.trim_start_matches(char::is_whitespace);
    let indent = &line[..line.len() - item.len()];
    let Some(at) = item.find(char::is_whitespace) else {
        return false;
    };
    let (marker, rest) = item.split_at(at);
    let next = if marker == "-" || marker == "*" {
        marker.to_owned()
    } else if let Some(number) = marker.strip_suffix('.')
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
    {
        // Never accidentally send a draft because its numeric marker overflowed.
        let mut digits = number.trim_start_matches('0').as_bytes().to_vec();
        let mut carry = true;
        for digit in digits.iter_mut().rev() {
            if *digit < b'9' {
                *digit += 1;
                carry = false;
                break;
            }
            *digit = b'0';
        }
        if carry {
            digits.insert(0, b'1');
        }
        format!("{}.", String::from_utf8(digits).expect("ASCII digits"))
    } else {
        return false;
    };
    if rest.trim().is_empty() {
        edit.undo_group(|e| {
            e.set_selection(start, caret);
            e.insert("");
        });
    } else {
        edit.insert(&format!("\n{indent}{next} "));
    }
    true
}

impl State {
    pub fn edit_last_visible(&mut self, tx: &mut Tx) {
        if self.access_blocked || !self.body.text.is_empty() {
            return;
        }
        let mine = |m: &&Message| m.actor == self.actor && !m.deleted;
        let root_editor = self.edits_root();
        let seen = if !root_editor {
            self.thread.iter().rev().find(mine)
        } else if self.is_timeline() {
            self.activity_messages.iter().rev().find(mine)
        } else {
            self.threads.iter().rev().map(|t| &t.message).find(mine)
        };
        let Some(seen) = seen.cloned() else { return };
        if root_editor {
            self.open_message_thread(seen.id);
            if self.page.thread_id() != Some(seen.re.unwrap_or(seen.id))
                || !self.activate_draft(false)
            {
                return;
            }
            self.browse.reveal = None;
        }
        if self.access_blocked || !self.thread.iter().any(|m| m.id == seen.id && !m.deleted) {
            return;
        }
        // Pin what the user saw; a concurrent edit must conflict, not be overwritten.
        let mut props = Properties::new(&seen);
        props.property = Property::Body;
        self.properties = Some(props);
        self.refresh();
        tx.focus_when_shown("edit-body", true);
    }
}
