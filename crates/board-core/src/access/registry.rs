use super::*;
use crate::{core::validate_client_key, errors::Kind::Conflict, validate_actor};
use rusqlite::Transaction;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Grant {
    pub id: i64,
    pub subject: String,
    #[serde(default)]
    pub subjects: Vec<String>,
    pub can: Rules,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GrantSpec {
    pub subject: String,
    pub can: Rules,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct GrantInput {
    pub subject: Option<String>,
    pub subjects: Option<Vec<String>>,
    pub can: Rules,
}
impl GrantInput {
    pub fn parts(self) -> Result<(Vec<String>, Rules)> {
        let subjects = match (self.subject, self.subjects) {
            (Some(s), None) => vec![s],
            (None, Some(v)) => v,
            _ => anyhow::bail!(Invalid.error("invalid_grant", "provide subject or subjects")),
        };
        Ok((subjects, self.can))
    }
}
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GrantBatch {
    pub id: i64,
    pub grants: Vec<Grant>,
}
#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub actions: Vec<Action>,
    pub pattern: Option<String>,
    pub level: Option<String>,
    pub over: Vec<String>,
    pub grants: Vec<i64>,
}
#[derive(Serialize, Deserialize)]
struct BatchRecord {
    key: Option<String>,
    grants: Vec<Grant>,
}
impl BatchRecord {
    fn read(data: &str) -> Result<Self> {
        let mut receipt: Self = serde_json::from_str(data)?;
        for g in &mut receipt.grants {
            if g.subjects.is_empty() {
                g.subjects.push(g.subject.clone());
            }
        }
        Ok(receipt)
    }
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
        subjects: c
            .prepare("SELECT subject FROM access_members WHERE grant_id=? ORDER BY position")?
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?,
        subject,
        can: serde_json::from_str(&can)?,
    })
}

pub(super) fn set_on(
    tx: &Transaction<'_>,
    owner: &str,
    id: Option<i64>,
    members: &[String],
    can: &Rules,
) -> Result<Grant> {
    ensure!(
        (1..=64).contains(&members.len()),
        Invalid.error("grant_empty", "a grant needs 1 to 64 subjects")
    );
    ensure!(
        members.iter().collect::<BTreeSet<_>>().len() == members.len(),
        Invalid.error("invalid_grant", "duplicate subject")
    );
    let subject = &members[0];
    for who in members {
        if who != "*" {
            validate_actor(who)?;
        }
    }
    can.validate()?;
    let id = if let Some(id) = id {
        let old = grant(tx, id)?;
        if old.subjects == members && old.can == *can {
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
    tx.execute("DELETE FROM access_members WHERE grant_id=?", [id])?;
    for (i, s) in members.iter().enumerate() {
        tx.execute(
            "INSERT INTO access_members VALUES(?,?,?)",
            params![id, s, i],
        )?;
    }
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
        subjects: members.to_owned(),
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
        let tx = self.conn.unchecked_transaction()?;
        let a = self.access_on(&tx)?;
        a.any_manage()?;
        let g = grant(&tx, id)?;
        ensure!(
            a.list_grants().is_ok() || g.can.0.keys().all(|p| a.manage_at(p, false)),
            Missing.error("grant_not_found", "grant not found")
        );
        tx.commit()?;
        Ok(g)
    }
    pub fn credential(&self, id: &str) -> Result<Credential> {
        self.require_owner()?;
        credential(&self.conn, id)
    }
    pub fn grants(&self) -> Result<Vec<Grant>> {
        let tx = self.conn.unchecked_transaction()?;
        self.access_on(&tx)?.list_grants()?;
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
        self.set_group_grant(owner, id, &[subject.into()], can, false)
    }
    pub fn set_group_grant(
        &self,
        owner: &str,
        id: Option<i64>,
        subjects: &[String],
        can: &Rules,
        lockout: bool,
    ) -> Result<Grant> {
        validate_actor(owner)?;
        self.write(|tx| {
            let a = self.access_on(tx)?;
            a.actor(owner)?;
            a.any_manage()?;
            let old = id.map(|id| grant(tx, id)).transpose()?;
            let result = set_on(tx, owner, id, subjects, can)?;
            policy::authorize_change(
                self,
                tx,
                &a,
                &[can],
                &old.iter().map(|g| &g.can).collect::<Vec<_>>(),
                lockout,
            )?;
            Ok(result)
        })
    }
    pub fn patch_grant(
        &self,
        owner: &str,
        id: i64,
        add: &[String],
        remove: &[String],
        can: Option<&Rules>,
        lockout: bool,
    ) -> Result<Grant> {
        self.write(|tx| {
            let a = self.access_on(tx)?;
            a.actor(owner)?;
            a.any_manage()?;
            ensure!(
                !add.is_empty() || !remove.is_empty() || can.is_some(),
                Invalid.error("invalid_grant", "nothing to update")
            );
            validate_actor(owner)?;
            let old = grant(tx, id)?;
            let mut subjects = old.subjects.clone();
            for s in remove {
                let i = subjects
                    .iter()
                    .position(|old| old == s)
                    .context(Invalid.error("not_in_grant", "subject is not in this grant"))?;
                subjects.remove(i);
            }
            for s in add {
                if !subjects.contains(s) {
                    subjects.push(s.clone());
                }
            }
            let desired = can.unwrap_or(&old.can);
            let g = set_on(tx, owner, Some(id), &subjects, desired)?;
            let widened = if !add.is_empty() || can.is_some() {
                vec![desired]
            } else {
                vec![]
            };
            policy::authorize_change(self, tx, &a, &widened, &[&old.can], lockout)?;
            Ok(g)
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
        self.group_grant_batch(
            owner,
            &grants
                .iter()
                .map(|g| (vec![g.subject.clone()], g.can.clone()))
                .collect::<Vec<_>>(),
            key,
            false,
        )
    }
    pub fn group_grant_batch(
        &self,
        owner: &str,
        grants: &[(Vec<String>, Rules)],
        key: Option<&str>,
        lockout: bool,
    ) -> Result<(GrantBatch, bool)> {
        self.write(|tx| {
            let a = self.access_on(tx)?;
            a.actor(owner)?;
            a.any_manage()?;
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
                    policy::authorize_change(
                        self,
                        tx,
                        &a,
                        &grants.iter().map(|g| &g.1).collect::<Vec<_>>(),
                        &[],
                        lockout,
                    )?;
                    let record = BatchRecord::read(&data)?;
                    ensure!(
                        actor == owner
                            && record.grants.len() == grants.len()
                            && record
                                .grants
                                .iter()
                                .zip(grants)
                                .all(|(old, (subjects, can))| old.can == *can
                                    && old.subjects == *subjects),
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
                .map(|(subjects, can)| set_on(tx, owner, None, subjects, can))
                .collect::<Result<Vec<_>>>()?;
            policy::authorize_change(
                self,
                tx,
                &a,
                &grants.iter().map(|g| &g.can).collect::<Vec<_>>(),
                &[],
                lockout,
            )?;
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
        let a = self.access_on(&tx)?;
        a.any_manage()?;
        let (data, actor): (String, String) = tx
            .query_row(
                "SELECT payload,actor FROM events WHERE cursor=? AND kind='grant.batch_created'",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .context(Missing.error("grant_batch_not_found", "grant batch not found"))?;
        let record = BatchRecord::read(&data)?;
        if a.list_grants().is_err() {
            a.actor(&actor)?;
            for grant in &record.grants {
                ensure!(
                    grant.can.0.keys().all(|p| a.manage_at(p, false)),
                    Forbidden.error(
                        "missing_permission",
                        "receipt is outside the caller's current authority"
                    )
                );
            }
        }
        tx.commit()?;
        Ok(GrantBatch {
            id,
            grants: record.grants,
        })
    }
    pub fn revoke_grant(&self, owner: &str, id: i64) -> Result<()> {
        self.revoke_grant_with_outcome(owner, id, false).map(|_| ())
    }
    pub fn revoke_grant_with_outcome(&self, owner: &str, id: i64, lockout: bool) -> Result<bool> {
        validate_actor(owner)?;
        self.write(|tx| {
            let a = self.access_on(tx)?;
            a.actor(owner)?;
            a.any_manage()?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM access_grants WHERE id=?)",
                [id],
                |r| r.get(0),
            )?;
            if !exists {
                a.list_grants()?;
                return Ok(false);
            }
            let old = grant(tx, id)?;
            tx.execute("DELETE FROM access_grants WHERE id=?", [id])?;
            policy::authorize_change(self, tx, &a, &[], &[&old.can], lockout)?;
            Board::event_tx(
                tx,
                "grant.revoked",
                owner,
                None,
                serde_json::json!({"id":id}),
            )?;
            Ok(true)
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
        Ok(self.explain_decision(actor, channel)?.actions)
    }
    pub fn explain_decision(&self, actor: &str, channel: &str) -> Result<Decision> {
        validate_actor(actor)?;
        validate_channel(channel)?;
        let tx = self.conn.unchecked_transaction()?;
        let current = self.access_on(&tx)?;
        let discloses = current.manage_at("*", false);
        ensure!(
            discloses || current.bound_actor() == Some(actor),
            Forbidden.error(
                "missing_permission",
                "manage at * is required to explain another actor"
            )
        );
        let rules = policy::matching(&tx, actor)?;
        let policy = if current.is_trusted() || current.bound_actor() != Some(actor) {
            Access {
                id: Some(String::new()),
                actor: Some(actor.to_owned()),
                policy: policy::combined(&rules, false),
                own: policy::combined(&rules, true),
                caps: vec![],
                extra_cap: false,
            }
        } else {
            current.actor(actor)?;
            current
        };
        let actions = ALL
            .into_iter()
            .filter(|a| policy.allows(*a, channel))
            .collect();
        tx.commit()?;
        let mut decision = Decision {
            actions,
            level: None,
            pattern: None,
            over: vec![],
            grants: vec![],
        };
        if discloses && rules.iter().any(|g| g.level > 0) {
            let mut patterns: Vec<&String> = rules
                .iter()
                .flat_map(|g| g.rules.0.keys())
                .filter(|p| {
                    p.as_str() == "*"
                        || p.as_str() == channel
                        || p.strip_suffix('/')
                            .is_some_and(|base| channel == base || channel.starts_with(p.as_str()))
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            patterns.sort_by_key(|p| {
                std::cmp::Reverse(if p.as_str() == channel {
                    usize::MAX
                } else if p.as_str() == "*" {
                    0
                } else {
                    p.len()
                })
            });
            if let Some((first, rest)) = patterns.split_first() {
                decision.pattern = Some((*first).clone());
                decision.over = rest.iter().map(|p| (*p).clone()).collect();
                let applies = || rules.iter().filter(|g| g.rules.0.contains_key(*first));
                let level = applies().map(|g| g.level).max().unwrap_or(0);
                decision.level = Some(["everyone", "family", "principal"][level as usize].into());
                decision.grants = applies()
                    .filter(|g| g.level == level)
                    .map(|g| g.id)
                    .collect();
            }
        }
        Ok(decision)
    }
}
