use super::*;
impl Board {
    pub fn source_renderer(&self, who: &str, name: &str) -> Result<Option<String>> {
        let tx = self.conn.unchecked_transaction()?;
        self.access_on(&tx)?.trusted()?;
        self.authorized_source(&tx, name, who)?;
        let body = tx
            .query_row(
                "SELECT script FROM sync_renderers WHERE source=?",
                [name],
                |r| r.get(0),
            )
            .optional()?;
        tx.commit()?;
        Ok(body)
    }
    pub fn set_source_renderer(&self, who: &str, name: &str, body: Option<&str>) -> Result<bool> {
        self.require_owner()?;
        if let Some(script) = body {
            ensure!(
                !script.trim().is_empty() && script.len() <= 8192,
                "renderer source must contain 1..8192 bytes"
            );
            let _ast = crate::policy::engine()
                .compile(script)
                .map_err(|e| Kind::Invalid.error("invalid_rule", e.to_string()))?;
        }
        self.write(|tx|{self.access_on(tx)?.trusted()?;let s=self.authorized_source(tx,name,who)?;
            let old:Option<String>=tx.query_row("SELECT script FROM sync_renderers WHERE source=?",[name],|r|r.get(0)).optional()?;
            if old.as_deref()==body{return Ok(false);}
            if let Some(script)=body{tx.execute("INSERT INTO sync_renderers VALUES(?,?) ON CONFLICT(source) DO UPDATE SET script=excluded.script",params![name,script])?;}else{tx.execute("DELETE FROM sync_renderers WHERE source=?",[name])?;}
            Self::event_tx(tx,"source.configured",who,None,json!({"source":name,"channel":s.channel,"renderer":body.is_some()}))?;Ok(old.is_none()&&body.is_some())})
    }
}
pub(super) fn body(
    tx: &Transaction<'_>,
    source: &Source,
    thread: &str,
    m: &Message,
) -> Result<String> {
    let Some(script) = tx
        .query_row(
            "SELECT script FROM sync_renderers WHERE source=?",
            [&source.name],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(m.text.clone());
    };
    let mut scope = rhai::Scope::new();
    let data = json!({"id":m.id,"root":m.re.unwrap_or(m.id),"channel":m.channel,"actor":m.actor,"text":m.text,"labels":m.labels,"source":source.name,"thread":thread,"files":m.attachments,"task_state":m.task.as_ref().map(|t|&t.state)});
    scope.push_constant(
        "message",
        rhai::serde::to_dynamic(data).map_err(|_| anyhow::anyhow!("invalid renderer context"))?,
    );
    let s = crate::policy::engine()
        .eval_with_scope::<String>(&mut scope, &script)
        .map_err(|_| {
            Kind::Invalid.error(
                "renderer_error",
                "renderer must return text within its evaluation budget",
            )
        })?;
    ensure!(
        s.encode_utf16().count() <= 10_000,
        "renderer exceeded the message limit"
    );
    ensure!(
        !s.trim().is_empty() || !m.attachments.is_empty(),
        "renderer returned empty text for a message with no files"
    );
    Ok(s)
}
