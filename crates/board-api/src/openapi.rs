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
        "SourcePatch":obj(json!({"outbound":b,"mirror_roots":b,"mirror_files":b,"display_authors":b,"github_items":{"enum":["all","issues","pulls"]}}),&[]),
        "Rule":obj(json!({"priority":n,"script":{"type":"string","maxLength":8192}}),&["script"]),
        "Renderer":obj(json!({"script":{"type":"string","maxLength":8192}}),&["script"]),
        "Typing":obj(json!({"status":s,"seconds":n}),&[]),
        "WatcherPull":obj(json!({"limit":n,"lease_seconds":n,"wait_seconds":{"type":"integer","minimum":0,"maximum":60}}),&[]),
        "Definition":obj(json!({"description":s,"schema":{"type":"object"}}),&["schema"])
    })
}
pub(super) fn describe(op: &mut Value, path: &str, verb: &str) {
    let input = match (path, verb) {
        ("/messages", "post") => "NewMessage",
        ("/messages/{id}", "patch") => "Edit",
        ("/messages/{id}/labels", "patch") => "LabelPatch",
        ("/tasks", "post") => "NewTask",
        ("/schedules", "post") => "NewSchedule",
        ("/sources", "post") => "Source",
        ("/sources/{name}", "patch") => "SourcePatch",
        ("/sources/{name}/renderer", "put") => "Renderer",
        ("/rules/{name}", "put") => "Rule",
        ("/threads/{id}/typing", "put") => "Typing",
        ("/watcher-matches", "post") => "WatcherPull",
        ("/schemas/{name}/{version}", "put") => "Definition",
        _ => "",
    };
    if !input.is_empty() {
        op["requestBody"] =
            json!({"required":true,"content":{"application/json":{"schema":reference(input)}}});
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
