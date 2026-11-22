use crate::Board;
use crate::errors::Kind::{Conflict, Invalid};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;
mod formats;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Block {
    pub schema: String,
    pub version: u32,
    pub data: Value,
}

/// Keep the requested card outside any unclosed Markdown example in the accompanying prose.
pub fn append_block(text: &str, name: &str, version: u32, data: &Value) -> Result<String> {
    validate_id(name, version)?;
    let fence = format!("```json#{name}@{version}\n{data}\n```");
    if text.is_empty() {
        return Ok(fence);
    }
    let candidate = format!("{text}\n\n{fence}");
    if crate::prose::fences(&candidate)?
        .last()
        .is_some_and(|f| f.start == text.len() + 2)
    {
        Ok(candidate)
    } else {
        Ok(format!("{fence}\n\n{text}"))
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub description: String,
    pub schema: Value,
    #[serde(rename = "builtIn")]
    pub built_in: bool,
    pub by: Option<String>,
    pub at: Option<String>,
}

impl Schema {
    pub fn valid_examples(&self) -> Result<impl Iterator<Item = &Value>> {
        let validator = formats::validator(&local_definition(self.schema.clone()))?;
        Ok(self.schema["examples"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(move |v| v.is_object() && validator.is_valid(v)))
    }

    /// Check an example block against this immutable registered version, without publishing it.
    pub fn accepts_sample(&self, data: &Value) -> Result<bool> {
        Ok(data.is_object()
            && formats::validator(&local_definition(self.schema.clone()))?.is_valid(data))
    }
}

pub(crate) fn init(c: &Connection) -> Result<()> {
    let schema = json!({"type":"object","additionalProperties":false,"required":["sha","title"],"properties":{
 "sha":{"type":"string","pattern":"^(?:[0-9a-f]{40}|[0-9a-f]{64})$"},"title":{"type":"string","minLength":1,"maxLength":1024}}});
    let deployment = json!({"type":"object","required":["url"],"additionalProperties":false,
        "properties":{"url":{"type":"string","format":"uri","pattern":"^https?://[^/\\s?#]+","maxLength":2048},
        "gitSha":{"type":"string","pattern":"^(?:[0-9a-f]{40}|[0-9a-f]{64})$"}}});
    let verdict_status = json!({"type":"string","enum":["pass","fail","not-run"]});
    let string = json!({"type":"string"});
    let verdict = json!({"type":"object","required":["status","summary"],"properties":{
        "status":verdict_status,"artifact":string,"summary":string,"checks":{"type":"array","items":{"type":"object",
        "required":["name","status"],"properties":{"name":string,"status":verdict_status,"command":string,"output":string}}}}});
    let diff = json!({"type":"object","required":["patch"],"properties":{"base":string,"head":string,
        "files":{"type":"array","items":string},"patch":string}});
    for (name, description, definition) in [
        ("git-commit", "A commit hash and its title.", schema),
        (
            "deployment",
            "A deployment address and optional source commit.",
            deployment,
        ),
        (
            "verdict",
            "Evidence for a check against a specific artifact.",
            verdict,
        ),
        (
            "diff",
            "A unified patch with optional revisions and file names.",
            diff,
        ),
    ]
    .into_iter()
    .chain(crate::swarm::coordination_schemas())
    {
        c.execute(
            "INSERT INTO schemas(name,version,description,definition,builtin) VALUES(?,1,?,?,1)",
            params![name, description, definition.to_string()],
        )?;
    }
    Ok(())
}

fn validate_id(name: &str, version: u32) -> Result<()> {
    ensure!(
        name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
            && name.len() <= 80
            && name.bytes().all(|b| b.is_ascii_lowercase()
                || b.is_ascii_digit()
                || matches!(b, b'.' | b'_' | b'-'))
            && (1..=99999).contains(&version),
        Invalid.error(
            "invalid_schema_id",
            format!("invalid schema ID: {name}@{version}")
        )
    );
    Ok(())
}
const PROJECTION: &str = "SELECT name,version,description,definition,builtin,actor,at FROM schemas";
fn schema_row(r: &rusqlite::Row<'_>) -> Result<Schema> {
    let name: String = r.get(0)?;
    let version = r.get(1)?;
    let mut schema: Value = serde_json::from_str(r.get_ref(3)?.as_str()?)?;
    let built_in: bool = r.get(4)?;
    // Examples are presentation hints, not part of the stored validation rules.
    if built_in
        && version == 1
        && let Some(sample) = builtin_example(&name)
    {
        schema["examples"] = json!([sample]);
    }
    Ok(Schema {
        id: format!("{name}@{version}"),
        name,
        version,
        description: r.get(2)?,
        schema,
        built_in,
        by: r.get(5)?,
        at: r.get(6)?,
    })
}
fn builtin_example(name: &str) -> Option<Value> {
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let revision = || json!({"kind":"git-commit","value":sha});
    Some(match name {
        "git-commit" => json!({"sha":sha,"title":"Describe the change"}),
        "deployment" => json!({"url":"https://example.org/releases/1"}),
        "verdict" => json!({"status":"not-run","summary":"Verification pending"}),
        "diff" => json!({"patch":"@@ -1 +1 @@\n-before\n+after"}),
        "handoff" => {
            json!({"revision":revision(),"summary":"Ready for review","changes":["Describe the change"],"to":"reviewer"})
        }
        "verification" => {
            json!({"handoff":1,"revision":revision(),"summary":"Verification pending","checks":[{"name":"tests","status":"not-run","command":"cargo test"}]})
        }
        "decision-request" => {
            json!({"to":"coordinator","summary":"Which release should include this change?"})
        }
        "decision-resolution" => json!({"request":1,"summary":"Use the next release."}),
        _ => return None,
    })
}
fn check_shape(v: &Value, depth: usize, count: &mut usize) -> Result<()> {
    if v.is_boolean() {
        return Ok(());
    }
    *count += 1;
    ensure!(depth <= 16 && *count <= 64, "schema too complex");
    let obj = v.as_object().context("schema nodes must be objects")?;
    for (k, val) in obj {
        match k.as_str() {
            "properties" => {
                for def in val.as_object().context("invalid properties")?.values() {
                    check_shape(def, depth + 1, count)?;
                }
            }
            "items" | "not" | "additionalProperties" => check_shape(val, depth + 1, count)?,
            "oneOf" | "anyOf" | "allOf" => {
                let alternatives = val
                    .as_array()
                    .filter(|xs| !xs.is_empty())
                    .with_context(|| format!("{k} needs at least one subschema"))?;
                for def in alternatives {
                    check_shape(def, depth + 1, count)?;
                }
            }
            "type" => {
                let valid = |s: &str| {
                    matches!(
                        s,
                        "object" | "array" | "string" | "number" | "integer" | "boolean" | "null"
                    )
                };
                ensure!(
                    val.as_str().is_some_and(valid)
                        || val
                            .as_array()
                            .is_some_and(|xs| !xs.is_empty() && distinct_names(xs, valid)),
                    "type must name one or more distinct JSON types"
                );
            }
            "required" => ensure!(
                val.as_array()
                    .is_some_and(|xs| distinct_names(xs, |_| true)),
                "required must be a list of distinct property names"
            ),
            "minItems" | "maxItems" | "minLength" | "maxLength" => ensure!(
                val.as_f64()
                    .is_some_and(|n| n.is_finite() && n >= 0.0 && n.fract() == 0.0),
                "{k} must be a nonnegative integer"
            ),
            "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" => ensure!(
                val.as_f64().is_some_and(f64::is_finite),
                "{k} must be a finite number"
            ),
            "multipleOf" => ensure!(
                val.as_f64().is_some_and(|n| n.is_finite() && n > 0.0),
                "multipleOf must be a positive number"
            ),
            "enum" => ensure!(
                val.as_array().is_some_and(|xs| !xs.is_empty()),
                "enum needs at least one value"
            ),
            "const" => (),
            "examples" => ensure!(val.is_array(), "examples must be an array"),
            "$schema" | "title" | "description" => ensure!(val.is_string(), "{k} must be text"),
            "format" => ensure!(
                matches!(
                    val.as_str(),
                    Some(
                        "date-time"
                            | "date"
                            | "time"
                            | "email"
                            | "uri"
                            | "uuid"
                            | "hostname"
                            | "ipv4"
                            | "ipv6"
                    )
                ),
                "unknown format"
            ),
            other => bail!("unsupported schema keyword: {other}"),
        }
    }
    Ok(())
}
fn distinct_names(xs: &[Value], valid: impl Fn(&str) -> bool) -> bool {
    let mut seen = BTreeSet::new();
    xs.iter()
        .all(|v| v.as_str().is_some_and(|s| valid(s) && seen.insert(s)))
}

// JSON has a single number type. Compare integral doubles to an integer without
// rounding the integer through f64 (which would merge distinct IDs above 2^53).
fn same_definition(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(l), Value::Number(r)) if l.is_f64() != r.is_f64() => {
            let (float, int) = if l.is_f64() { (l, r) } else { (r, l) };
            let Some(n) = float.as_f64() else {
                return false;
            };
            n.fract() == 0.0
                && int
                    .as_i64()
                    .map(i128::from)
                    .or_else(|| int.as_u64().map(i128::from))
                    == Some(n as i128)
        }
        (Value::Array(l), Value::Array(r)) => {
            l.len() == r.len() && l.iter().zip(r).all(|(x, y)| same_definition(x, y))
        }
        (Value::Object(l), Value::Object(r)) => {
            l.len() == r.len()
                && l.iter()
                    .all(|(k, v)| r.get(k).is_some_and(|u| same_definition(v, u)))
        }
        _ => a == b,
    }
}

fn check_definition(schema: &Value) -> Result<()> {
    ensure!(
        schema.get("type").and_then(Value::as_str) == Some("object"),
        "schema must describe an object"
    );
    budget(schema, 0, &mut 0)?;
    ensure!(
        serde_json::to_vec(schema)?.len() <= 32768,
        "schema exceeds 32768 bytes"
    );
    check_shape(schema, 0, &mut 0)?;
    let compiled = local_definition(schema.clone());
    formats::validator(&compiled)?;
    Ok(())
}
fn budget(v: &Value, depth: usize, nodes: &mut usize) -> Result<()> {
    *nodes += 1;
    ensure!(
        *nodes <= 512 && depth <= 16,
        "schema exceeds 512 nodes or 16 levels"
    );
    match v {
        Value::String(s) => ensure!(!s.contains('\0'), "invalid NUL in schema"),
        Value::Object(o) => {
            for (k, v) in o {
                ensure!(!k.contains('\0'), "invalid NUL in schema key");
                budget(v, depth + 1, nodes)?;
            }
        }
        Value::Array(xs) => {
            for x in xs {
                budget(x, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}
// A dialect annotation is display data here, never an instruction to fetch a remote schema.
// Only strip annotations in subschemas; literal "const" and "enum" data stay byte-for-byte meaningful.
fn local_definition(mut value: Value) -> Value {
    fn strip(v: &mut Value) {
        if let Some(o) = v.as_object_mut() {
            o.remove("$schema");
            for (key, sub) in o.iter_mut() {
                match key.as_str() {
                    "properties" => {
                        if let Some(props) = sub.as_object_mut() {
                            for p in props.values_mut() {
                                strip(p);
                            }
                        }
                    }
                    "items" | "additionalProperties" | "not" => strip(sub),
                    "oneOf" | "anyOf" | "allOf" => {
                        if let Some(arr) = sub.as_array_mut() {
                            for p in arr {
                                strip(p);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    strip(&mut value);
    value
}
impl Board {
    pub fn register_schema(
        &self,
        name: &str,
        version: u32,
        description: &str,
        schema: &Value,
    ) -> Result<bool> {
        self.register_schema_as(
            name,
            version,
            description,
            schema,
            &crate::local::actor_name(),
        )
    }
    pub fn register_schema_as(
        &self,
        name: &str,
        version: u32,
        description: &str,
        schema: &Value,
        actor: &str,
    ) -> Result<bool> {
        self.write(|tx| {
            let a=self.access_on(tx)?;a.actor(actor)?;
            ensure!(a.manage_at("*",false),crate::errors::Kind::Forbidden.error("missing_permission","schema registration requires manage at *"));
            crate::validate_actor(actor)?;
            validate_id(name, version)?;
            ensure!(description.encode_utf16().count() <= 2000,
                Invalid.error("invalid_schema", "schema description exceeds 2000 characters"));
            let reserved:bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM schemas WHERE name=? AND builtin)", [name], |r| r.get(0))?;
            ensure!(!reserved, Conflict.error("reserved_schema", "reserved schema name"));
            check_definition(schema).map_err(|e|Invalid.error("invalid_schema",e.to_string()))?;
            if let Some((d, def)) = tx
                .query_row(
                    "SELECT description,definition FROM schemas WHERE name=? AND version=?",
                    params![name, version],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                )
                .optional()?
            {
                ensure!(
                    d == description && same_definition(&serde_json::from_str::<Value>(&def)?,schema),
                    Conflict.error("schema_immutable", "register a new version")
                );
                return Ok(false);
            }
            let count: u32 = tx.query_row("SELECT count(*) FROM schemas WHERE NOT builtin", [], |r| r.get(0))?;
            ensure!(count < 500, Invalid.error("too_many_schemas", "at most 500 registered versions"));
            let cursor = Board::event_tx(
                tx,
                "schema.registered",
                actor,
                None,
                json!({"name":name,"version":version}),
            )?;
            tx.execute("INSERT INTO schemas(name,version,description,definition,builtin,actor,at) SELECT ?,?,?,?,0,actor,created_at FROM events WHERE cursor=?", params![name,version,description,schema.to_string(),cursor])?;
            Ok(true)
        })
    }
    pub fn schemas(&self) -> Result<Vec<Schema>> {
        let tx = self.conn.unchecked_transaction()?;
        self.access_on(&tx)?;
        let mut stmt = tx.prepare(&format!(
            "{PROJECTION} ORDER BY builtin DESC, (name='git-commit') DESC, name, version"
        ))?;
        let mut rs = stmt.query([])?;
        let mut out = vec![];
        while let Some(r) = rs.next()? {
            out.push(schema_row(r)?);
        }
        drop(rs);
        drop(stmt);
        tx.commit()?;
        Ok(out)
    }
    pub fn schema(&self, name: &str, version: u32) -> Result<Schema> {
        validate_id(name, version)?;
        let tx = self.conn.unchecked_transaction()?;
        self.access_on(&tx)?;
        let result = {
            let mut stmt =
                tx.prepare_cached(&format!("{PROJECTION} WHERE name=? AND version=?"))?;
            let mut rows = stmt.query(params![name, version])?;
            schema_row(rows.next()?.context(
                crate::errors::Kind::Missing.error("schema_not_found", "schema not found"),
            )?)?
        };
        tx.commit()?;
        Ok(result)
    }
    pub(crate) fn blocks_on(c: &Connection, text: &str) -> Result<Vec<Block>> {
        if !text.contains("json#") {
            return Ok(vec![]);
        }
        let mut out = vec![];
        for fence in crate::prose::fences(text)? {
            let (schema, num) = fence
                .name
                .split_once('@')
                .context(Invalid.error("invalid_block", "invalid schema ID in data fence"))?;
            ensure!(
                !num.starts_with('0') && !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit()),
                Invalid.error("invalid_block", "invalid schema version")
            );
            let version = num
                .parse::<u32>()
                .context(Invalid.error("invalid_block", "invalid schema version"))?;
            validate_id(schema, version)
                .context(Invalid.error("invalid_block", "invalid schema ID in data fence"))?;
            let def: String = c
                .query_row(
                    "SELECT definition FROM schemas WHERE name=? AND version=?",
                    params![schema, version],
                    |r| r.get(0),
                )
                .optional()?
                .with_context(|| {
                    Invalid.error(
                        "schema_not_found",
                        format!("unknown schema: {schema}@{version}"),
                    )
                })?;
            let data: Value = serde_json::from_str(fence.data)
                .with_context(|| Invalid.error("invalid_block", "not JSON"))?;
            ensure!(
                data.is_object(),
                Invalid.error("invalid_block", "expected a JSON object")
            );
            let validator = formats::validator(&local_definition(serde_json::from_str(&def)?))?;
            validator.validate(&data).map_err(|e| {
                Invalid.error(
                    "invalid_block",
                    format!("in {schema}@{version} at {}: {e}", e.instance_path),
                )
            })?;
            out.push(Block {
                schema: schema.into(),
                version,
                data,
            });
        }
        Ok(out)
    }
}
