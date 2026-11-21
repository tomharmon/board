use anyhow::Result;
use board_local::{
    Board,
    access::{GrantBatch, GrantSpec, Rules},
};
use clap::Subcommand;
use serde_json::json;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum Grants {
    List,
    Create {
        subject: String,
        #[arg(long)]
        can: String,
    },
    /// Atomically create 1-16 grants from a JSON array. Use - for stdin.
    Batch {
        file: PathBuf,
        #[arg(long, visible_alias = "id")]
        idempotency_key: Option<String>,
    },
    /// Read the original immutable batch receipt.
    Receipt {
        id: i64,
    },
    Update {
        id: i64,
        subject: String,
        #[arg(long)]
        can: String,
    },
    Revoke {
        id: i64,
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
                grant.subject,
                serde_json::to_string(&grant.can)?
            );
        }
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
        } => {
            let grants: Vec<GrantSpec> = serde_json::from_str(&crate::load_text(&file)?)?;
            let (batch, made) = b.create_grant_batch(actor, &grants, idempotency_key.as_deref())?;
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
                        g.subject,
                        serde_json::to_string(&g.can)?
                    );
                }
            }
        }
        Grants::Explain { subject, channel } => {
            let actions = b.explain(&subject, &channel)?;
            if js {
                println!(
                    "{}",
                    json!({"subject":subject,"channel":channel,"actions":actions})
                );
            } else {
                println!(
                    "{} in {}: {}",
                    subject,
                    channel,
                    serde_json::to_string(&actions)?
                );
            }
        }
        Grants::Revoke { id } => {
            b.revoke_grant(actor, id)?;
            if js {
                println!("{}", json!({"revoked":id}));
            } else {
                println!("revoked grant {id}");
            }
        }
        cmd => {
            let (id, subject, can) = match cmd {
                Grants::Create { subject, can } => (None, subject, can),
                Grants::Update { id, subject, can } => (Some(id), subject, can),
                _ => unreachable!(),
            };
            let grant = b.set_grant(actor, id, &subject, &Rules::parse(&can)?)?;
            if js {
                println!("{}", json!({"grant":grant}));
            } else {
                println!(
                    "{}  {}  {}",
                    grant.id,
                    grant.subject,
                    serde_json::to_string(&grant.can)?
                );
            }
        }
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
