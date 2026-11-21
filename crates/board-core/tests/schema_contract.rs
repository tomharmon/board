use anyhow::Result;
use board_local::{Board, errors::classify};
use serde_json::{Value, json};

fn field(sub: Value) -> Value {
    json!({"type":"object","properties":{"x":sub}})
}

#[test]
fn registry_accepts_only_bounded_structural_keywords_with_meaningful_values() -> Result<()> {
    let b = Board::memory()?;
    for (i, sub) in [
        json!({"enum":[]}),
        json!({"enum":"a"}),
        json!({"oneOf":[]}),
        json!({"allOf":[]}),
        json!({"anyOf":[]}),
        json!({"anyOf":[false,1]}),
        json!({"properties":{"x":null}}),
        json!({"items":[{}]}),
        json!({"additionalProperties":[]}),
        json!({"not":null}),
        json!({"type":[]}),
        json!({"type":["number","number"]}),
        json!({"type":["text"]}),
        json!({"required":["x","x"]}),
        json!({"required":["x",1]}),
        json!({"minItems":-1}),
        json!({"maxLength":1.5}),
        json!({"minLength":"3"}),
        json!({"maximum":"1"}),
        json!({"exclusiveMaximum":false}),
        json!({"multipleOf":0}),
        json!({"multipleOf":-1.5}),
        json!({"title":null}),
        json!({"description":1}),
        json!({"$schema":true}),
        json!({"format":"phone"}),
        json!({"pattern":".*"}),
        json!({"uniqueItems":true}),
        json!({"$ref":"file:///tmp/not-a-schema"}),
        json!({"$defs":{}}),
        json!({"const":{"a\0b":1}}),
    ]
    .into_iter()
    .enumerate()
    {
        let error = b
            .register_schema_as("rejected", 1, "", &field(sub.clone()), "worker")
            .unwrap_err();
        assert_eq!(classify(&error).1, "invalid_schema", "case {i}: {sub}");
    }
    assert_eq!(b.cursor()?, 0);
    for (i, sub) in [
        json!({"enum":[null,{},[1],1,1]}),
        json!({"const":{"$ref":"https://example.invalid/offline"}}),
        json!({"type":["string","integer","null"]}),
        json!({"required":[],"properties":{"any":true,"never":false}}),
        json!({"minimum":-3.5,"exclusiveMinimum":-4,"multipleOf":0.001}),
        json!({"minItems":1.0,"maxItems":1e25}),
        json!({"oneOf":[true,false],"anyOf":[true],"allOf":[{},true],"not":false}),
        json!({"$schema":"https://example.invalid/not-fetched","format":"ipv6"}),
    ]
    .into_iter()
    .enumerate()
    {
        assert!(b.register_schema_as("allowed", i as u32 + 1, "", &field(sub), "worker")?);
    }
    Ok(())
}

#[test]
fn schema_structure_budgets_ignore_literal_keywords_but_count_every_json_value() -> Result<()> {
    let b = Board::memory()?;
    // Root and every object subschema count. Booleans only consume total JSON nodes.
    let defs = |n, entry: Value| json!({"type":"object","allOf":vec![entry;n]});
    assert!(b.register_schema("nodes", 1, "", &defs(63, json!({})))?);
    assert!(
        b.register_schema("nodes", 2, "", &defs(64, json!({})))
            .is_err()
    );
    assert!(b.register_schema("nodes", 3, "", &defs(509, json!(true)))?);
    assert!(
        b.register_schema("nodes", 4, "", &defs(510, json!(true)))
            .is_err()
    );
    assert!(b.register_schema(
        "nodes",
        5,
        "",
        &json!({"type":"object","const":{"$ref":"literal","enum":[]}})
    )?);
    assert!(
        b.register_schema(
            "nodes",
            6,
            "",
            &json!({"type":"object","const":vec![0;513]})
        )
        .is_err()
    );
    Ok(())
}
