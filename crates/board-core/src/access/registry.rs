use super::*;
use crate::{core::validate_client_key, errors::Kind::Conflict, validate_actor};
use rusqlite::Transaction;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Grant {
    pub id: i64,
    pub subject: String,
    pub can: Rules,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GrantSpec {
    pub subject: String,
    pub can: Rules,
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GrantBatch {
    pub id: i64,
    pub grants: Vec<Grant>,
}
#[derive(Serialize, Deserialize)]
struct BatchRecord {
    key: Option<String>,
    grants: Vec<Grant>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Credential {
    pub id: String,
    pub actor: String,
    pub scope: Option<Rules>,
    pub expires: Option<i64>,
    pub revoked: bool,
}
#[derive(Serialize)]
pub struct Issued {
    pub credential: Credential,
    pub token: String,
}

fn grant(c: &Connection, id: i64) -> Result<Grant> {
    let (subject, can) = c
        .query_row(
            "SELECT subject,rules FROM access_grants WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?
        .context(Missing.error("grant_not_found", "grant not found"))?;
    Ok(Grant {
        id,
        subject,
        can: serde_json::from_str(&can)?,
    })
}

fn set_on(
    tx: &Transaction<'_>,
    owner: &str,
    id: Option<i64>,
    subject: &str,
    can: &Rules,
) -> Result<Grant> {
    if subject != "*" {
        validate_actor(subject)?;
    }
    can.validate()?;
    let id = if let Some(id) = id {
        let old = grant(tx, id)?;
        if old.subject == subject && old.can == *can {
            return Ok(old);
        }
        tx.execute(
            "UPDATE access_grants SET subject=?,rules=? WHERE id=?",
            params![subject, serde_json::to_string(can)?, id],
        )?;
        id
    } else {
        tx.execute(
            "INSERT INTO access_grants(subject,rules) VALUES(?,?)",
            params![subject, serde_json::to_string(can)?],
        )?;
        tx.last_insert_rowid()
    };
    Board::event_tx(
        tx,
        "grant.changed",
        owner,
        None,
        serde_json::json!({"id":id}),
    )?;
    Ok(Grant {
        id,
        subject: subject.to_owned(),
        can: can.clone(),
    })
}
fn credential(c: &Connection, id: &str) -> Result<Credential> {
    let (actor, scope, expires, revoked) = c
        .query_row(
            "SELECT actor,scope,expires,revoked FROM access_credentials WHERE id=?",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get(2)?,
                    r.get(3)?,
                ))
            },
        )
        .optional()?
        .context(Missing.error("credential_not_found", "credential not found"))?;
    Ok(Credential {
        id: id.into(),
        actor,
        scope: scope.map(|s| serde_json::from_str(&s)).transpose()?,
        expires,
        revoked,
    })
}
impl Board {
    pub fn grant(&self, id: i64) -> Result<Grant> {
        self.require_owner()?;
        grant(&self.conn, id)
    }
    pub fn credential(&self, id: &str) -> Result<Credential> {
        self.require_owner()?;
        credential(&self.conn, id)
    }
    pub fn grants(&self) -> Result<Vec<Grant>> {
        let tx = self.conn.unchecked_transaction()?;
        self.access_on(&tx)?.trusted()?;
        let ids = tx
            .prepare("SELECT id FROM access_grants ORDER BY id")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        let result = ids
            .into_iter()
            .map(|id| grant(&tx, id))
            .collect::<Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(result)
    }
    pub fn set_grant(
        &self,
        owner: &str,
        id: Option<i64>,
        subject: &str,
        can: &Rules,
    ) -> Result<Grant> {
        validate_actor(owner)?;
        self.write(|tx| {
            self.access_on(tx)?.trusted()?;
            set_on(tx, owner, id, subject, can)
        })
    }

    /// The admin event is also the receipt: its cursor addresses the immutable
    /// original grants, including after an individual grant changes or is revoked.
    pub fn create_grant_batch(
        &self,
        owner: &str,
        grants: &[GrantSpec],
        key: Option<&str>,
    ) -> Result<(GrantBatch, bool)> {
        self.write(|tx| {
            self.access_on(tx)?.trusted()?;
            validate_actor(owner)?;
            ensure!(
                (1..=16).contains(&grants.len()),
                Invalid.error("invalid_batch", "provide 1 to 16 grants")
            );
            if let Some(key) = key {
                validate_client_key(key)?;
                let prior: Option<(i64, String, String)> = tx
                    .query_row(
                        "SELECT cursor,actor,payload FROM events WHERE kind='grant.batch_created'
                     AND json_extract(payload,'$.key')=? ORDER BY cursor LIMIT 1",
                        [key],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?;
                if let Some((id, actor, data)) = prior {
                    let record: BatchRecord = serde_json::from_str(&data)?;
                    ensure!(
                        actor == owner
                            && record.grants.len() == grants.len()
                            && record
                                .grants
                                .iter()
                                .zip(grants)
                                .all(|(old, new)| old.subject == new.subject && old.can == new.can),
                        Conflict
                            .error("duplicate_id", "grant batch key belongs to another request")
                    );
                    return Ok((
                        GrantBatch {
                            id,
                            grants: record.grants,
                        },
                        false,
                    ));
                }
            }
            let grants = grants
                .iter()
                .map(|spec| set_on(tx, owner, None, &spec.subject, &spec.can))
                .collect::<Result<Vec<_>>>()?;
            let id = Self::event_tx(
                tx,
                "grant.batch_created",
                owner,
                None,
                serde_json::json!({"key":key,"grants":grants}),
            )?;
            Ok((GrantBatch { id, grants }, true))
        })
    }

    pub fn grant_batch(&self, id: i64) -> Result<GrantBatch> {
        let tx = self.conn.unchecked_transaction()?;
        self.access_on(&tx)?.trusted()?;
        let data: String = tx
            .query_row(
                "SELECT payload FROM events WHERE cursor=? AND kind='grant.batch_created'",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .context(Missing.error("grant_batch_not_found", "grant batch not found"))?;
        let record: BatchRecord = serde_json::from_str(&data)?;
        tx.commit()?;
        Ok(GrantBatch {
            id,
            grants: record.grants,
        })
    }
    pub fn revoke_grant(&self, owner: &str, id: i64) -> Result<()> {
        validate_actor(owner)?;
        self.write(|tx| {
            self.access_on(tx)?.trusted()?;
            grant(tx, id)?;
            tx.execute("DELETE FROM access_grants WHERE id=?", [id])?;
            Board::event_tx(
                tx,
                "grant.revoked",
                owner,
                None,
                serde_json::json!({"id":id}),
            )?;
            Ok(())
        })
    }
    pub fn credentials(&self) -> Result<Vec<Credential>> {
        let tx = self.conn.unchecked_transaction()?;
        self.access_on(&tx)?.trusted()?;
        let ids = tx
            .prepare("SELECT id FROM access_credentials ORDER BY rowid")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let result = ids
            .into_iter()
            .map(|id| credential(&tx, &id))
            .collect::<Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(result)
    }
    pub fn issue_credential(
        &self,
        owner: &str,
        actor: &str,
        scope: Option<&Rules>,
        ttl: Option<u64>,
    ) -> Result<Issued> {
        validate_actor(owner)?;
        validate_actor(actor)?;
        if let Some(scope) = scope {
            scope.validate()?;
        }
        ensure!(
            ttl.is_none_or(|s| (1..=365 * 86_400).contains(&s)),
            Invalid.error("invalid_ttl", "TTL must be 1 second to 365 days")
        );
        self.write(|tx|{self.access_on(tx)?.trusted()?;
            let (id,token):(String,String)=tx.query_row("SELECT lower(hex(randomblob(16))),'bd1_'||lower(hex(randomblob(32)))",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let digest=Sha256::digest(token.as_bytes()).to_vec();
            tx.execute("INSERT INTO access_credentials(id,digest,actor,scope,expires) VALUES(?,?,?,?,unixepoch()+?)",
                params![id,digest,actor,scope.map(serde_json::to_string).transpose()?,ttl.map(|n|n as i64)])?;
            Board::event_tx(tx,"credential.issued",owner,None,serde_json::json!({"id":id}))?;
            Ok(Issued{credential:credential(tx,&id)?,token})
        })
    }
    pub fn revoke_credential(&self, owner: &str, id: &str) -> Result<()> {
        validate_actor(owner)?;
        self.write(|tx| {
            self.access_on(tx)?.trusted()?;
            if !credential(tx, id)?.revoked {
                tx.execute("UPDATE access_credentials SET revoked=1 WHERE id=?", [id])?;
                Board::event_tx(
                    tx,
                    "credential.revoked",
                    owner,
                    None,
                    serde_json::json!({"id":id}),
                )?;
            }
            Ok(())
        })
    }
    /// Effective permissions for a local actor before credential-specific narrowing.
    pub fn explain(&self, actor: &str, channel: &str) -> Result<Vec<Action>> {
        validate_actor(actor)?;
        validate_channel(channel)?;
        let tx = self.conn.unchecked_transaction()?;
        let current = self.access_on(&tx)?;
        let policy = if current.is_trusted() {
            let base = actor.split('/').next().unwrap_or(actor);
            let rules = tx
                .prepare("SELECT rules FROM access_grants WHERE subject IN ('*',?,?)")?
                .query_map([actor, base], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Access {
                id: Some(String::new()),
                actor: Some(actor.to_owned()),
                grants: rules
                    .iter()
                    .map(|s| serde_json::from_str(s))
                    .collect::<serde_json::Result<_>>()?,
                caps: vec![],
                extra_cap: false,
            }
        } else {
            current.actor(actor)?;
            current
        };
        let answer = ALL
            .into_iter()
            .filter(|a| policy.allows(*a, channel))
            .collect();
        tx.commit()?;
        Ok(answer)
    }
}
