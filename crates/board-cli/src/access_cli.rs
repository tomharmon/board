use anyhow::Result;
use board_local::{
    Board,
    access::{Grant, GrantBatch, GrantInput, Rules},
};
use clap::Subcommand;
use serde_json::json;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum Grants {
    List,
    Create {
        #[arg(required = true)]
        subjects: Vec<String>,
        #[arg(long)]
        can: String,
        #[arg(long)]
        lock_me_out: bool,
    },
    /// Atomically create 1-16 grants from a JSON array. Use - for stdin.
    Batch {
        file: PathBuf,
        #[arg(long, visible_alias = "id")]
        idempotency_key: Option<String>,
        #[arg(long)]
        lock_me_out: bool,
    },
    /// Read the original immutable batch receipt.
    Receipt {
        id: i64,
    },
    Update {
        id: i64,
        #[arg(required = true)]
        subjects: Vec<String>,
        #[arg(long)]
        can: String,
        #[arg(long)]
        lock_me_out: bool,
    },
    /// Add or remove subjects without replacing the grant.
    Patch {
        id: i64,
        #[arg(long)]
        add: Vec<String>,
        #[arg(long)]
        remove: Vec<String>,
        #[arg(long)]
        can: Option<String>,
        #[arg(long)]
        lock_me_out: bool,
    },
    Revoke {
        id: i64,
        #[arg(long)]
        lock_me_out: bool,
    },
    Explain {
        subject: String,
        channel: String,
    },
}
fn print_batch(batch: GrantBatch, made: Option<bool>, js: bool) -> Result<()> {
    if js {
        let mut value = json!({"batch":batch});
        if let Some(made) = made {
            value["created"] = json!(made);
        }
        println!("{value}");
    } else {
        println!("grant batch #{}", batch.id);
        for grant in batch.grants {
            println!(
                "{}  {}  {}",
                grant.id,
                grant.subjects.join(","),
                serde_json::to_string(&grant.can)?
            );
        }
    }
    Ok(())
}
fn print_grant(g: Grant, js: bool) -> Result<()> {
    if js {
        println!("{}", json!({"grant":g}));
    } else {
        println!(
            "{}  {}  {}",
            g.id,
            g.subjects.join(","),
            serde_json::to_string(&g.can)?
        );
    }
    Ok(())
}
#[derive(Subcommand)]
pub enum Credentials {
    List,
    Issue {
        actor: String,
        #[arg(long)]
        scope: Option<String>,
        #[arg(long)]
        ttl: Option<u64>,
    },
    Revoke {
        id: String,
    },
}
pub fn grants(b: &Board, actor: &str, cmd: Grants, js: bool) -> Result<()> {
    match cmd {
        Grants::Batch {
            file,
            idempotency_key,
            lock_me_out,
        } => {
            let grants: Vec<GrantInput> = serde_json::from_str(&crate::load_text(&file)?)?;
            let groups = grants
                .into_iter()
                .map(GrantInput::parts)
                .collect::<Result<Vec<_>>>()?;
            let (batch, made) =
                b.group_grant_batch(actor, &groups, idempotency_key.as_deref(), lock_me_out)?;
            print_batch(batch, Some(made), js)?;
        }
        Grants::Receipt { id } => print_batch(b.grant_batch(id)?, None, js)?,
        Grants::List => {
            let grants = b.grants()?;
            if js {
                println!("{}", json!({"grants":grants}));
            } else {
                for g in grants {
                    println!(
                        "{}  {}  {}",
                        g.id,
                        g.subjects.join(","),
                        serde_json::to_string(&g.can)?
                    );
                }
            }
        }
        Grants::Explain { subject, channel } => {
            let decision = b.explain_decision(&subject, &channel)?;
            if js {
                println!("{}", json!(decision));
            } else {
                println!(
                    "{} in {}: {}",
                    subject,
                    channel,
                    serde_json::to_string(&decision.actions)?
                );
            }
        }
        Grants::Revoke { id, lock_me_out } => {
            let revoked = b.revoke_grant_with_outcome(actor, id, lock_me_out)?;
            if js {
                println!("{}", json!({"id":id,"revoked":revoked}));
            } else {
                println!(
                    "grant {id}: {}",
                    if revoked { "revoked" } else { "already absent" }
                );
            }
        }
        Grants::Create {
            subjects,
            can,
            lock_me_out,
        } => print_grant(
            b.set_group_grant(actor, None, &subjects, &Rules::parse(&can)?, lock_me_out)?,
            js,
        )?,
        Grants::Update {
            id,
            subjects,
            can,
            lock_me_out,
        } => print_grant(
            b.set_group_grant(
                actor,
                Some(id),
                &subjects,
                &Rules::parse(&can)?,
                lock_me_out,
            )?,
            js,
        )?,
        Grants::Patch {
            id,
            add,
            remove,
            can,
            lock_me_out,
        } => print_grant(
            b.patch_grant(
                actor,
                id,
                &add,
                &remove,
                can.as_deref().map(Rules::parse).transpose()?.as_ref(),
                lock_me_out,
            )?,
            js,
        )?,
    }
    Ok(())
}
pub fn credentials(b: &Board, actor: &str, cmd: Credentials, js: bool) -> Result<()> {
    match cmd {
        Credentials::List => {
            let rows = b.credentials()?;
            if js {
                println!("{}", json!({"credentials":rows}));
            } else {
                for c in rows {
                    println!(
                        "{}  {}  {}",
                        c.id,
                        c.actor,
                        if c.revoked { "revoked" } else { "issued" }
                    );
                }
            }
        }
        Credentials::Issue {
            actor: subject,
            scope,
            ttl,
        } => {
            let scope = scope.as_deref().map(Rules::parse).transpose()?;
            let issued = b.issue_credential(actor, &subject, scope.as_ref(), ttl)?;
            if js {
                println!("{}", serde_json::to_string(&issued)?);
            } else {
                println!("{}", issued.token);
            }
        }
        Credentials::Revoke { id } => {
            b.revoke_credential(actor, &id)?;
            if js {
                println!("{}", json!({"revoked":id}));
            } else {
                println!("revoked credential {id}");
            }
        }
    }
    Ok(())
}
