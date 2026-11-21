# What runs where

For a personal board, install the two binaries and run `board post` or
`board-ui`. There is no initial service setup. GitHub is still the Git host;
Slack is still the chat service; Linear is still the issue tracker. None is
required to use Board, and installing Board does not connect any account.

| Responsibility | Local replacement | What needs to run |
| --- | --- | --- |
| Database, blobs and event stream | Bundled SQLite, WAL, transactions and online backup. Bytes aren't stored behind expiring download URLs. | Nothing besides the client. |
| Timers, leases and subscriptions | Durable schedules, fenced task claims, watcher cursors and per-thread delivery receipts in SQLite. | `board worker` fires timers. A parked or stopped worker doesn't lose work. Consumers pull or run their own callback. |
| Identity and permissions | Single-user filesystem trust by default; optional local fixed-actor bearer credentials with channel grants, expiry and revocation. | No account server. See [access and files](ACCESS.md). |
| Desktop and programmatic use | Native Rune, Rust domain API, scriptable CLI, optional Axum REST with event streaming. | GUI and CLI open SQLite directly. Start `board serve` only for an HTTP client. |
| External conversations | Optional Rust HTTPS clients for GitHub, Slack and Linear; original records, immutable copy mappings and delivery receipts stored locally. | Only an explicit provider command initiates network access. See the [connector guide](SYNC.md). |
| Local agents | Claim and renew work for an explicitly supplied program, or lease watcher events and handle them with a local executable. The board doesn't interpret messages as programs. | Bring your own executable; no model account, container runtime or sandbox product is required. See [automation](AUTOMATION.md). |
| Deployment reports | A neutral, locally validated `deployment@1` structured message can carry a URL/commit. | No hosting platform required. |

## Local-only scope

The only listener binds to IPv4 loopback. It has no bind-host option, public
webhook, tunnel, browser cookies, telemetry, or hosted control plane. Don't
expose the port through a proxy or share a SQLite file on a network drive.
An OS process able to change the database directly is already trusted.

The [domain](../crates/board-core) accepts authorized operations independent
of transport. The [HTTP adapter](../crates/board-api) supplies identity and
owns listener policy; the [connector crate](../crates/board-sync) owns external
requests. A future team deployment could reuse these boundaries. It would need
independent user authentication, TLS, host and storage operations and its own
testing. It is **not implemented or supported** by this release.
