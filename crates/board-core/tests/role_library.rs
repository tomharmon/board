use board_local::Board;
use std::fs;
use tempfile::tempdir;

#[test]
fn role_edits_skill_revisions_and_multi_role_confirmation_survive_restart() {
    let t = tempdir().unwrap();
    let db = t.path().join("board.sqlite3");
    let b = Board::open(&db).unwrap();
    let defaults = b.roles_list().unwrap();
    assert_eq!(defaults.len(), 6);
    for r in defaults {
        assert!(r.is_builtin && !r.system_prompt.is_empty() && r.skills.len() == 1);
        assert_eq!(r.default_system_prompt.unwrap(), r.system_prompt);
    }
    let old = b.role_show("reviewer").unwrap();
    let updated = b
        .role_edit(
            "reviewer",
            "reviewer",
            "Review",
            "changed",
            "Review with care.",
            Some(old.version),
        )
        .unwrap();
    assert!(updated.version > old.version);
    assert!(b.role_reset("reviewer", Some(old.version)).is_err());
    assert_eq!(
        b.role_reset("reviewer", Some(updated.version))
            .unwrap()
            .system_prompt,
        old.system_prompt
    );
    assert!(b.role_delete("reviewer").is_err());
    let custom = b
        .role_create("copy-editor", "Editor", "Copy", "Check examples.")
        .unwrap();
    b.role_edit(
        "copy-editor",
        "writer",
        "Writer",
        "Copy",
        "Check examples.",
        Some(custom.version),
    )
    .unwrap();
    let dir = t.path().join("examples");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("SKILL.md"),"---\nname: prove-examples\ndescription: >\n  Check the examples.\n---\nUse a disposable DB.").unwrap();
    fs::create_dir(dir.join("references")).unwrap();
    fs::write(dir.join("references/criteria.md"), "Restart and reread.").unwrap();
    let library = b.skill_import(&dir).unwrap();
    assert_eq!(library.info.slug, "prove-examples");
    assert_eq!(library.files.len(), 2);
    for role in ["writer", "reviewer"] {
        b.role_attach_skill(role, "prove-examples").unwrap();
    }
    let affected = b.skill_show("prove-examples").unwrap().info.roles;
    assert_eq!(affected, ["reviewer", "writer"]);
    assert!(
        b.delete_skill_bundle("prove-examples", false)
            .unwrap_err()
            .to_string()
            .contains("reviewer, writer")
    );
    b.role_detach_skill("writer", "prove-examples").unwrap();
    assert!(
        b.skill_delete_confirmed("prove-examples", &affected)
            .is_err()
    );
    let v = b.role_show("reviewer").unwrap().version;
    assert_eq!(b.skill_import(&dir).unwrap().info.version, 2);
    assert!(b.role_show("reviewer").unwrap().version > v);
    assert_eq!(
        b.delete_skill_bundle("prove-examples", true).unwrap(),
        ["reviewer"]
    );
    b.role_delete("writer").unwrap();
    drop(b);
    let b = Board::open(&db).unwrap();
    assert_eq!(b.roles_list().unwrap().len(), 6);
    assert!(b.skill_show("prove-examples").is_err());
}

#[test]
fn unsafe_and_oversized_imports_leave_the_library_unchanged() {
    let t = tempdir().unwrap();
    let b = Board::open(t.path().join("db")).unwrap();
    let dir = t.path().join("sample");
    fs::create_dir(&dir).unwrap();
    assert!(b.skill_import(&dir).is_err());
    fs::write(dir.join("SKILL.md"), "---\nname: ../outside\n---\ntext").unwrap();
    assert!(b.skill_import(&dir).is_err());
    fs::write(dir.join("SKILL.md"), "hello").unwrap();
    for (name, data) in [
        (".secret", vec![1]),
        ("bin", vec![0, 0]),
        ("invalid", vec![255]),
        ("huge", vec![b'x'; 256 * 1024 + 1]),
    ] {
        fs::write(dir.join(name), data).unwrap();
        assert!(b.skill_import(&dir).is_err(), "{name}");
        fs::remove_file(dir.join(name)).unwrap();
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(t.path().join("db"), dir.join("link")).unwrap();
        assert!(b.skill_import(&dir).is_err());
        fs::remove_file(dir.join("link")).unwrap();
    }
    for i in 0..4 {
        fs::write(dir.join(i.to_string()), vec![b'x'; 256 * 1024]).unwrap();
    }
    assert!(b.skill_import(&dir).is_err());
    for i in 0..32 {
        fs::write(dir.join(i.to_string()), "x").unwrap();
    }
    assert!(b.skill_import(&dir).is_err());
    assert!(b.skill_show("sample").is_err());
}

#[test]
fn coding_artifacts_use_local_validated_schemas() {
    let t = tempdir().unwrap();
    let b = Board::open(t.path().join("db")).unwrap();
    let v = b.schema("verdict", 1).unwrap();
    assert!(v.built_in);
    assert!(v.accepts_sample(&serde_json::json!({"status":"not-run","summary":"driver unavailable","checks":[{"name":"capture","status":"not-run"}]})).unwrap());
    assert!(
        !v.accepts_sample(&serde_json::json!({"status":"looks fine","summary":"oops"}))
            .unwrap()
    );
    assert!(
        !v.accepts_sample(
            &serde_json::json!({"status":"pass","summary":"ok","checks":[{"name":"test"}]})
        )
        .unwrap()
    );
    let d = b.schema("diff", 1).unwrap();
    assert!(
        d.built_in
            && d.accepts_sample(&serde_json::json!({"patch":"+change","files":["main.rs"]}))
                .unwrap()
    );
    assert!(!d.accepts_sample(&serde_json::json!({"files":[]})).unwrap());
}
