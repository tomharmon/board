use super::*;

pub struct People {
    pub picking: bool,
    pub query: TextEdit,
    pub rows: Vec<board_local::ActorEntry>,
    pub offset: usize,
    pub editing: bool,
    pub name: TextEdit,
    pub handle: TextEdit,
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
        }
    }
}
impl State {
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
