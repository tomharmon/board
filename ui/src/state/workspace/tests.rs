use super::State;
use board_local::{
    Board,
    automation::{self, TaskTarget, create_task},
};
use rune::{Env, Runtime};

fn workspace(db: Board) -> Runtime<State> {
    let state = State::test_board(db, None);
    let mut ui = Runtime::new(state, crate::view::view, (1200., 800.), Env::default());
    ui.frame(0.);
    ui
}

fn tap(ui: &mut Runtime<State>, key: &str) {
    let (x, y) = ui
        .find(key)
        .unwrap_or_else(|| panic!("missing control: {key}"));
    ui.pointer_down(x, y);
    ui.pointer_up(x, y);
    ui.frame(0.2);
}

fn fill(ui: &mut Runtime<State>, key: &str, value: &str) {
    tap(ui, key);
    ui.paste(value);
    ui.frame(0.2);
}

fn shows(ui: &Runtime<State>, text: &str) -> bool {
    ui.elements()
        .iter()
        .any(|e| e.text.as_deref().is_some_and(|s| s.contains(text)))
}

#[test]
fn task_routing_filters_query_all_pages() {
    let db = Board::memory().unwrap();
    let make = |name: &str, actor: &str, channel: &str, needs: &[i64]| {
        create_task(
            &db,
            actor,
            TaskTarget::New {
                channel,
                text: name,
                options: &Default::default(),
            },
            needs,
            Some(&format!("{actor}/builder")),
        )
        .unwrap()
        .id
    };
    let prereq = make("First job", "reader", "ops/work", &[]);
    for n in 0..43 {
        make(&format!("Other task {n}"), "other", "other", &[]);
    }
    let target = make("Filtered job", "reader", "ops/work", &[prereq]);
    let mut ui = workspace(db);
    tap(&mut ui, "tasks");
    assert!(ui.find("next-page").is_some());
    tap(&mut ui, "task-criteria");
    fill(&mut ui, "criteria-owner", "reader");
    fill(&mut ui, "criteria-assignee", "reader/builder");
    fill(&mut ui, "criteria-channel", "ops/");
    fill(&mut ui, "criteria-needs", &format!("#{prereq}"));
    tap(&mut ui, "apply-task-filters");
    assert!(shows(&ui, "Filtered job"));
    assert!(ui.find(&format!("task-row-{target}")).is_some());
    assert!(!shows(&ui, "First job"));
    assert!(ui.find("next-page").is_none());
}

#[test]
fn task_conversation_posts_a_reply_without_leaving_the_table() {
    let db = Board::memory().unwrap();
    let task = create_task(
        &db,
        "lin",
        TaskTarget::New {
            channel: "engineering",
            text: "Verify installers",
            options: &Default::default(),
        },
        &[],
        None,
    )
    .unwrap()
    .id;
    let mut ui = workspace(db);
    tap(&mut ui, "tasks");
    tap(&mut ui, &format!("task-row-{task}"));
    let body = "Package checks passed on a clean install.";
    fill(&mut ui, "message-body", body);
    tap(&mut ui, "send");
    let messages = ui.state.db().unwrap().thread(task).unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].re, Some(task));
    assert_eq!(messages[1].text, body);
    for m in &messages {
        assert!(ui.find(&format!("message-{}", m.id)).is_some());
    }
    for key in [
        "task-board",
        "task-banner",
        "reply-composer",
        "task-options",
    ] {
        assert!(ui.find(key).is_some(), "missing {key}");
    }
}

#[test]
fn all_tasks_includes_failed_and_completed_without_an_open_filter() {
    use automation::TaskAction;
    let db = Board::memory().unwrap();
    let mut ids = Vec::new();
    for (text, action) in [
        ("Verify installers", TaskAction::Fail("Needs correction")),
        ("Review checklist", TaskAction::Complete(Some("Done"))),
    ] {
        let id = create_task(
            &db,
            "worker",
            TaskTarget::New {
                channel: "engineering",
                text,
                options: &Default::default(),
            },
            &[],
            None,
        )
        .unwrap()
        .id;
        let claim = automation::claim(&db, "worker", id, 300).unwrap();
        automation::change_task(
            &db,
            id,
            "worker",
            Some(claim.epoch),
            claim.claim_token.as_deref(),
            action,
        )
        .unwrap();
        ids.push(id);
    }
    let mut ui = workspace(db);
    tap(&mut ui, "tasks");
    assert!(ui.find("segment0").is_none());
    assert!(shows(&ui, "all tasks"));
    for id in ids {
        assert!(ui.find(&format!("task-row-{id}")).is_some());
    }
    assert!(!shows(&ui, "oldest first") && !shows(&ui, "the latest across every channel"));
}

#[test]
fn empty_task_action_creates_a_persisted_task_and_filters_can_be_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.sqlite3");
    let mut ui = workspace(Board::open(&path).unwrap());
    tap(&mut ui, "tasks");
    tap(&mut ui, "empty-new-task");
    ui.paste("Verify backups");
    ui.frame(0.2);
    tap(&mut ui, "create-task");
    let tasks = automation::list_tasks(ui.state.db().unwrap(), None, Some("all")).unwrap();
    assert_eq!(tasks.len(), 1);
    let task = &tasks[0];
    assert_eq!(
        (&task.channel[..], &task.text[..]),
        ("general", "Verify backups")
    );
    let key = format!("task-row-{}", task.id);
    assert!(ui.find(&key).is_some());
    drop(ui);

    let mut ui = workspace(Board::open(path).unwrap());
    tap(&mut ui, "tasks");
    tap(&mut ui, "task-criteria");
    fill(&mut ui, "criteria-channel", "other");
    tap(&mut ui, "apply-task-filters");
    assert!(shows(&ui, "No tasks match these filters"));
    tap(&mut ui, "empty-clear-task-filters");
    assert!(ui.find(&key).is_some());
    assert!(ui.find("tasks-empty").is_none());
}

#[test]
fn administers_event_watchers_and_local_schemas() {
    let mut ui = workspace(Board::memory().unwrap());
    tap(&mut ui, "workspace");
    tap(&mut ui, "work-watchers");
    tap(&mut ui, "add-resource");
    fill(&mut ui, "resource-name", "observer");
    fill(&mut ui, "resource-channel", "dev/");
    fill(&mut ui, "resource-value", "message.posted");
    tap(&mut ui, "save-resource");
    assert!(ui.find("watcher-observer").is_some());
    assert!(shows(&ui, "observer"));
    assert!(ui.find("work-schedules").is_none());
    let db = ui.state.db().unwrap();
    let watcher = automation::get_watcher(db, "observer").unwrap();
    assert_eq!(watcher.filter.channel.as_deref(), Some("dev/"));
    assert_eq!(watcher.kind.as_deref(), Some("message.posted"));
    db.post("other", "author", "Unrelated", &Default::default())
        .unwrap();
    let posted = db
        .post("dev/build", "author", "Build ready", &Default::default())
        .unwrap();
    let delivery = automation::next_event(db, "observer").unwrap().unwrap();
    assert_eq!(delivery.message.unwrap().id, posted.id);
    automation::ack_event(db, "observer", delivery.cursor).unwrap();
    assert!(automation::next_event(db, "observer").unwrap().is_none());

    tap(&mut ui, "work-schemas");
    tap(&mut ui, "add-resource");
    fill(&mut ui, "resource-name", "build-result");
    tap(&mut ui, "save-resource");
    tap(&mut ui, "schema-build-result-1");
    assert!(shows(&ui, "build-result@1"));
    assert!(shows(&ui, r#""type": "object""#));
    assert!(
        ui.state
            .db()
            .unwrap()
            .schemas()
            .unwrap()
            .iter()
            .any(|s| s.name == "build-result")
    );
}

#[test]
fn source_forms_save_all_provider_types_and_require_publishing_consent() {
    use board_local::sources::{GithubItems, Provider};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sources.db");
    let db = Board::open(&path).unwrap();
    let mut ui = workspace(Board::open(&path).unwrap());
    tap(&mut ui, "workspace");
    tap(&mut ui, "work-sources");
    assert!(shows(&ui, "No sources configured"));
    let create = |ui: &mut Runtime<State>, provider: &str, name: &str, remote: &str| {
        tap(ui, "source-add");
        tap(ui, &format!("source-provider-{provider}"));
        fill(ui, "source-name", name);
        fill(ui, "source-remote", remote);
        fill(ui, "source-channel", "work/review");
        tap(ui, "source-create");
    };
    create(&mut ui, "github", "invalid", "not-a-repo");
    assert!(shows(&ui, "expected GitHub OWNER/REPO"));
    let form = &ui.state.desk.source.mapping;
    assert_eq!(
        (&*form.name.text, &*form.remote.text, &*form.channel.text),
        ("invalid", "not-a-repo", "work/review")
    );
    tap(&mut ui, "source-cancel");
    assert!(ui.find("source-name").is_none());
    assert!(db.sources("reader").unwrap().is_empty());
    for (provider, name, remote, kind) in [
        ("github", "bugs", "sample/repo", Provider::Github),
        ("slack", "chat", "C01234", Provider::Slack),
        ("linear", "tickets", "ENG", Provider::Linear),
        ("local", "stream", "events", Provider::Local),
    ] {
        create(&mut ui, provider, name, remote);
        assert!(ui.find("source-configure").is_some());
        assert!(ui.find("source-copy-command").is_some());
        assert!(shows(&ui, "Publishing off"));
        let command = if kind == Provider::Local {
            format!("board sync import {name} event.json")
        } else {
            format!("board sync pull {name}")
        };
        assert!(shows(&ui, &command));
        let cfg = db.source("reader", name).unwrap();
        assert_eq!(cfg.provider, kind);
        assert_eq!(cfg.remote, remote);
        assert_eq!(cfg.channel, "work/review");
        assert!(!cfg.outbound && !cfg.settings.mirror_roots && !cfg.settings.mirror_files);
        tap(&mut ui, "work-sources");
    }
    for key in [
        "source-chat",
        "source-configure",
        "source-publish",
        "source-mirror-roots",
        "source-mirror-files",
        "source-display-authors",
        "source-settings-save",
    ] {
        tap(&mut ui, key);
    }
    assert!(ui.find("source-settings-confirm").is_some());
    assert!(!db.source("reader", "chat").unwrap().outbound);
    tap(&mut ui, "source-settings-confirm");
    assert!(shows(&ui, "Publishing on"));
    let chat = db.source("reader", "chat").unwrap();
    assert!(
        chat.outbound
            && chat.settings.mirror_roots
            && chat.settings.mirror_files
            && chat.settings.display_authors
    );
    for key in [
        "work-sources",
        "source-bugs",
        "source-configure",
        "source-gh-pulls",
        "source-settings-save",
    ] {
        tap(&mut ui, key);
    }
    let gh = db.source("reader", "bugs").unwrap();
    assert_eq!(gh.settings.github_items, GithubItems::Pulls);
    assert!(!gh.outbound);
}
