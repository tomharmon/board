use super::*;
use board_local::Schema;
use serde_json::{Map, Value, json};

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

// Generate a small suggestion, not a solver. A candidate is never shown as a
// usable block unless the core's actual registered-schema validator accepts it.
fn candidate(s: &Value, key: &str, optional: bool, remaining: &mut u32) -> Option<Value> {
    *remaining = remaining.checked_sub(1)?;
    if let Some(v) = s.get("const").or_else(|| s["enum"].get(0)) {
        return Some(v.clone());
    }
    let kind = s["type"].as_str().or_else(|| s["type"].get(0)?.as_str());
    if kind == Some("object") || s.get("properties").is_some() || s.get("required").is_some() {
        let mut out = Map::new();
        if let Some(required) = s["required"].as_array() {
            for k in required.iter().filter_map(Value::as_str) {
                out.insert(
                    k.into(),
                    candidate(&s["properties"][k], k, optional, remaining)?,
                );
            }
        }
        if optional && let Some(properties) = s["properties"].as_object() {
            for (k, field) in properties.iter().take(5) {
                if !out.contains_key(k)
                    && let Some(v) = candidate(field, k, optional, remaining)
                {
                    out.insert(k.into(), v);
                }
            }
        }
        return Some(Value::Object(out));
    }
    if let Some(option) = ["anyOf", "oneOf", "allOf"]
        .iter()
        .find_map(|k| s[*k].get(0))
    {
        return candidate(option, key, optional, remaining);
    }
    match kind {
        Some("array") => {
            let n = s["minItems"].as_u64().unwrap_or(0);
            if n > 16 {
                return None;
            }
            (0..n)
                .map(|_| candidate(&s["items"], key, optional, remaining))
                .collect::<Option<Vec<_>>>()
                .map(Value::Array)
        }
        Some("boolean") => Some(json!(false)),
        Some("number" | "integer") => {
            let step = s["multipleOf"].as_f64().unwrap_or(1.);
            let lower = s["minimum"]
                .as_f64()
                .or_else(|| s["exclusiveMinimum"].as_f64().map(|v| v + step));
            let upper = s["maximum"]
                .as_f64()
                .or_else(|| s["exclusiveMaximum"].as_f64().map(|v| v - step));
            let n = lower
                .map(|v| (v / step).ceil() * step)
                .unwrap_or_else(|| upper.map_or(0., |v| (v.min(0.) / step).floor() * step));
            serde_json::Number::from_f64(n).map(Value::Number)
        }
        Some("null") => Some(Value::Null),
        Some("string") => {
            let base = match s["format"].as_str() {
                Some("uri" | "uri-reference") => "https://example.invalid/",
                Some("date-time") => "2030-01-01T00:00:00Z",
                Some("date") => "2030-01-01",
                Some("email") => "user@example.invalid",
                Some("uuid") => "00000000-0000-4000-8000-000000000000",
                Some("hostname") => "example.invalid",
                Some("ipv4") => "192.0.2.1",
                Some("ipv6") => "2001:db8::1",
                _ if matches!(key, "sha" | "gitSha") => "0000000000000000000000000000000000000000",
                _ => "example",
            };
            let min = s["minLength"].as_u64().unwrap_or(0);
            if min > 1024 {
                return None;
            }
            let mut value: String = base
                .chars()
                .take(s["maxLength"].as_u64().unwrap_or(1024).min(1024) as usize)
                .collect();
            value.extend(std::iter::repeat_n(
                'x',
                (min as usize).saturating_sub(value.len()),
            ));
            Some(Value::String(value))
        }
        _ => Some(json!({})),
    }
}

impl SchemaPage {
    fn new(s: &Schema) -> Result<Self> {
        let mut example = None;
        for optional in [true, false] {
            if let Some(data) = candidate(&s.schema, "", optional, &mut 72)
                && data.is_object()
                && s.accepts_sample(&data)?
            {
                let body = format!(
                    "```json#{}\n{}\n```",
                    s.id,
                    serde_json::to_string_pretty(&data)?
                );
                if body.encode_utf16().count() <= 9_800 {
                    example = Some(body);
                    break;
                }
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
    fn schema_buttons_copy_exact_definition_and_valid_fenced_block_through_rune() {
        let db = Board::open(":memory:").unwrap();
        let schema = db.schema("git-commit", 1).unwrap();
        let expected = SchemaPage::new(&schema).unwrap().example.unwrap();
        let m = db
            .post("scratch", "tester", &expected, &Default::default())
            .unwrap();
        assert_eq!(m.blocks.len(), 1);
        let mut state = State::opened(Ok((db, None)));
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
