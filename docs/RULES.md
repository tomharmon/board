# Local write rules

Rules are optional owner-installed [Rhai](https://rhai.rs/) expressions. Installing a rule affects the board, not just the current interface. Nothing in a message, file, webhook, or other untrusted text is evaluated as source code. The CLI, REST, GUI, worker and sync imports all use the same pipeline for posts, replies and text edits.

Example `review.rhai`:

```rhai
if write.channel == "build/review" && write.operation == "post" {
    let labels = write.labels;
    labels["queue"] = "review";
    #{ labels: labels }
}
```

```sh
board rules set review ./review.rhai --priority 10
board post build/review "Please check this"
board rules list
board rules remove review
```

The constant `write` has `operation` (`post` or `edit`), `text`, `labels`, `actor`, `channel`, nullable `root` and `id`, `has_files`, `no_sync`, and nullable `source` (source key, thread and external message ID). Rules run in ascending priority, then name. Return nothing or an empty map to pass; `#{text:"..."}` replaces text and `#{labels:...}` replaces labels on a new message. `#{drop:"reason"}` rejects the write. One rule's output is the next one's input. An edit still has to match its revision and belong to the caller.

A result may also include `emit:[#{channel:"audit",text:"review opened"}]`, or `reply_to` instead of `channel`. These posts originate as `board:middleware` rather than impersonating the caller, and do not reenter the rule chain. A private write keeps its derived posts private. Effects, validation, the originating post and their events share one transaction. Rejections, crashes and invalid blocks do not leave side effects behind; a keyed replay does not run the chain again. A policy drop permanently skips a queued import and cancels a scheduled send rather than retrying indefinitely.

Use `#{drop:"policy reason", respond:"Please supply a ticket number"}` for an explicit
reply. An ordinary caller gets a 422 with the response text. For a shareable
import at a publishing-enabled source, the rejection durably queues one
response addressed to the original remote thread. It does not fabricate a
Board message. Only an explicit connector send transmits it, and its receipt
suppresses the provider echo. A diagnostic `drop` without `respond` is never
posted externally.

Only a trusted, unrestricted owner can read or install rule code. There are at most 16 rules, 8 KiB per program, 30,000 evaluation operations per rule, and eight emitted messages per write. There are no IO bindings, module imports, dynamic evaluation, wall clock, external commands or network client. See [Rhai operation limits](https://rhai.rs/book/safety/max-operations.html) and the [transactional tests](../crates/board-core/tests/write_rules.rs). This is a local write policy, not a JavaScript compatibility layer or a mechanism for synchronous remote callbacks.
