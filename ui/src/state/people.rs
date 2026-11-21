use super::*;

pub struct People {
    pub picking: bool,
    pub query: TextEdit,
    pub rows: Vec<board_local::ActorEntry>,
    pub offset: usize,
    pub editing: bool,
    pub name: TextEdit,
    pub handle: TextEdit,
    pub inline: InlinePeople,
}
#[derive(Default)]
pub struct InlinePeople {
    word: String,
    dismissed: bool,
    pub rows: Vec<board_local::ActorEntry>,
    pub selected: usize,
}

fn mention_token(edit: &TextEdit) -> Option<(std::ops::Range<usize>, &str)> {
    if edit.anchor != edit.focus || edit.preedit().is_some() {
        return None;
    }
    let word = |ch: char| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':');
    let before = &edit.text[..edit.focus];
    let start = before.rfind(|c: char| !word(c))?;
    if before.as_bytes()[start] != b'@'
        || (start > 0
            && !before[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_whitespace() || matches!(c, '(' | '[' | '{')))
    {
        return None;
    }
    let query = &before[start + 1..];
    if query.is_empty() || query.len() > 100 {
        return None;
    }
    let tail = &edit.text[edit.focus..];
    let end = edit.focus + tail.find(|c: char| !word(c)).unwrap_or(tail.len());
    Some((start..end, query))
}
impl Default for People {
    fn default() -> Self {
        Self {
            picking: false,
            query: TextEdit::new(""),
            rows: Vec::new(),
            offset: 0,
            editing: false,
            name: TextEdit::new(""),
            handle: TextEdit::new(""),
            inline: InlinePeople::default(),
        }
    }
}
impl State {
    pub fn inline_open(&self) -> bool {
        !self.people.inline.rows.is_empty()
            && mention_token(&self.body).is_some_and(|(_, q)| q == self.people.inline.word)
    }
    pub fn update_inline(&mut self, force: bool) {
        let token = (!self.access_blocked
            && !self.people.picking
            && !self.delivery.open
            && self.body.focused)
            .then(|| mention_token(&self.body))
            .flatten();
        let Some((_, q)) = token else {
            self.people.inline = Default::default();
            return;
        };
        let old = &self.people.inline;
        if old.word == q && (old.dismissed || !force) {
            return;
        }
        let rows = self
            .db()
            .and_then(|b| b.actor_directory(Some(q)))
            .unwrap_or_default()
            .into_iter()
            .filter(|a| !a.id.contains(']') && !a.id.chars().any(char::is_whitespace))
            .take(5)
            .collect();
        self.people.inline = InlinePeople {
            word: q.to_owned(),
            rows,
            ..Default::default()
        };
    }
    pub fn inline_key(&mut self, k: &rune::KeyEvent, tx: &mut Tx) -> bool {
        use rune::Key;
        let i = &mut self.people.inline;
        if i.rows.is_empty()
            || !mention_token(&self.body).is_some_and(|(_, q)| q == i.word)
            || k.mods.cmd
            || k.mods.ctrl
            || k.mods.alt
            || k.mods.shift
        {
            return false;
        }
        match k.key {
            Key::Down => i.selected = (i.selected + 1) % i.rows.len(),
            Key::Up => i.selected = (i.selected + i.rows.len() - 1) % i.rows.len(),
            Key::Escape => {
                i.rows.clear();
                i.dismissed = true;
            }
            Key::Tab => {
                let id = i.rows[i.selected].id.clone();
                self.complete_mention(&id, tx);
            }
            _ => return false,
        }
        true
    }
    pub fn complete_mention(&mut self, id: &str, tx: &mut Tx) {
        let Some((span, q)) = mention_token(&self.body) else {
            return;
        };
        if q != self.people.inline.word || !self.people.inline.rows.iter().any(|p| p.id == id) {
            return;
        }
        let visible = self
            .db()
            .and_then(|b| b.actor_directory(Some(q)))
            .is_ok_and(|rows| rows.iter().any(|a| a.id == id));
        if !visible {
            self.people.inline = Default::default();
            self.error = Some("Mention suggestion no longer available".into());
            self.refresh();
            return;
        }
        self.body.set_selection(span.start, span.end);
        self.body.insert(&format!("@[{id}] "));
        self.people.inline = Default::default();
        self.draft_changed();
        self.note_typing();
        tx.focus_when_shown("message-body", true);
    }
    pub fn pick_person(&mut self, tx: &mut Tx) {
        self.people = People {
            picking: true,
            ..Default::default()
        };
        self.refresh();
        tx.focus_when_shown("person-query", true);
    }

    pub fn mention_person(&mut self, id: &str, tx: &mut Tx) {
        if !self.people.picking {
            return;
        }
        let found = self.db().and_then(|b| b.actor_directory(Some(id)));
        if let Ok(people) = &found
            && people.iter().any(|a| a.id == id)
            && !id.contains(']')
            && !id.chars().any(char::is_whitespace)
        {
            self.body.insert(&format!("@[{id}] "));
            self.draft_changed();
            self.note_typing();
            self.people = People::default();
            tx.focus_when_shown("message-body", true);
        } else {
            self.error = Some(
                found
                    .err()
                    .map_or_else(|| "Cannot mention this identity".into(), |e| e.to_string()),
            );
        }
    }

    pub fn edit_presentation(&mut self, tx: &mut Tx) {
        let people = match self.db().and_then(|b| b.actor_directory(Some(&self.actor))) {
            Ok(rows) => rows,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        let own = people.iter().find(|p| p.id == self.actor);
        self.people.name = TextEdit::new(own.map_or(&self.actor, |p| &p.name));
        self.people.handle = TextEdit::new(own.map_or(&self.actor, |p| &p.handle));
        self.people.editing = true;
        tx.focus_when_shown("person-name", true);
    }

    pub fn save_presentation(&mut self) {
        match self.db().and_then(|b| {
            b.set_actor_profile(
                &self.actor,
                self.people.handle.text.trim(),
                self.people.name.text.trim(),
            )
        }) {
            Ok(()) => {
                self.people.editing = false;
                self.people.query.clear();
                self.error = None;
                self.refresh();
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }
}
