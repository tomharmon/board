use super::*;
use board_local::Schema;

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum SchemaPart {
    Definition,
    Example,
}
pub struct SchemaPage {
    pub id: String,
    pub definition: String,
    pub example: Option<String>,
    pub copied: Option<SchemaPart>,
}

impl SchemaPage {
    fn new(s: &Schema) -> Result<Self> {
        let mut example = None;
        for data in s.valid_examples()? {
            let body = format!(
                "```json#{}\n{}\n```",
                s.id,
                serde_json::to_string_pretty(data)?
            );
            if body.encode_utf16().count() <= 10_000 {
                example = Some(body);
                break;
            }
        }
        Ok(Self {
            id: s.id.clone(),
            definition: serde_json::to_string_pretty(&s.schema)?,
            example,
            copied: None,
        })
    }
}

impl State {
    pub fn show_schema(&mut self, name: &str, version: u32) {
        self.desk.schema_open = None;
        match self
            .db()
            .and_then(|b| b.schema(name, version))
            .and_then(|s| SchemaPage::new(&s))
        {
            Ok(page) => {
                self.error = None;
                self.desk.schema_open = Some(page);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }
    fn schema_text(&self, part: SchemaPart) -> Result<String> {
        let page = self.desk.schema_open.as_ref().context("No open schema")?;
        let schema = self
            .desk
            .schemas
            .iter()
            .find(|s| s.id == page.id)
            .context("Schema no longer available")?;
        self.db()?.schema(&schema.name, schema.version)?;
        match part {
            SchemaPart::Definition => Ok(page.definition.clone()),
            SchemaPart::Example => page.example.clone().context("No validated example block"),
        }
    }
    pub fn copy_schema_part(&mut self, part: SchemaPart, tx: &mut Tx) {
        match self.schema_text(part) {
            Ok(text) => {
                tx.copy(text);
                self.desk
                    .schema_open
                    .as_mut()
                    .expect("visible schema")
                    .copied = Some(part);
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }
    pub fn compose_example(&mut self, tx: &mut Tx) {
        let block = match self.schema_text(SchemaPart::Example) {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(e.to_string());
                return;
            }
        };
        self.go(Page::Compose);
        if self.page == Page::Compose && self.error.is_none() {
            let mut text = self.body.text.clone();
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&block);
            if text.encode_utf16().count() > 10_000 {
                self.error = Some("The example would exceed this draft's message limit".into());
                return;
            }
            self.body = TextEdit::new(text);
            self.draft_changed();
            tx.focus_when_shown("message-body", true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Runtime, clipboard};

    #[test]
    fn every_builtin_schema_has_an_authored_valid_example() {
        let db = Board::memory().unwrap();
        for schema in db.schemas().unwrap() {
            assert!(
                SchemaPage::new(&schema).unwrap().example.is_some(),
                "{}",
                schema.id
            );
        }
    }

    #[test]
    fn schema_buttons_copy_exact_definition_and_valid_fenced_block_through_rune() {
        let db = Board::open(":memory:").unwrap();
        let schema = db.schema("git-commit", 1).unwrap();
        let expected = SchemaPage::new(&schema).unwrap().example.unwrap();
        let m = db
            .post("scratch", "tester", &expected, &Default::default())
            .unwrap();
        assert_eq!(m.blocks.len(), 1);
        let mut state = State::test_board(db, None);
        state.go(Page::Schemas);
        state.show_schema("git-commit", 1);
        let mut rt = Runtime::new(state, crate::view::view, (1000.0, 950.0), Env::default());
        let clip = clipboard::memory();
        rt.use_clipboard(&clip);
        rt.frame(0.0);
        for (key, value) in [
            ("copy-schema-example", expected),
            (
                "copy-schema-definition",
                serde_json::to_string_pretty(&schema.schema).unwrap(),
            ),
        ] {
            let (x, y) = rt.find(key).unwrap();
            rt.pointer_down(x, y);
            rt.pointer_up(x, y);
            rt.frame(1.0 / 30.0);
            assert_eq!(
                clip.text().as_deref().map(String::as_str),
                Some(value.as_str())
            );
        }
    }
}
