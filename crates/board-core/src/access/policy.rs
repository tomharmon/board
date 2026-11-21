use super::*;

pub(super) struct Matched {
    pub id: i64,
    pub level: u8,
    pub rules: Rules,
}

pub(super) fn matching(c: &Connection, actor: &str) -> Result<Vec<Matched>> {
    let person = actor.split('/').next().unwrap_or(actor);
    let raw=c.prepare("SELECT g.id,max(CASE WHEN m.subject='*' THEN 0 WHEN m.subject=?1 THEN 2 ELSE 1 END),g.rules
        FROM access_members m JOIN access_grants g ON g.id=m.grant_id
        WHERE m.subject IN ('*',?1,?2) GROUP BY g.id ORDER BY g.id")?
        .query_map([actor,person],|r|Ok((r.get(0)?,r.get(1)?,r.get::<_,String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raw.into_iter()
        .map(|(id, level, raw)| {
            Ok(Matched {
                id,
                level,
                rules: serde_json::from_str(&raw)?,
            })
        })
        .collect()
}

fn merge<'a>(rows: impl Iterator<Item = &'a Matched>) -> Rules {
    let mut out = BTreeMap::<String, (u8, Actions)>::new();
    for g in rows {
        for (key, actions) in &g.rules.0 {
            let entry = out
                .entry(key.clone())
                .or_insert_with(|| (g.level, Actions::new()));
            if g.level > entry.0 {
                *entry = (g.level, actions.clone());
            } else if g.level == entry.0 {
                entry.1.extend(actions);
            }
        }
    }
    Rules(
        out.into_iter()
            .map(|(k, (_, actions))| (k, actions))
            .collect(),
    )
}

pub(super) fn combined(rows: &[Matched], own: bool) -> Option<Rules> {
    // Everyone can constrain an admitted actor, but is never a means of admission.
    rows.iter()
        .any(|g| g.level > 0)
        .then(|| merge(rows.iter().filter(|g| !own || g.level > 0)))
}

/// Evaluate both the previous and prospective board policies inside the writer
/// transaction. Each representative stands for a region on which access is constant.
pub(super) fn authorize_change(
    board: &Board,
    c: &Connection,
    before: &Access,
    changed: &[&Rules],
    removed: &[&Rules],
    lockout: bool,
) -> Result<()> {
    if before.is_trusted() {
        return Ok(());
    }
    let touched: BTreeSet<&String> = changed
        .iter()
        .chain(removed)
        .flat_map(|r| r.0.keys())
        .collect();
    let everyone = merge(matching(c, "*")?.iter());
    let spots: BTreeSet<&String> = touched
        .iter()
        .copied()
        .chain(everyone.0.keys())
        .chain(
            before
                .policy
                .iter()
                .chain(&before.own)
                .chain(&before.caps)
                .flat_map(|r| r.0.keys()),
        )
        .collect();
    let under = |spot: &str, p: &str| {
        spot != p
            && (p == "*"
                || (p.ends_with('/') && (spot == &p[..p.len() - 1] || spot.starts_with(p))))
    };
    for p in &touched {
        ensure!(
            before.manage_at(p, false) && before.manage_at(p, true),
            Forbidden.error("grant_reach", format!("manage required at {p}"))
        );
        for spot in &spots {
            ensure!(
                !under(spot, p) || before.manage_at(spot, true),
                Forbidden.error("grant_reach", format!("own policy lacks manage at {spot}"))
            );
        }
    }
    if !changed.is_empty() {
        for spot in spots {
            let ch = Access::pattern_sample(spot);
            for action in ALL {
                ensure!(
                    !everyone.permits(action, &ch) || before.allows(action, &ch),
                    Forbidden.error(
                        "grant_admission",
                        format!("admitting someone would exceed {action:?} at {spot}")
                    )
                );
            }
        }
    }
    if !lockout {
        let after = board.access_on(c).ok();
        ensure!(
            after.is_some_and(|a| touched
                .iter()
                .all(|p| a.manage_at(p, false) && a.manage_at(p, true))),
            crate::errors::Kind::Conflict.error(
                "grant_lockout",
                "this change would prevent its maker from undoing it"
            )
        );
    }
    Ok(())
}
