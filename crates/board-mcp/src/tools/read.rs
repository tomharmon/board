use super::{ActorContext, value};
use anyhow::Result;
use board_local::{Board, Filter, swarm::SwarmFilter};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize, Serialize, JsonSchema)]
#[schemars(inline)]
#[serde(untagged)]
pub(crate) enum Run {
    Slug(String),
    Id(#[schemars(range(min = 1))] i64),
}

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Feed {
    Public,
    Dm,
    Addressed,
}

impl Run {
    pub(super) fn reference(self) -> String {
        match self {
            Self::Slug(s) => s,
            Self::Id(n) => format!("#{n}"),
        }
    }
}
impl Feed {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Dm => "dm",
            Self::Addressed => "addressed",
        }
    }
}

/// An untagged union keeps the three existing call forms without a mode flag.
#[derive(Deserialize, JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
#[schemars(extend("type" = "object"))]
pub(crate) enum Read {
    Thread {
        #[schemars(range(min = 1))]
        target: i64,
        text: Option<String>,
        by: Option<String>,
        mentions: Option<String>,
        #[schemars(range(min = 1))]
        after: Option<i64>,
        #[schemars(range(min = 1, max = 500))]
        limit: Option<usize>,
    },
    Run {
        run: Run,
        kind: Option<Feed>,
        worker: Option<String>,
        #[schemars(range(min = 0))]
        after: Option<i64>,
        #[schemars(range(min = 1, max = 500))]
        limit: Option<usize>,
    },
    Search {
        channel: Option<String>,
        text: Option<String>,
        by: Option<String>,
        mentions: Option<String>,
        #[schemars(range(min = 0))]
        after: Option<i64>,
        #[schemars(range(min = 1, max = 500))]
        limit: Option<usize>,
    },
}

impl Read {
    pub(super) fn execute(self, board: &Board, ctx: &ActorContext) -> Result<Value> {
        let (target, channel, text, by, mentions, after, limit) = match self {
            Self::Run {
                run,
                kind,
                worker,
                after,
                limit,
            } => {
                let run = super::selected_run(ctx, Some(run))?;
                let kind = kind.unwrap_or(Feed::Public);
                return value(board.swarm_read_filtered(
                    &ctx.actor,
                    &run,
                    after.unwrap_or(0),
                    limit.unwrap_or(50),
                    true,
                    &SwarmFilter {
                        kind: Some(kind.as_str().into()),
                        worker,
                    },
                )?);
            }
            Self::Thread {
                target,
                text,
                by,
                mentions,
                after,
                limit,
            } => (Some(target), None, text, by, mentions, after, limit),
            Self::Search {
                channel,
                text,
                by,
                mentions,
                after,
                limit,
            } => (None, channel, text, by, mentions, after, limit),
        };
        let mut filter = Filter {
            channel,
            text,
            by,
            mentions,
            after,
            ..Default::default()
        };
        filter.bind_actor(&ctx.actor);
        match target {
            Some(id) => value(board.conversation_window(id, &filter, Some(limit.unwrap_or(50)))?),
            None => value(board.messages_window(&filter, None, limit.unwrap_or(50))?),
        }
    }
}
