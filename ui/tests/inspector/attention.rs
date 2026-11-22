use super::*;
use board_local::{
    append_block,
    swarm::{SwarmAddWorker, SwarmStart},
};

fn seed(f: &Fixture) -> (i64, i64) {
    let b = Board::open(&f.db).unwrap();
    let run = b
        .swarm_start(
            "reader",
            &SwarmStart {
                goal: "Verify Unicode handling".into(),
                slug: Some("unicode".into()),
                ..Default::default()
            },
        )
        .unwrap();
    for name in ["impl", "verify"] {
        b.swarm_add_worker(
            "reader",
            "unicode",
            &SwarmAddWorker {
                name: name.into(),
                brief: "Check the submitted revision".into(),
                ..Default::default()
            },
        )
        .unwrap();
    }
    b.swarm_check_in("reader/impl", "unicode", "impl").unwrap();
    let revision = json!({"kind":"sha256","value":"a".repeat(64)});
    let post = |who, name, data| {
        b.swarm_post(
            who,
            "unicode",
            &append_block("", name, 1, &data).unwrap(),
            &Default::default(),
        )
        .unwrap()
        .message
        .id
    };
    let h = post(
        "reader/impl",
        "handoff",
        json!({"revision":revision,"summary":"Handle Unicode whitespace","changes":["src/fields.rs"],"to":"verify"}),
    );
    post(
        "reader/verify",
        "verification",
        json!({"handoff":h,"revision":revision,"summary":"Linux verified; macOS unavailable","checks":[{"name":"Unicode cases","status":"pass","command":"cargo test unicode","output":"5 passed"},{"name":"macOS","status":"not-run","command":"cargo test","output":"No Mac available"}]}),
    );
    let d = post(
        "reader/verify",
        "decision-request",
        json!({"to":"reader","summary":"Accept Linux evidence for this local build?","handoff":h}),
    );
    (run.id, d)
}

#[test]
fn operator_can_find_and_answer_a_decision_without_reading_the_feed() {
    let f = Fixture::new();
    let (_, decision) = seed(&f);
    for (size, prefix, mut steps) in [
        ((1200, 800), "desktop", vec![]),
        (
            (390, 844),
            "phone",
            vec![json!({"t":0.1,"type":"tap","key":"open-nav"})],
        ),
    ] {
        steps.push(json!({"t":0.3,"type":"tap","key":"needs-attention"}));
        let (tree, pixels) = f.snapshot(json!(steps), size, "dark", &format!("attention-{prefix}"));
        tree.expect_text("Your actions · 1");
        tree.expect_text("Agent follow-up · 1");
        tree.expect_key(&format!("attention-respond-{decision}"));
        let area = tree.bounds("attention-scroll");
        assert!(area.x >= 0. && area.edge() <= f64::from(size.0) + 1.);
        assert_eq!(pixels.dimensions(), size);
    }
    let tree = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"needs-attention"},
            {"t":0.3,"type":"tap","key":format!("attention-respond-{decision}")},
            {"t":0.5,"paste":"Accepted for this local Linux build; Mac remains unverified."},
            {"t":0.7,"type":"tap","key":"send-decision-answer"}
        ]),
        DESKTOP,
        "1.1",
    );
    tree.expect_text("Your actions · 0");
    assert!(
        Board::open(&f.db)
            .unwrap()
            .swarm_receipt("reader", "unicode")
            .unwrap()
            .decisions
            .is_empty()
    );
}

#[test]
fn stopped_run_shows_its_revision_bound_receipt_on_request_without_claiming_unrun_checks_passed() {
    let f = Fixture::new();
    let (run, _) = seed(&f);
    Board::open(&f.db)
        .unwrap()
        .swarm_stop("reader", "unicode")
        .unwrap();
    for (size, prefix, mut steps) in [
        ((1200, 900), "desktop", vec![]),
        (
            (390, 844),
            "phone",
            vec![json!({"t":0.1,"type":"tap","key":"open-nav"})],
        ),
    ] {
        steps.extend([
            json!({"t":0.3,"type":"tap","key":"swarms"}),
            json!({"t":0.5,"type":"tap","key":format!("swarm-run-{run}")}),
            json!({"t":0.7,"type":"tap","key":"swarm-receipt"}),
        ]);
        let (tree, _) = f.snapshot(json!(steps), size, "light", &format!("receipt-{prefix}"));
        for text in [
            "Run receipt",
            "Run: stopped",
            "not-run · macOS",
            "pass · Unicode cases",
            "No Mac available",
            "src/fields.rs",
        ] {
            tree.expect_text(text);
        }
        assert!(!tree.has_key("swarm-composer"));
        let area = tree.bounds("swarm-receipt-scroll");
        assert!(area.w > 100. && area.edge() <= f64::from(size.0) + 1.);
    }
}

#[test]
fn navigation_does_not_paint_the_previous_task_page_over_swarms() {
    let f = Fixture::new();
    let size = (1200, 800);
    let capture = |steps: Value, name: &str| {
        let path = f.dir.path().join(format!("{name}.png"));
        let output = f
            .command(steps, DESKTOP, "0.45")
            .arg("--snap")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!f.tree().has_key("task-board"));
        read_frame(&path, size)
    };
    let direct = capture(json!([{"t":0.3,"type":"tap","key":"swarms"}]), "direct");
    let switched = capture(
        json!([{"t":0.1,"type":"tap","key":"tasks"},{"t":0.3,"type":"tap","key":"swarms"}]),
        "switched",
    );
    for y in (240..770).step_by(9) {
        for x in (260..1180).step_by(9) {
            assert_eq!(
                direct[(x, y)],
                switched[(x, y)],
                "previous page left painted content at {x},{y}"
            );
        }
    }
}

#[test]
fn stopping_the_open_run_switches_directly_to_its_receipt() {
    let f = Fixture::new();
    let (run, _) = seed(&f);
    let tree = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"swarms"},
            {"t":0.3,"type":"tap","key":format!("swarm-run-{run}")},
            {"t":0.5,"type":"tap","key":"swarm-stop"},
            {"t":0.7,"type":"tap","key":"swarm-stop-confirm"}
        ]),
        DESKTOP,
        "1.1",
    );
    tree.expect_key("swarm-receipt-scroll");
    tree.expect_text("Run: stopped");
    assert!(!tree.has_key("swarm-composer"));
}

#[test]
fn a_decision_answer_survives_inspecting_the_source_and_returning() {
    let f = Fixture::new();
    let (_, decision) = seed(&f);
    let tree = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"needs-attention"},
            {"t":0.3,"type":"tap","key":format!("attention-respond-{decision}")},
            {"t":0.5,"paste":"Draft decision to keep while reviewing evidence"},
            {"t":0.7,"type":"tap","key":format!("attention-source-{decision}")},
            {"t":0.9,"type":"tap","key":"needs-attention"}
        ]),
        DESKTOP,
        "1.3",
    );
    let answer = tree
        .0
        .iter()
        .find(|e| e["key"] == "decision-answer")
        .unwrap();
    assert_eq!(
        answer["text"],
        "Draft decision to keep while reviewing evidence"
    );
    assert_eq!(
        Board::open(&f.db)
            .unwrap()
            .swarm_receipt("reader", "unicode")
            .unwrap()
            .decisions
            .len(),
        1
    );
}
