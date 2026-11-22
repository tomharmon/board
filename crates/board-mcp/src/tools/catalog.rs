use crate::Toolset;
use anyhow::Result;
use rmcp::{
    handler::server::tool::{schema_for_input, schema_for_output},
    model::{JsonObject, Tool as McpTool},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};

use super::read::{Feed, Read, Run};
type Text = String;
type Bool = bool;
type Data = Value;
type Ids = Vec<Id>;

macro_rules! bounded {
    ($name:ident, $ty:ty, $constraint:meta) => {
        #[derive(Deserialize, Serialize, JsonSchema)]
        #[schemars(inline)]
        pub(crate) struct $name(#[schemars($constraint)] pub(crate) $ty);
    };
}
bounded!(Id, i64, range(min = 1));
bounded!(Cursor, i64, range(min = 0));
bounded!(Count, i64, range(min = 0, max = 100000));
bounded!(Limit, usize, range(min = 1, max = 500));
bounded!(Page, usize, range(min = 1, max = 100));
bounded!(Seconds, u64, range(max = 300));
bounded!(Lease, i64, range(min = 30, max = 3600));
bounded!(
    FileChunk,
    usize,
    range(min = 1, max = board_local::FILE_CHUNK_BYTES)
);
const BASE64_MAX: usize = board_local::FILE_CHUNK_BYTES.div_ceil(3) * 4;
bounded!(Base64Chunk, String, length(max = BASE64_MAX));
bounded!(
    PlanItems,
    Vec<board_local::automation::PlanItem>,
    length(min = 1, max = board_local::automation::MAX_PLAN_ITEMS)
);

pub(crate) struct Tool {
    pub name: &'static str,
    description: &'static str,
    schema: fn() -> Arc<JsonObject>,
    decode: fn(&Value) -> Result<Call>,
}

pub(crate) struct Published {
    pub tool: &'static Tool,
    pub definition: McpTool,
    validator: OnceLock<jsonschema::Validator>,
}
impl Published {
    pub fn parse(&self, args: &Value) -> Result<Call> {
        let validator = self.validator.get_or_init(|| {
            jsonschema::validator_for(&json!(&*self.definition.input_schema))
                .expect("generated tool schema")
        });
        validator.validate(args).map_err(|e| {
            anyhow::anyhow!(
                "invalid tool arguments at {} (schema {})",
                e.instance_path,
                e.schema_path
            )
        })?;
        (self.tool.decode)(args)
    }
}
impl Tool {
    pub(crate) fn offered(&self, set: Toolset, worker: bool) -> bool {
        if set == Toolset::Full {
            return if worker {
                !coordinator_tool(self.name)
            } else {
                self.name != "worker_assignment"
            };
        }
        match self.name {
            "board_post" | "board_read" | "swarm_wait" | "swarm_observe" => true,
            "worker_assignment" => worker,
            "swarm_start" | "swarm_add" | "swarm_stop" | "tasks_plan" => !worker,
            _ => false,
        }
    }
}
fn schema<T: JsonSchema + 'static>() -> Arc<JsonObject> {
    schema_for_input::<T>().expect("tool parameters are objects")
}
macro_rules! tools {
    (@input $ty:ident, [$($rk:ident:$rt:ident),*], [$($ok:ident:$ot:ident),*]) => {
        #[derive(Deserialize, Serialize, JsonSchema)]
        #[serde(deny_unknown_fields)]
        pub(crate) struct $ty {
            $(pub $rk: $rt,)*
            $(#[serde(skip_serializing_if="Option::is_none")]
              #[schemars(extend("not" = {"type":"null"}))]
              pub $ok: Option<$ot>,)*
        }
    };
    (@input $ty:ident, $args:ident) => { pub(crate) type $ty = $args; };
    ($($ty:ident, $name:literal, $doc:literal, $args:tt $(, $opts:tt)?;)+) => {
        $(tools!(@input $ty, $args $(, $opts)?);)+
        pub(crate) enum Call { $($ty($ty)),+ }
        const TOOLS: &[Tool] = &[$(Tool {
            name:$name, description:$doc, schema:schema::<$ty>,
            decode:|v| Ok(Call::$ty($ty::deserialize(v)?)),
        }),+];
    };
}
tools! {
    SwarmStart, "swarm_start","Start a named run on an existing thread_id or create a goal thread in channel. Choose a stable slug before calling; retry the same arguments after a disconnect. Does not launch workers.",[goal:Text,slug:Text],[thread_id:Id,channel:Text,repo_root:Text,max_notifications:Count];
    SwarmObserve, "swarm_observe","Read the run overview, paging threads with next_task_after, exceptions with next_action_after, and changes with cursor. Or pass explain_task instead of cursors for that task's unfinished prerequisite chain and linked review evidence. limit bounds the returned steps.",[],[run:Run,after:Cursor,action_after:Cursor,task_after:Cursor,explain_task:Id,limit:Page];
    SwarmList, "swarm_list","List visible swarm runs, newest first.",[],[before:Id,limit:Limit];
    SwarmAdd, "swarm_add","Register a worker and brief. Optional thread_id links existing work without granting access, creating a task or claiming it. Returns the worker selector and, for a local owner, literal CLI arguments.",[name:Text,brief:Text],[run:Run,thread_id:Id,role:Text,workspace_path:Text,branch_name:Text];
    SwarmStatus, "swarm_status","Inspect a run and its roster. Omit run to use BOARD_SWARM_RUN.",[],[run:Run];
    SwarmCheckIn, "swarm_check_in","Report that this worker has started. Supplying runtime metadata replaces its previous report. session_ref is a public roster label, never a secret. Returns a short acknowledgment.",[],[run:Run,model:Text,workspace_mode:Text,session_ref:Text];
    WorkerAssignment, "worker_assignment","Read the goal, own brief, pinned instructions and available skill files. Supply both skill and path to read one pinned text file, not a local path.",[],[skill:Text,path:Text];
    SwarmLaunchFailed, "swarm_launch_failed","Report a launch failure actually observed by the harness. Coordinator only.",[name:Text,reason:Text],[run:Run];
    SwarmReceipt, "swarm_receipt","Read immutable handoffs, checks for their exact revisions, and outstanding decisions.",[],[run:Run];
    BoardAttention, "board_attention","List explicit decisions and blocked work; action_required distinguishes this actor's actions from agent follow-up.",[],[];
    SwarmDecide, "swarm_decide","Answer an explicit decision request addressed to this actor, even after stop.",[id:Id,summary:Text],[run:Run];
    SwarmSetLimit, "swarm_set_limit","Adjust a run's peer notification budget. Requires coordinator or owning person.",[max_notifications:Count],[run:Run];
    SwarmPost, "swarm_post","Post publicly; @name addresses a teammate. Returns notified and budget-suppressed recipients, not read receipts. A schema block needs both NAME@VERSION and schema_data.",[text:Text],[run:Run,reply_to:Id,schema:Text,schema_data:Data,idempotency_key:Text,attachment_ids:Ids];
    SwarmDm, "swarm_dm","Send a side message to a worker name or coordinator.",[to:Text,text:Text],[run:Run];
    SwarmRead, "swarm_read","Read visible conversation in cursor order without acknowledging it. Filter kind by public, dm, or addressed (to you); optionally filter by worker name.",[],[run:Run,after:Cursor,limit:Limit,kind:Feed,worker:Text];
    SwarmWait, "swarm_wait","Wait for your addressed notifications. Pass the last processed message batch's cursor as ack on your next call; unacknowledged batches can replay. Use after instead to read from a position without acknowledging.",[],[run:Run,after:Cursor,ack:Id,timeout_seconds:Seconds];
    SwarmDone, "swarm_done","Mark this worker finished with an optional public summary.",[],[run:Run,summary:Text];
    SwarmStop, "swarm_stop","Stop a run and wake its waiters. Requires coordinator or owning person.",[],[run:Run];
    RolesList, "roles_list","List built-in and custom agent roles.",[],[];
    RolesShow, "roles_show","Inspect a role, its instructions and attached skills.",[slug:Text],[];
    BoardChannels, "board_channels","List readable channels with an optional name prefix.",[],[prefix:Text,limit:Limit];
    BoardFileUpload, "board_file_upload","Stage up to 64 KiB of evidence as standard padded base64. Returns attachment metadata; publish its id in a post or reply's attachment_ids.",[channel:Text,name:Text,content_type:Text,data_base64:Base64Chunk],[];
    BoardFileRead, "board_file_read","Read a bounded slice of an authorized attachment by ID. Append decoded data_base64 bytes and continue with next_offset until null.",[id:Id],[offset:Cursor,length:FileChunk];
    BoardWhoami, "board_whoami","Report the identity bound to this connection and its worker assignment, if any. Never changes identity.",[],[];
    BoardPost, "board_post","Start a thread with channel or reply to a message with reply_to. For a reply on a thread linked by multiple swarms, specify run. In linked threads, notified and suppressed report signals, not reads. Use idempotency_key for retries.",[text:Text],[channel:Text,reply_to:Id,run:Run,no_sync:Bool,schema:Text,schema_data:Data,idempotency_key:Text,attachment_ids:Ids];
    BoardRead, "board_read","Open a thread by message ID (target), search ordinary messages, or read a run feed (run, optional kind and worker). The run feed defaults to public; kind can select dm or addressed instead. Reading never acknowledges the inbox. An opened task includes readable prerequisite results in message.task.inputs.",Read;
    TasksList, "tasks_list","List visible tasks oldest first. Pass next_after as after for the next page; cursor is the snapshot event head.",[],[channel:Text,state:Text,assignee:Text,after:Cursor,limit:Limit];
    TasksCreate, "tasks_create","Create a task with optional prerequisites, assignment and atomic initial claim.",[channel:Text,text:Text],[assignee:Text,needs:Ids,claim:Bool];
    TasksPlan, "tasks_plan","Create ordinary tasks in input order. Each item supplies text for a new thread in channel, or thread_id to track an existing conversation in place. Set run to include these threads in swarm_observe. Retry with the same key.",[channel:Text,tasks:PlanItems],[key:Text,run:Run];
    TasksClaim, "tasks_claim","Exclusively lease a specific task. The response includes readable prerequisite results and their revision evidence.",[id:Id],[lease_seconds:Lease];
    TasksRenew, "tasks_renew","Extend this session's live claim without changing its epoch.",[id:Id],[lease_seconds:Lease];
    TasksRelease, "tasks_release","Release this session's lease on the specified task.",[id:Id],[];
    TasksComplete, "tasks_complete","Complete a task using the fenced lease held by this MCP session.",[id:Id],[result:Text];
    TasksFail, "tasks_fail","Fail a task with a reason using this session's live lease.",[id:Id,reason:Text],[];
    TasksBlock, "tasks_block","Put a task on hold; a claimed task requires this session's lease.",[id:Id,reason:Text],[];
}
fn coordinator_tool(name: &str) -> bool {
    matches!(
        name,
        "swarm_start"
            | "tasks_plan"
            | "swarm_list"
            | "swarm_add"
            | "swarm_launch_failed"
            | "swarm_set_limit"
            | "swarm_stop"
            | "roles_list"
            | "roles_show"
    )
}
pub(crate) fn catalog(board: &board_local::Board, set: Toolset, worker: bool) -> Vec<Published> {
    TOOLS
        .iter()
        .filter(|t| t.offered(set, worker))
        .map(|tool| publish(tool, board))
        .collect()
}

fn publish(tool: &'static Tool, board: &board_local::Board) -> Published {
    let mut input = json!(&*(tool.schema)());
    if matches!(tool.name, "swarm_post" | "board_post") {
        if tool.name == "board_post" {
            input["oneOf"] = json!([{"required":["channel"]},{"required":["reply_to"]}]);
        }
        input["dependentRequired"] = json!({"schema":["schema_data"],"schema_data":["schema"]});
        if tool.name == "board_post" {
            input["dependentRequired"]["run"] = json!(["reply_to"]);
        }
        let variants: Vec<_> = [
            "handoff",
            "verification",
            "decision-request",
            "decision-resolution",
        ]
        .into_iter()
        .filter_map(|name| board.schema(name, 1).ok().filter(|s| s.built_in))
        .map(|s| {
            json!({
                "if":{"required":["schema"],"properties":{"schema":{"const":s.id}}},
                "then":{"properties":{"schema_data":s.schema}}
            })
        })
        .collect();
        if !variants.is_empty() {
            input["allOf"] = json!(variants);
        }
    } else if tool.name == "swarm_start" {
        input["oneOf"] = json!([{"required":["channel"]},{"required":["thread_id"]}]);
    } else if tool.name == "worker_assignment" {
        input["dependentRequired"] = json!({"skill":["path"],"path":["skill"]});
    } else if tool.name == "swarm_wait" {
        input["not"] = json!({"required":["ack","after"]});
    } else if tool.name == "swarm_observe" {
        input["not"] = json!({"required":["explain_task"],
                    "anyOf":[{"required":["after"]},{"required":["action_after"]},{"required":["task_after"]}]});
    }

    let mut definition = McpTool::new(
        tool.name,
        tool.description,
        input.as_object().unwrap().clone(),
    );
    definition.output_schema = match tool.name {
        "tasks_plan" => Some(schema_for_output::<super::ToolOutput<super::PlanReply>>()),
        "swarm_observe" => Some(schema_for_output::<super::ToolOutput<super::Observation>>()),
        _ => None,
    };
    Published {
        tool,
        definition,
        validator: OnceLock::new(),
    }
}
