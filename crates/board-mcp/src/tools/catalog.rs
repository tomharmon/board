use anyhow::{Result, ensure};
use serde_json::{Map, Value, json};

#[derive(Clone, Copy)]
enum Kind {
    Text,
    Run,
    Target,
    Id,
    Cursor,
    Limit,
    Count,
    Seconds,
    Lease,
    Bool,
    Labels,
    Ids,
    Data,
}

impl Kind {
    fn schema(self) -> Value {
        use Kind::*;
        match self {
            Text => json!({"type":"string"}),
            Target | Run => json!({"oneOf":[{"type":"string"},{"type":"integer","minimum":1}]}),
            Id => json!({"type":"integer","minimum":1}),
            Count => json!({"type":"integer","minimum":0,"maximum":100000}),
            Cursor => json!({"type":"integer","minimum":0}),
            Limit => json!({"type":"integer","minimum":1,"maximum":500}),
            Seconds => json!({"type":"integer","minimum":0,"maximum":300,"default":90}),
            Lease => json!({"type":"integer","minimum":30,"maximum":3600,"default":300}),
            Bool => json!({"type":"boolean"}),
            Labels => json!({"type":"object","additionalProperties":{"type":"string"}}),
            Ids => json!({"type":"array","items":{"type":"integer","minimum":1}}),
            Data => json!({}),
        }
    }
    fn accepts(self, v: &Value) -> bool {
        use Kind::*;
        match self {
            Text => v.is_string(),
            Target | Run => v.is_string() || v.as_i64().is_some_and(|n| n > 0),
            Id => v.as_i64().is_some_and(|n| n > 0),
            Count => v.as_i64().is_some_and(|n| (0..=100000).contains(&n)),
            Cursor => v.as_i64().is_some_and(|n| n >= 0),
            Limit => v.as_i64().is_some_and(|n| (1..=500).contains(&n)),
            Seconds => v.as_i64().is_some_and(|n| (0..=300).contains(&n)),
            Lease => v.as_i64().is_some_and(|n| (30..=3600).contains(&n)),
            Bool => v.is_boolean(),
            Labels => v
                .as_object()
                .is_some_and(|v| v.values().all(Value::is_string)),
            Ids => v
                .as_array()
                .is_some_and(|v| v.iter().all(|n| n.as_i64().is_some_and(|i| i > 0))),
            Data => true,
        }
    }
}

pub(crate) struct Tool {
    pub name: &'static str,
    description: &'static str,
    fields: &'static [(&'static str, Kind, bool)],
}
impl Tool {
    fn definition(&self) -> Value {
        let properties: Map<_, _> = self
            .fields
            .iter()
            .map(|(k, t, _)| ((*k).into(), t.schema()))
            .collect();
        let required: Vec<_> = self
            .fields
            .iter()
            .filter_map(|(k, _, r)| r.then_some(k))
            .collect();
        json!({"name":self.name,"description":self.description,"inputSchema":{
            "type":"object","additionalProperties":false,"properties":properties,"required":required
        }})
    }
    pub fn validate(&self, v: &Value) -> Result<()> {
        let object = v
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("arguments must be an object"))?;
        for key in object.keys() {
            ensure!(
                self.fields.iter().any(|(k, _, _)| *k == key),
                "unknown argument: {key}"
            );
        }
        for &(key, kind, required) in self.fields {
            if let Some(value) = object.get(key) {
                ensure!(
                    kind.accepts(value),
                    "invalid value for {key}: expected {}",
                    kind.schema()
                );
            } else {
                ensure!(!required, "missing required argument: {key}");
            }
        }
        Ok(())
    }
}
macro_rules! tools {
    ($($name:literal, $doc:literal, [$($rk:ident:$rt:ident),*], [$($ok:ident:$ot:ident),*];)+) => {
        const TOOLS: &[Tool] = &[$(Tool {name:$name,description:$doc,fields:&[
            $((stringify!($rk),Kind::$rt,true),)* $((stringify!($ok),Kind::$ot,false),)*
        ]}),+];
    };
}
tools! {
    "swarm_start","Create a local coordination run and post its goal; no workers are launched.",[goal:Text],[slug:Text,repo_root:Text,max_notifications:Count];
    "swarm_list","List visible swarm runs, newest first.",[],[before:Id,limit:Limit];
    "swarm_add","Register a worker; returns its pinned instructions, skills directory and environment for your harness.",[name:Text,brief:Text],[run:Run,role:Text,workspace_path:Text,branch_name:Text];
    "swarm_status","Inspect a run and its roster. Omit run to use BOARD_SWARM_RUN.",[],[run:Run];
    "swarm_set_limit","Adjust a run's peer notification budget. Requires coordinator or owning person.",[max_notifications:Count],[run:Run];
    "swarm_post","Post publicly; @name notifies that teammate. A schema block needs both NAME@VERSION and schema_data.",[text:Text],[run:Run,labels:Labels,schema:Text,schema_data:Data];
    "swarm_dm","Send a side message to a worker name or coordinator.",[to:Text,text:Text],[run:Run];
    "swarm_read","Read public posts and visible DMs in event cursor order.",[],[run:Run,after:Cursor,limit:Limit];
    "swarm_wait","Wait for this actor's notification, timeout, or a stopped run. Use the returned cursor to resume.",[],[run:Run,after:Cursor,timeout_seconds:Seconds];
    "swarm_done","Mark this worker finished with an optional public summary.",[],[run:Run,summary:Text];
    "swarm_stop","Stop a run and wake its waiters. Requires coordinator or owning person.",[],[run:Run];
    "roles_list","List built-in and custom agent roles.",[],[];
    "roles_show","Inspect a role, its instructions and attached skills.",[slug:Text],[];
    "board_channels","List readable channels with an optional name prefix.",[],[prefix:Text,limit:Limit];
    "board_whoami","Report the identity bound to this connection and its worker assignment, if any. Never changes identity.",[],[];
    "board_post","Create a thread in a local channel.",[channel:Text,text:Text],[labels:Labels,no_sync:Bool];
    "board_reply","Reply to an individual message by its stable ID.",[id:Id,text:Text],[labels:Labels,no_sync:Bool];
    "board_read","Read a thread by message ID or search visible messages. Channel scopes ending with / include descendants.",[],[target:Target,channel:Text,text:Text,by:Text,mentions:Text,after:Cursor,limit:Limit];
    "tasks_list","List visible tasks by channel, lifecycle state, assignee or labels.",[],[channel:Text,state:Text,assignee:Text,labels:Labels,limit:Limit];
    "tasks_create","Create a task with optional prerequisites, assignment and atomic initial claim.",[channel:Text,text:Text],[assignee:Text,needs:Ids,labels:Labels,claim:Bool];
    "tasks_claim","Exclusively lease a task by ID or the next ready task in ready_channel; only one selector is allowed.",[],[id:Id,ready_channel:Text,labels:Labels,lease_seconds:Lease];
    "tasks_complete","Complete a task using the fenced lease held by this MCP session.",[id:Id],[result:Text];
    "tasks_fail","Fail a task with a reason using this session's live lease.",[id:Id,reason:Text],[];
    "tasks_block","Put a task on hold; a claimed task requires this session's lease.",[id:Id,reason:Text],[];
}
pub(crate) fn coordinator_tool(name: &str) -> bool {
    matches!(
        name,
        "swarm_start"
            | "swarm_list"
            | "swarm_add"
            | "swarm_set_limit"
            | "swarm_stop"
            | "roles_list"
            | "roles_show"
    )
}
pub(crate) fn catalog(worker: bool) -> Vec<Value> {
    TOOLS
        .iter()
        .filter(|t| !worker || !coordinator_tool(t.name))
        .map(Tool::definition)
        .collect()
}
pub(crate) fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.name == name)
}
