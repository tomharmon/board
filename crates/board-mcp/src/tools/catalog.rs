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
    WaitSeconds,
    Lease,
    Bool,
    Labels,
    Ids,
    Data,
    PlanItems,
    Slots,
    Retries,
    Page,
    FileChunk,
    Base64Chunk,
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
            Seconds | WaitSeconds => {
                json!({"type":"integer","minimum":0,"maximum":300,"default":if matches!(self, Seconds) {90} else {0}})
            }
            Lease => json!({"type":"integer","minimum":30,"maximum":3600,"default":300}),
            Bool => json!({"type":"boolean"}),
            Labels => json!({"type":"object","additionalProperties":{"type":"string"}}),
            Ids => json!({"type":"array","items":{"type":"integer","minimum":1}}),
            Data => json!({}),
            Slots => json!({"type":"integer","minimum":1,"maximum":128,"default":8}),
            Retries => json!({"type":"integer","minimum":0,"maximum":1000,"default":0}),
            Page => json!({"type":"integer","minimum":1,"maximum":100,"default":20}),
            FileChunk => {
                json!({"type":"integer","minimum":1,"maximum":board_local::FILE_CHUNK_BYTES,"default":board_local::FILE_CHUNK_BYTES})
            }
            Base64Chunk => {
                json!({"type":"string","contentEncoding":"base64","maxLength":board_local::FILE_CHUNK_BYTES.div_ceil(3)*4})
            }
            PlanItems => json!({"type":"array","minItems":1,"items":{
                "type":"object","additionalProperties":false,"required":["text"],"properties":{
                    "text":Text.schema(),"key":{"type":["string","null"]},
                    "for":{"type":["string","null"]},"labels":Labels.schema(),
                    "attachment_ids":Ids.schema(),"no_sync":Bool.schema(),"noSync":Bool.schema(),
                    "needs":{"type":"array","items":{"oneOf":[Text.schema(),Id.schema(),
                        {"type":"object","additionalProperties":false,"required":["key"],"properties":{"key":Text.schema()}},
                        {"type":"object","additionalProperties":false,"required":["id"],"properties":{"id":{"oneOf":[Id.schema(),Text.schema()]}}}
                    ]}}
                }
            }}),
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
            Seconds | WaitSeconds => v.as_i64().is_some_and(|n| (0..=300).contains(&n)),
            Lease => v.as_i64().is_some_and(|n| (30..=3600).contains(&n)),
            Bool => v.is_boolean(),
            Labels => v
                .as_object()
                .is_some_and(|v| v.values().all(Value::is_string)),
            Ids => v
                .as_array()
                .is_some_and(|v| v.iter().all(|n| n.as_i64().is_some_and(|i| i > 0))),
            Data => true,
            Slots => v.as_i64().is_some_and(|n| (1..=128).contains(&n)),
            Retries => v.as_i64().is_some_and(|n| (0..=1000).contains(&n)),
            Page => v.as_i64().is_some_and(|n| (1..=100).contains(&n)),
            FileChunk => v
                .as_u64()
                .is_some_and(|n| (1..=board_local::FILE_CHUNK_BYTES as u64).contains(&n)),
            Base64Chunk => v
                .as_str()
                .is_some_and(|s| s.len() <= board_local::FILE_CHUNK_BYTES.div_ceil(3) * 4),
            PlanItems => v.as_array().is_some_and(|a| !a.is_empty()),
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
    "swarm_start","Record a goal, worker capacity and retry budget; no workers are launched.",[goal:Text],[slug:Text,repo_root:Text,max_notifications:Count,max_active:Slots,retry_budget:Retries];
    "swarm_observe","Read the goal, ordinary task threads, current exceptions and recent changes. Page threads with next_task_after, exceptions with next_action_after, and changes with cursor.",[],[run:Run,after:Cursor,action_after:Cursor,task_after:Cursor,limit:Page];
    "swarm_list","List visible swarm runs, newest first.",[],[before:Id,limit:Limit];
    "swarm_add","Register an assignment without starting a process. Returns its worker selector; only export_local:true writes skills and returns a local CLI launch package.",[name:Text,brief:Text],[run:Run,role:Text,workspace_path:Text,branch_name:Text,export_local:Bool];
    "swarm_status","Inspect a run and its roster. Omit run to use BOARD_SWARM_RUN.",[],[run:Run];
    "swarm_check_in","Report that this worker has started. Supplying runtime metadata replaces its previous report. session_ref is a public roster label, never a secret. Set compact:true for a short acknowledgment.",[],[run:Run,compact:Bool,model:Text,workspace_mode:Text,session_ref:Text];
    "worker_assignment","Read the goal, own brief, pinned instructions and available skill files. Supply both skill and path to read one pinned text file, not a local path.",[],[skill:Text,path:Text];
    "swarm_launch_failed","Report a launch failure actually observed by the harness. Coordinator only.",[name:Text,reason:Text],[run:Run];
    "swarm_receipt","Read immutable handoffs, checks for their exact revisions, and outstanding decisions.",[],[run:Run];
    "board_attention","List explicit decisions and blocked work; action_required distinguishes this actor's actions from agent follow-up.",[],[];
    "swarm_decide","Answer an explicit decision request addressed to this actor, even after stop.",[id:Id,summary:Text],[run:Run];
    "swarm_set_limit","Adjust a run's peer notification budget. Requires coordinator or owning person.",[max_notifications:Count],[run:Run];
    "swarm_post","Post publicly; @name notifies that teammate. A schema block needs both NAME@VERSION and schema_data.",[text:Text],[run:Run,labels:Labels,schema:Text,schema_data:Data,idempotency_key:Text,attachment_ids:Ids];
    "swarm_dm","Send a side message to a worker name or coordinator.",[to:Text,text:Text],[run:Run];
    "swarm_read","Read visible conversation in cursor order without acknowledging it. Filter kind by public, dm, or addressed (to you); optionally filter by worker name.",[],[run:Run,after:Cursor,limit:Limit,kind:Text,worker:Text];
    "swarm_wait","Wait for your addressed notifications. Pass the last processed message batch's cursor as ack on your next call; unacknowledged batches can replay. Use after instead to read from a position without acknowledging.",[],[run:Run,after:Cursor,ack:Id,timeout_seconds:Seconds];
    "swarm_done","Mark this worker finished with an optional public summary.",[],[run:Run,summary:Text];
    "swarm_stop","Stop a run and wake its waiters. Requires coordinator or owning person.",[],[run:Run];
    "roles_list","List built-in and custom agent roles.",[],[];
    "roles_show","Inspect a role, its instructions and attached skills.",[slug:Text],[];
    "board_channels","List readable channels with an optional name prefix.",[],[prefix:Text,limit:Limit];
    "board_file_upload","Stage up to 64 KiB of evidence as standard padded base64. Returns attachment metadata; publish its id in a post or reply's attachment_ids.",[channel:Text,name:Text,content_type:Text,data_base64:Base64Chunk],[];
    "board_file_read","Read a bounded slice of an authorized attachment by ID. Append decoded data_base64 bytes and continue with next_offset until null.",[id:Id],[offset:Cursor,length:FileChunk];
    "board_whoami","Report the identity bound to this connection and its worker assignment, if any. Never changes identity.",[],[];
    "board_post","Create a thread. In a live swarm channel, @name notifies that teammate. Use idempotency_key for retries.",[channel:Text,text:Text],[labels:Labels,no_sync:Bool,schema:Text,schema_data:Data,idempotency_key:Text,attachment_ids:Ids];
    "board_reply","Reply by message ID. In a live swarm channel, @name notifies that teammate. Use idempotency_key for retries.",[id:Id,text:Text],[labels:Labels,no_sync:Bool,schema:Text,schema_data:Data,idempotency_key:Text,attachment_ids:Ids];
    "board_read","Read a thread by message ID or search visible messages. Channel scopes ending with / include descendants.",[],[target:Target,channel:Text,text:Text,by:Text,mentions:Text,after:Cursor,limit:Limit];
    "tasks_list","List visible tasks oldest first. Pass next_after as after for the next page; cursor is the snapshot event head.",[],[channel:Text,state:Text,assignee:Text,labels:Labels,after:Cursor,limit:Limit];
    "tasks_create","Create a task with optional prerequisites, assignment and atomic initial claim.",[channel:Text,text:Text],[assignee:Text,needs:Ids,labels:Labels,claim:Bool];
    "tasks_plan","Create a batch of ordinary tasks and return them in input order. Use a stable key to retry; large batches checkpoint.",[channel:Text,tasks:PlanItems],[key:Text];
    "tasks_claim","Exclusively lease by id or ready_channel. Only a ready queue can wait; nothing_ready means the wait timed out.",[],[id:Id,ready_channel:Text,labels:Labels,lease_seconds:Lease,wait_seconds:WaitSeconds];
    "tasks_renew","Extend this session's live claim without changing its epoch.",[id:Id],[lease_seconds:Lease];
    "tasks_release","Return this session's claimed task to the eligible pool.",[id:Id],[];
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
            | "swarm_launch_failed"
            | "swarm_set_limit"
            | "swarm_stop"
            | "roles_list"
            | "roles_show"
    )
}
pub(crate) fn catalog(board: &board_local::Board, worker: bool, structured: bool) -> Vec<Value> {
    TOOLS
        .iter()
        .filter(|t| if worker { !coordinator_tool(t.name) } else { t.name != "worker_assignment" })
        .map(|tool| {
            let mut d = tool.definition();
            if matches!(tool.name, "swarm_post" | "board_post" | "board_reply") {
                let input = &mut d["inputSchema"];
                input["dependentRequired"] = json!({"schema":["schema_data"],"schema_data":["schema"]});
                let variants: Vec<_> = ["handoff","verification","decision-request","decision-resolution"]
                    .into_iter()
                    .filter_map(|name| board.schema(name, 1).ok().filter(|s| s.built_in))
                    .map(|s| json!({
                        "if":{"required":["schema"],"properties":{"schema":{"const":s.id}}},
                        "then":{"properties":{"schema_data":s.schema}}
                    })).collect();
                if !variants.is_empty() {
                    input["allOf"] = json!(variants);
                }
            } else if tool.name == "worker_assignment" {
                d["inputSchema"]["dependentRequired"] = json!({"skill":["path"],"path":["skill"]});
            } else if tool.name == "swarm_wait" {
                d["inputSchema"]["not"] = json!({"required":["ack","after"]});
            } else if tool.name == "tasks_claim" {
                d["inputSchema"]["oneOf"] = json!([{"required":["id"]},{"required":["ready_channel"]}]);
                d["inputSchema"]["dependentRequired"] = json!({"wait_seconds":["ready_channel"]});
            }
            if structured && let Some(success) = output_schema(tool.name) {
                d["outputSchema"] = json!({"type":"object","oneOf":[success,{
                    "required":["code","message"],"properties":{"code":Kind::Text.schema(),"message":Kind::Text.schema()}
                }]});
            }
            d
        })
        .collect()
}
fn output_schema(name: &str) -> Option<Value> {
    Some(match name {
        "tasks_plan" => json!({"required":["key","tasks"],"properties":{
            "key":Kind::Text.schema(),"tasks":{"type":"array","items":{"type":"object","required":["id"],"properties":{"id":Kind::Id.schema()}}}
        }}),
        "swarm_observe" => {
            json!({"required":["run","goal","channel","status","policy","workers","tasks","threads","next_task_after","available_capacity","actions","changes","cursor","next_action_after","has_more"],
                "properties":{"run":Kind::Text.schema(),"status":Kind::Text.schema(),"policy":{"type":"object"},
                "goal":Kind::Text.schema(),"channel":Kind::Text.schema(),"workers":{"type":"object"},"tasks":{"type":"object"},"available_capacity":Kind::Cursor.schema(),
                "threads":{"type":"array","items":{"type":"object","required":["id","title","state"]}},"next_task_after":{"type":["integer","null"]},
                "actions":{"type":"array"},"changes":{"type":"array"},"cursor":Kind::Cursor.schema(),
                "next_action_after":{"type":["integer","null"]},"has_more":Kind::Bool.schema()}
            })
        }
        _ => return None,
    })
}
pub(crate) fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.name == name)
}
