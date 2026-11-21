use super::*;
use crate::errors::Kind::Conflict;

impl Board {
    /// Create an empty channel, a person/family grant and an everyone lock as one write.
    /// Programs trusted with the database file itself are still its administrators.
    pub fn create_private_channel(&self, name: &str, actor: &str) -> Result<Vec<Grant>> {
        validate_channel(name)?;
        crate::validate_actor(actor)?;
        let principal = actor.split('/').next().unwrap_or(actor);
        ensure!(
            principal != "*" && !principal.starts_with("source:"),
            Invalid.error("no_own_principal", "a private channel needs a local person")
        );
        let family = format!("{name}/");
        let allow = Rules([(family.clone(), ALL.into_iter().collect())].into());
        let lock = Rules([(family.clone(), Actions::new())].into());
        self.write(|tx| {
            let before = self.access_on(tx)?;
            before.actor(actor)?;
            before.list_grants()?;
            before.require(Action::Manage, name)?;
            // A wildcard or a less-specific ancestor loses to the new family rule.
            // Exact and descendant rules could still admit someone past the lock.
            let grant: Option<i64> = tx.query_row(
                "SELECT g.id FROM access_grants g,json_each(g.rules) rule
                 WHERE rule.key=?1 OR substr(rule.key,1,length(?2))=?2 ORDER BY g.id LIMIT 1",
                params![name,family], |r| r.get(0)).optional()?;
            if let Some(id) = grant {
                anyhow::bail!(Conflict.error("channel_granted", format!("grant {id} already reaches this channel or its descendants")));
            }
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM channels WHERE name=?1 OR substr(name,1,length(?2))=?2)",
                params![name,family], |r| r.get(0))?;
            ensure!(!exists, Conflict.error("channel_exists", "the channel or one of its descendants already exists"));
            let own = registry::set_on(tx, actor, None, &[principal.to_owned()], &allow)?;
            let closed = registry::set_on(tx, actor, None, &["*".into()], &lock)?;
            policy::authorize_change(self, tx, &before, &[&allow, &lock], &[], false)?;
            let after = self.access_on(tx)?;
            after.require(Action::Read, name)?;
            after.require(Action::Post, name)?;
            tx.execute("INSERT INTO channels(name) VALUES(?)", [name])?;
            Board::event_tx(tx, "channel.created", actor, None, serde_json::json!({"name":name}))?;
            Ok(vec![own, closed])
        })
    }
}
