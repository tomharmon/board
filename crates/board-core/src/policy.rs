//! Owner-installed offline middleware. Only installed source code is evaluated, never a message as code.
use crate::{Board, PostOptions, errors::Kind};
use anyhow::{Result, ensure};
use rhai::{Dynamic, Engine, Scope};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

pub(crate) fn init(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS write_rules(name TEXT PRIMARY KEY,priority INTEGER NOT NULL,script TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS rule_internal(id INTEGER PRIMARY KEY CHECK(id=1));")?;
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Rule {
    pub name: String,
    pub priority: i64,
    pub script: String,
}
pub(crate) fn engine() -> Engine {
    let mut e = Engine::new();
    e.set_max_operations(30_000)
        .set_max_call_levels(8)
        .set_max_variables(64)
        .set_max_functions(8)
        .set_max_expr_depths(32, 16)
        .set_max_array_size(512)
        .set_max_map_size(512)
        .set_max_string_size(64 * 1024);
    e.disable_symbol("eval")
        .disable_symbol("import")
        .on_print(|_| {})
        .on_debug(|_, _, _| {});
    e
}
fn rows(c: &Connection) -> Result<Vec<Rule>> {
    Ok(
        c.prepare("SELECT name,priority,script FROM write_rules ORDER BY priority,name")?
            .query_map([], |r| {
                Ok(Rule {
                    name: r.get(0)?,
                    priority: r.get(1)?,
                    script: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?,
    )
}
impl Board {
    pub fn write_rules(&self) -> Result<Vec<Rule>> {
        let tx = self.conn.unchecked_transaction()?;
        self.access_on(&tx)?.trusted()?;
        let r = rows(&tx)?;
        tx.commit()?;
        Ok(r)
    }
    pub fn set_write_rule(&self, who: &str, rule: &Rule) -> Result<bool> {
        self.require_owner()?;
        crate::validate_actor(who)?;
        ensure!(
            !rule.name.is_empty()
                && rule.name.len() <= 64
                && rule
                    .name
                    .bytes()
                    .all(|x| x.is_ascii_alphanumeric() || matches!(x, b'-' | b'_')),
            "invalid rule name"
        );
        ensure!(
            !rule.script.trim().is_empty() && rule.script.len() <= 8192,
            "a rule must contain 1..8192 bytes of source"
        );
        let _ast = engine()
            .compile(&rule.script)
            .map_err(|e| Kind::Invalid.error("invalid_rule", e.to_string()))?;
        self.write(|tx|{
            self.access_on(tx)?.trusted()?;
            let old:Option<(String,i64)>=tx.query_row("SELECT script,priority FROM write_rules WHERE name=?",[&rule.name],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if old.as_ref().is_some_and(|(script,n)|script==&rule.script&&*n==rule.priority){return Ok(false);}
            ensure!(old.is_some()||tx.query_row("SELECT count(*) FROM write_rules",[],|r|r.get::<_,i64>(0))?<16,"at most 16 installed rules");
            tx.execute("INSERT INTO write_rules VALUES(?,?,?) ON CONFLICT(name) DO UPDATE SET priority=excluded.priority,script=excluded.script",params![rule.name,rule.priority,rule.script])?;
            Self::event_tx(tx,"rule.changed",who,None,json!({"name":rule.name}))?;Ok(old.is_none())
        })
    }
    pub fn remove_write_rule(&self, who: &str, name: &str) -> Result<()> {
        self.write(|tx| {
            self.access_on(tx)?.trusted()?;
            if tx.execute("DELETE FROM write_rules WHERE name=?", [name])? != 0 {
                Self::event_tx(tx, "rule.removed", who, None, json!({"name":name}))?;
            }
            Ok(())
        })
    }
}
#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Decision {
    text: Option<String>,
    labels: Option<BTreeMap<String, String>>,
    drop: Option<String>,
    respond: Option<String>,
    emit: Vec<Effect>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Effect {
    channel: Option<String>,
    reply_to: Option<i64>,
    text: String,
    #[serde(default)]
    no_sync: bool,
}
pub(crate) struct Write<'a> {
    pub channel: &'a str,
    pub actor: &'a str,
    pub id: Option<i64>,
    pub root: Option<i64>,
    pub text: &'a str,
    pub labels: &'a BTreeMap<String, String>,
    pub attached: bool,
    pub no_sync: bool,
}
pub(crate) type Patch = (String, BTreeMap<String, String>);
pub(crate) fn filter(tx: &Transaction<'_>, input: Write<'_>) -> Result<Option<Patch>> {
    if tx.query_row(
        "SELECT NOT EXISTS(SELECT 1 FROM write_rules) OR EXISTS(SELECT 1 FROM rule_internal)",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(None);
    }
    let src: Option<(String, String, String)> = tx
        .query_row(
            "SELECT source,thread,external FROM sync_import_context WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (mut text, mut labels) = (input.text.to_owned(), input.labels.clone());
    let mut effects = Vec::new();
    let private = input.no_sync
        || tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM private_messages WHERE id=?)",
            [input.root],
            |r| r.get::<_, bool>(0),
        )?;
    let e = engine();
    for rule in rows(tx)? {
        let context = json!({"operation":if input.id.is_some(){"edit"}else{"post"},"channel":input.channel,"actor":input.actor,"id":input.id,"root":input.root,
            "text":text,"labels":labels,"no_sync":private,"has_files":input.attached,"source":src.as_ref().map(|(s,t,m)|json!({"key":s,"thread":t,"message":m}))});
        let mut scope = Scope::new();
        scope.push_constant(
            "write",
            rhai::serde::to_dynamic(context)
                .map_err(|_| anyhow::anyhow!("invalid write context"))?,
        );
        let out = e
            .eval_with_scope::<Dynamic>(&mut scope, &rule.script)
            .map_err(|_| {
                Kind::Invalid.error(
                    "rule_error",
                    format!("write rule {} failed or exceeded its budget", rule.name),
                )
            })?;
        if out.is_unit() {
            continue;
        }
        let patch: Decision = rhai::serde::from_dynamic(&out).map_err(|_| {
            Kind::Invalid.error(
                "rule_error",
                format!("invalid result from rule {}", rule.name),
            )
        })?;
        if let Some(reason) = patch.drop {
            let response = patch
                .respond
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.chars().take(1000).collect::<String>());
            let detail = response
                .as_deref()
                .unwrap_or(&reason)
                .chars()
                .take(1000)
                .collect::<String>();
            return Err(Kind::Invalid
                .error("dropped", detail)
                .with_response(response)
                .into());
        }
        ensure!(patch.respond.is_none(), "respond is only valid with drop");
        if let Some(v) = patch.text {
            text = v;
        }
        if let Some(v) = patch.labels {
            ensure!(input.id.is_none(), "rules cannot relabel edits");
            labels = v;
        }
        effects.extend(patch.emit);
        ensure!(
            effects.len() <= 8,
            "at most eight middleware outputs per write"
        );
        if !input.attached && text.trim().is_empty() {
            return Err(Kind::Invalid
                .error("dropped", "write removed by local rule")
                .into());
        }
    }
    if !effects.is_empty() {
        tx.execute("INSERT INTO rule_internal VALUES(1)", [])?;
        // All effects roll back if the triggering write or another effect is rejected.
        for action in effects {
            let opts = PostOptions {
                no_sync: action.no_sync || private,
                ..Default::default()
            };
            match (action.channel, action.reply_to) {
                (Some(ch), None) => {
                    Board::post_tx(tx, &ch, "board:middleware", &action.text, &opts)?
                }
                (None, Some(id)) => {
                    Board::reply_tx(tx, id, "board:middleware", &action.text, &opts)?
                }
                _ => {
                    return Err(anyhow::anyhow!(
                        "rule output needs exactly one channel or reply target"
                    ));
                }
            };
        }
        tx.execute("DELETE FROM rule_internal WHERE id=1", [])?;
    }
    Ok(Some((text, labels)))
}
