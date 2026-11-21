//! The common write contracts and projections. The route inventory is tested against the live router.
use serde_json::{Value, json};
fn obj(p: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":p,"required":required,"additionalProperties":false})
}
fn reference(n: &str) -> Value {
    json!({"$ref":format!("#/components/schemas/{n}")})
}
pub(super) fn models() -> Value {
    let s = json!({"type":"string"});
    let n = json!({"type":"integer","format":"int64"});
    let b = json!({"type":"boolean"});
    let labels = json!({"type":"object","additionalProperties":s});
    let arr = |v: Value| json!({"type":"array","items":v});
    let mut properties = json!({"channel":s,"reply_to":n,"text":{"type":"string","maxLength":10000},"no_sync":b,"labels":labels,
        "after":n,"attachments":{"type":"array","maxItems":10,"items":reference("InlineFile")},
        "attachment_ids":{"type":"array","maxItems":10,"items":{"oneOf":[n,s]}}});
    let mut post = obj(properties.clone(), &[]);
    post["oneOf"] = json!([{"required":["channel"]},{"required":["reply_to"]}]);
    post["properties"]["sync_targets"] = json!({"type":"array","maxItems":32,"items":{"type":"string"},"description":"Configured SOURCE or SOURCE:THREAD; only for a new shareable root."});
    let p = properties.as_object_mut().unwrap();
    p.remove("after");
    p.remove("reply_to");
    p.insert("attachment_ids".into(), arr(n.clone()));
    for (k, v) in [
        ("thread_id", n.clone()),
        ("assignee", s.clone()),
        ("needs", arr(n.clone())),
        ("claim", b.clone()),
        ("lease_seconds", n.clone()),
        ("token", s.clone()),
    ] {
        p.insert(k.into(), v);
    }
    let mut task = obj(properties.clone(), &[]);
    task["oneOf"] = json!([{"required":["channel"]},{"required":["thread_id"]}]);
    let mut schedule = post.clone();
    schedule["properties"]
        .as_object_mut()
        .unwrap()
        .remove("after");
    schedule["properties"]
        .as_object_mut()
        .unwrap()
        .remove("sync_targets");
    for k in ["due", "every", "times", "until"] {
        schedule["properties"][k] = n.clone();
    }
    schedule["properties"]["attachment_ids"] = arr(n.clone());
    schedule["required"] = json!(["due"]);
    json!({
        "Problem":{"type":"object","required":["status","code","detail"],"properties":{"status":n,"code":s,"detail":s}},
        "Message":{"type":"object","required":["id","channel","text","version","revision","deleted"],"properties":{"id":n,"re":{"type":["integer","null"]},"reply_to":{"type":["integer","null"]},"channel":s,"actor":s,"text":s,"revision":n,"version":n,"deleted":b,"no_sync":b,"labels":labels,"links":arr(reference("LinkSnapshot")),"file_notices":arr(reference("FileNotice")),"reactions":{"type":"object","additionalProperties":arr(s.clone())}}},
        "LinkSnapshot":{"type":"object","properties":{"url":s,"message":{"type":["integer","null"]},"text":{"type":["string","null"]},"author":{"type":["string","null"]},"reason":{"type":["string","null"]}}},
        "FileNotice":{"type":"object","properties":{"name":s,"size":{"type":["integer","null"]},"reason":s}},
        "InlineFile":obj(json!({"name":s,"data_base64":{"type":"string","contentEncoding":"base64"}}),&["name","data_base64"]),
        "NewMessage":post,"NewTask":task,"NewSchedule":schedule,
        "Edit":obj(json!({"text":s,"if_revision":n}),&["text"]),
        "LabelPatch":obj(json!({"set":labels,"unset":arr(s.clone()),"conditions":{"type":"object","additionalProperties":{"type":["string","null"]}}}),&[]),
        "Source":obj(json!({"name":s,"channel":s,"provider":{"enum":["github","slack","linear","local"]},"remote":s}),&["name","channel","provider","remote"]),
        "NewChannel":obj(json!({"name":s,"private":{"type":"boolean","default":false,"description":"Atomically restrict this new channel and its subtree to the local creator's family. Never changes an existing channel."}}),&["name"]),
        "SourcePatch":obj(json!({"outbound":b,"mirror_roots":b,"mirror_files":b,"display_authors":b,"github_items":{"enum":["all","issues","pulls"]}}),&[]),
        "JoinThread":obj(json!({"root":n}),&["root"]),
        "Rule":obj(json!({"priority":n,"script":{"type":"string","maxLength":8192}}),&["script"]),
        "Renderer":obj(json!({"script":{"type":"string","maxLength":8192}}),&["script"]),
        "Typing":obj(json!({"status":s,"seconds":n}),&[]),
        "WatcherPull":obj(json!({"limit":n,"lease_seconds":n,"wait_seconds":{"type":"integer","minimum":0,"maximum":60}}),&[]),
        "WatcherAck":obj(json!({"skip":b,"error":{"type":"string","minLength":1,"maxLength":1000},"if_pending":{"type":"boolean","default":false,"description":"Require a live lease. Its winning receipt reports no_match on a completed delivery instead of replaying success."}}),&[]),
        "Definition":obj(json!({"description":s,"schema":{"type":"object"}}),&["schema"])
    })
}
pub(super) fn describe(op: &mut Value, path: &str, verb: &str) {
    if verb == "get" {
        let filters: &[&str] = match path {
            "/threads" | "/messages" => &[
                "channel", "from", "by", "text", "mentions", "re", "label", "after", "before",
                "limit",
            ],
            "/threads/{id}" => &["label", "after", "limit"],
            "/events" => &[
                "channel", "from", "by", "text", "mentions", "re", "label", "after", "limit",
                "kind", "all", "last", "wait_ms",
            ],
            "/links" => &["url"],
            "/schedules" => &["channel", "by", "re", "all", "after", "limit"],
            _ => &[],
        };
        if !filters.is_empty() {
            let mut params: Vec<_> = filters.iter().map(|&name| {
                let schema = match name {
                    "label" => json!({"type":"array","items":{"type":"string"}}),
                    "all" => json!({"type":"boolean","default":false}),
                    "after" | "before" | "last" | "wait_ms" | "limit" | "re" => json!({"type":"integer","minimum": if matches!(name,"re"|"limit"|"before"){1}else{0}}),
                    _ => json!({"type":"string"}),
                };
                json!({"name":name,"in":"query","required":name=="url","schema":schema})
            }).collect();
            if path == "/events" {
                params.push(json!({"name":"Last-Event-ID","in":"header","schema":{"type":"string"},"description":"Resume after this delivered cursor, bypassing the original history seed."}));
            }
            op["parameters"] = json!(params);
        }
    }
    let input = match (path, verb) {
        ("/messages", "post") => "NewMessage",
        ("/channels", "post") => "NewChannel",
        ("/messages/{id}", "patch") => "Edit",
        ("/messages/{id}/labels", "patch") => "LabelPatch",
        ("/tasks", "post") => "NewTask",
        ("/schedules", "post") => "NewSchedule",
        ("/sources", "post") => "Source",
        ("/sources/{name}", "patch") => "SourcePatch",
        ("/sources/{name}/thread-links", "put") => "JoinThread",
        ("/sources/{name}/renderer", "put") => "Renderer",
        ("/rules/{name}", "put") => "Rule",
        ("/threads/{id}/typing", "put") => "Typing",
        ("/watcher-matches", "post") => "WatcherPull",
        ("/watchers/{name}/acknowledgements/{cursor}", "put") => "WatcherAck",
        ("/schemas/{name}/{version}", "put") => "Definition",
        _ => "",
    };
    if !input.is_empty() {
        op["requestBody"] = json!({"required":input!="WatcherAck","content":{"application/json":{"schema":reference(input)}}});
    }
    if input == "NewChannel" {
        op["responses"].as_object_mut().unwrap().remove("2XX");
        op["responses"]["200"] =
            json!({"description":"The requested public channel already exists; created is false."});
        op["responses"]["201"] = json!({"description":"New channel. Private creation also returns both committed grants.","headers":{"Location":{"schema":{"type":"string"},"description":"New channel resource"}}});
        for (code, description) in [
            (
                "403",
                "Permission denied. Private creation also requires listing board-wide grants.",
            ),
            (
                "409",
                "Private creation found an existing channel/subtree (channel_exists) or a grant reaching it (channel_granted). Nothing changed.",
            ),
        ] {
            op["responses"][code] = json!({"description":description,"content":{"application/json":{"schema":reference("Problem")}}});
        }
    }
    if path == "/sources/{name}/thread-links" {
        op["parameters"] = json!([{"name":"thread","in":"query","required":true,"schema":{"type":"string"},"description":"Existing provider conversation ID. Joining publishes no existing messages."}]);
    }
    if input == "WatcherAck" {
        op["parameters"] = json!([{"name":"If-Match","in":"header","schema":{"type":"string"},"description":"Quoted delivery ETag. Required for a leased delivery and for if_pending."}]);
        op["responses"].as_object_mut().unwrap().remove("2XX");
        op["responses"]["204"] = json!({"description":"Settled, or by default an exact replay."});
        for (code, desc) in [
            (
                "412",
                "Strict replay by the winning proof: no_match. An unknown, unreadable, wrong or stale delivery receipt: delivery_conflict.",
            ),
            ("428", "if_pending requires a quoted ETag in If-Match."),
        ] {
            op["responses"][code] = json!({"description":desc,"content":{"application/json":{"schema":reference("Problem")}}});
        }
    }
    let success = json!({"description":"Current message","content":{"application/json":{"schema":reference("Message")}}});
    if matches!(
        (path, verb),
        ("/messages/{id}", "get" | "patch")
            | ("/messages/{id}/labels", "patch")
            | ("/messages", "post")
    ) {
        op["responses"].as_object_mut().unwrap().remove("2XX");
        op["responses"]["200"] = success;
    }
    if (path, verb) == ("/messages", "post") {
        op["parameters"] =
            json!([{"name":"Idempotency-Key","in":"header","schema":{"type":"string"}}]);
        op["responses"]["201"] = op["responses"]["200"].clone();
        op["responses"]["201"]["headers"] =
            json!({"Location":{"schema":{"type":"string"},"description":"New message resource"}});
    }
}
