# Historical SQLite fixtures

These are gzip-compressed copies of actual disposable databases, not current databases with a version number changed. None has credentials.

- V7: built the historical Rust CLI at `68cb9f3`, posted `Original v7 thread` to `ops`, and inserted the legacy three-field draft into its original `composer_drafts` table.
- V8: created and inspected with the pre-migration Rust CLI and Rune binary; the sole thread in `studio/review` attaches the project's inspector screenshot. The fixture is a SQLite backup of that file, with a legacy reply draft added.

`composer_recovery.rs` verifies the original version and absence of the new file storage before opening each with the current implementation.

The v14 fixture was created with the pre-v15 release CLI. It has an ordinary message and a ready task, plus one row inserted in its original write-rule table. The swarm migration test verifies this historical layout before opening it with the current binary.
