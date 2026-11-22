use super::*;
use image::{Rgba, RgbaImage};

fn element<'a>(tree: &'a Tree, key: &str) -> &'a Value {
    tree.expect_key(key);
    tree.0.iter().find(|e| e["key"] == key).unwrap()
}

fn luminance(p: Rgba<u8>) -> f64 {
    let linear = |c: u8| {
        let x = f64::from(c) / 255.;
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(p[0]) + 0.7152 * linear(p[1]) + 0.0722 * linear(p[2])
}

fn contrast(a: Rgba<u8>, b: Rgba<u8>) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn visible_ink(pixels: &RgbaImage, area: Rect, background: Rgba<u8>, ratio: f64) -> usize {
    (area.y.ceil() as u32..area.bottom().ceil() as u32)
        .map(|y| {
            (area.x.ceil() as u32..area.edge().ceil() as u32)
                .filter(|&x| contrast(pixels[(x, y)], background) >= ratio)
                .count()
        })
        .sum()
}

#[test]
fn dark_and_light_composers_paint_readable_prompts_controls_and_dashes() {
    let f = Fixture::new();
    for theme in ["dark", "light"] {
        let (t, pixels) = f.snapshot(
            json!([{"t":0.15,"type":"tap","key":"tasks"}]),
            (1100, 680),
            theme,
            &format!("empty-tasks-{theme}"),
        );
        let shell = t.bounds("sidebar-header");
        assert_eq!(
            (shell.x, shell.y),
            (0., 0.),
            "no baked-in platform titlebar inset"
        );
        t.expect_text("No tasks yet");
        t.expect_key("empty-new-task");
        let area = t.bounds("task-composer");
        let bg = pixels[(area.x as u32 - 3, area.y as u32)];
        let dashed = Rect {
            x: area.x + 10.,
            y: area.y.round(),
            w: area.w - 20.,
            h: 1.,
        };
        let ink = visible_ink(&pixels, dashed, bg, 3.);
        assert!(
            ink > dashed.w as usize / 5 && ink < dashed.w as usize * 9 / 10,
            "the boundary must be visibly dashed in {theme}: {ink} pixels"
        );
        let body = t.bounds("quick-task-body");
        assert!(
            visible_ink(&pixels, body, bg, 4.5) > 25,
            "task prompt disappears in {theme}"
        );
        let field = t.bounds("quick-task-channel-field");
        let inside = pixels[(field.edge() as u32 - 7, field.y as u32 + 15)];
        assert!(
            visible_ink(&pixels, t.bounds("quick-task-channel"), inside, 4.5) > 12,
            "channel placeholder is illegible in {theme}"
        );
        let empty = t.bounds("empty-new-task");
        assert!(
            visible_ink(&pixels, empty, bg, 4.5) > 35,
            "empty action is illegible in {theme}"
        );
    }
}

fn glyph_rows(pixels: &RgbaImage, glyph: Rect, background: Rgba<u8>) -> Vec<u32> {
    (glyph.y.floor() as u32..glyph.bottom().ceil() as u32)
        .filter(|&y| {
            (glyph.x.round() as u32..glyph.edge().round() as u32)
                .any(|x| contrast(pixels[(x, y)], background) > 2.5)
        })
        .collect()
}

#[test]
fn hash_prefix_and_edited_destination_share_a_real_painted_baseline_in_each_form() {
    let f = Fixture::new();
    for (screen, field) in [
        (None, "destination"),
        (Some("tasks"), "quick-task-channel"),
        (Some("workspace"), "explain-channel"),
    ] {
        let mut steps = Vec::new();
        if let Some(key) = screen {
            steps.push(json!({"t":0.1,"type":"tap","key":key}));
        }
        steps.extend([
            json!({"t":0.3,"type":"tap","key":field}),
            json!({"t":0.45,"type":"key","cmd":true,"name":"a"}),
            json!({"t":0.6,"paste":"x#x"}),
        ]);
        let (tree, pixels) = f.snapshot(
            Value::Array(steps),
            (1100, 680),
            "dark",
            &format!("baseline-{field}"),
        );
        assert_eq!(element(&tree, field)["text"], "x#x");
        let prefix = tree.bounds(&format!("{field}-prefix"));
        let editor = tree.bounds(field);
        let group = tree.bounds(&format!("{field}-field"));
        close_to(group.y + group.h / 2., editor.y + editor.h / 2., 0.5, field);
        close_to(prefix.y, editor.y, 0.2, field);
        let background = pixels[(group.edge() as u32 - 8, (group.y + group.h / 2.) as u32)];
        let typed_hash = Rect {
            x: editor.x + prefix.w,
            y: editor.y,
            w: prefix.w,
            h: editor.h,
        };
        let left = glyph_rows(&pixels, prefix, background);
        let right = glyph_rows(&pixels, typed_hash, background);
        assert!(
            left.len() >= 6 && right.len() >= 6,
            "missing glyph in {field}: {left:?} {right:?}"
        );
        assert!(
            left[0].abs_diff(right[0]) <= 1
                && left.last().unwrap().abs_diff(*right.last().unwrap()) <= 1,
            "{field}: hash baseline differs in the painted frame, prefix {left:?}, typed {right:?}"
        );
    }
}

#[test]
fn access_subject_is_one_keyboard_switch_and_explain_is_a_separate_action() {
    let f = Fixture::new();
    let tree = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"workspace"},
            {"t":0.3,"type":"tap","key":"explain-channel"},
            {"t":0.5,"paste":"#ops"},
            {"t":0.7,"type":"tap","key":"explain-self"},
            {"t":0.9,"type":"key","name":"Right"},
            {"t":1.1,"type":"tap","key":"explain-actor"},
            {"t":1.3,"paste":"reader/build"},
            {"t":1.5,"type":"tap","key":"explain-access"}
        ]),
        DESKTOP,
        "1.8",
    );
    assert_eq!(element(&tree, "explain-identity")["role"], "radiogroup");
    assert_eq!(element(&tree, "explain-subject")["role"], "radio");
    assert_eq!(element(&tree, "explain-subject")["itemSelected"], true);
    assert_eq!(element(&tree, "explain-access")["role"], "button");
    assert!(
        !tree.has_key("explain-channel-prefix"),
        "do not duplicate a typed #"
    );
    assert!(
        element(&tree, "access-explanation")["text"]
            .as_str()
            .unwrap()
            .starts_with("@reader/build  #ops  ")
    );
    let group = tree.bounds("explain-identity");
    assert!(
        tree.bounds("explain-self").x >= group.x
            && tree.bounds("explain-subject").edge() <= group.edge()
    );
    assert!(tree.bounds("explain-access").x >= group.edge() + 8.);

    let narrow = f.render(
        json!([
            {"t":0.15,"type":"tap","key":"open-nav"},{"t":0.4,"type":"tap","key":"workspace"},
            {"t":0.65,"type":"tap","key":"explain-subject"}
        ]),
        "390x720",
        "0.95",
    );
    for key in [
        "explain-channel-field",
        "explain-identity",
        "explain-actor-field",
        "explain-access",
    ] {
        let r = narrow.bounds(key);
        assert!(
            r.x >= 0. && r.edge() <= 390. && r.bottom() <= 720.,
            "{key} outside compact viewport: {r:?}"
        );
    }
}

#[test]
fn optional_hash_on_a_composer_destination_survives_restart_and_posts_to_the_exact_channel() {
    let f = Fixture::new();
    let board = Board::open(&f.db).unwrap();
    let tree = f.render(json!([
        {"t":0.15,"type":"tap","key":"destination"},{"t":0.35,"type":"key","cmd":true,"name":"a"},
        {"t":0.55,"paste":"#ops/review"},{"t":0.75,"type":"key","name":"Enter"},
        {"t":0.95,"paste":"Check the restore"},{"t":1.15,"type":"tap","key":"tasks"}
    ]),DESKTOP,"1.6");
    tree.expect_key("task-board");
    let saved = board.load_composer("reader", "new").unwrap().unwrap();
    assert_eq!(saved.draft.channel, "ops/review");
    assert_eq!(saved.draft.body, "Check the restore");
    let tree = f.render(json!([{"t":0.2,"type":"tap","key":"send"}]), DESKTOP, "0.9");
    let messages = board.messages(&Filter::default(), 4).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].channel, "ops/review");
    assert_eq!(messages[0].text, "Check the restore");
    tree.expect_text("Check the restore");
    assert!(board.load_composer("reader", "new").unwrap().is_none());
}

#[test]
fn empty_task_action_creates_a_persisted_task_and_filters_can_be_cleared() {
    let f = Fixture::new();
    let tree = f.render(
        json!([
            {"t":0.1,"type":"tap","key":"tasks"},{"t":0.35,"type":"tap","key":"empty-new-task"},
            {"t":0.6,"paste":"Verify backups"},{"t":0.85,"type":"tap","key":"create-task"}
        ]),
        DESKTOP,
        "1.2",
    );
    let db = Board::open(&f.db).unwrap();
    let tasks = board_local::automation::list_tasks(&db, None, Some("all")).unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        (&tasks[0].channel[..], &tasks[0].text[..]),
        ("general", "Verify backups")
    );
    tree.expect_key(&format!("task-row-{}", tasks[0].id));
    let filter = json!([
        {"t":0.1,"type":"tap","key":"tasks"},{"t":0.3,"type":"tap","key":"task-criteria"},
        {"t":0.5,"type":"tap","key":"criteria-channel"},{"t":0.7,"paste":"other"},
        {"t":0.9,"type":"tap","key":"apply-task-filters"}
    ]);
    let filtered = f.render(filter.clone(), DESKTOP, "1.8");
    filtered.expect_key("empty-clear-task-filters");
    filtered.expect_text("No tasks match these filters");
    let mut clear = filter.as_array().unwrap().clone();
    clear.push(json!({"t":1.8,"type":"tap","key":"empty-clear-task-filters"}));
    let tree = f.render(Value::Array(clear), DESKTOP, "2.8");
    tree.expect_key(&format!("task-row-{}", tasks[0].id));
    assert!(!tree.has_key("tasks-empty"));
}
