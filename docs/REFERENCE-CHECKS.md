# Reproduce the reference checks

Board does not install the original client or runtime. These optional
development checks require Node 26, an already built Rust binary, and a
separate checkout at revision `c2d3324b117adaa8e9a0107e15ea2fca207c95ac`.
They create isolated disposable databases, never use your normal board, and
do not start a public service. From this workspace:

```sh
ref=/home/tom/code/tmp/board                 # or your read-only checkout
bin="$PWD/target/release/board"
git -C "$ref" rev-parse HEAD
node scripts/compare-reference.mjs "$ref" "$bin"
node scripts/compare-schema-values.mjs "$ref" "$bin"
node scripts/compare-reactions.mjs "$ref" "$bin"
node scripts/compare-profile-handles.mjs "$ref" "$bin"
```

These scripts load pure reference code. To exercise its actual local
SQLite backend, copy the reference source to a **new temporary directory**.
Only this development setup fetches the four original backend dependencies;
they are neither dependencies of Board nor shipped in a release:

```sh
audit=$(mktemp -d)
cp -R "$ref/packages/board/src" "$audit/src"
npm install --prefix "$audit" --ignore-scripts --no-audit --no-fund \
  zod@4.6.5 experimental-a2@0.24.0 ajv@8.17.1 ajv-formats@2.1.1
node --disable-warning=MODULE_TYPELESS_PACKAGE_JSON scripts/run-reference-conformance.mjs "$audit"
node --disable-warning=MODULE_TYPELESS_PACKAGE_JSON scripts/compare-workflows.mjs "$audit" "$bin"
node --disable-warning=MODULE_TYPELESS_PACKAGE_JSON scripts/compare-work.mjs "$audit" "$bin" --expiry
node --disable-warning=MODULE_TYPELESS_PACKAGE_JSON scripts/compare-private-channels.mjs "$audit" "$bin"
node --disable-warning=MODULE_TYPELESS_PACKAGE_JSON scripts/compare-platform-blocks.mjs "$audit" "$bin"
```

Both workflow scripts call real original backend methods and real Rust
processes. They compare observable behavior (not arbitrary ID numbers) and
fail on a mismatch. Their transcripts include ordinary versus all-event waits,
individual reply cursors, zero-based retry numbers and the renewal proof seen
by the successful holder and the strict `--if-pending` acknowledgement. The
work script separately asserts one default response difference: repeating an acknowledgement returns success
for the idempotent local resource rather than `no_match`. The slow case waits for an actual lease
to expire. These comparisons do **not** certify wire compatibility, every
possible input, or the operation of a third-party account.

As a control, `run-reference-conformance.mjs` runs the unmodified original conformance
kit **against the original backend**, not Board's REST API. With one local person,
real profile/narrowing headers and no hosted account, the original reported **86 passed,
5 skipped, 0 failed**. Its 91 results include the discovery handshake as well as the
90 statically inventoried cases. It prints each skip's reason.

The private-channel check additionally invokes the original client against the original HTTP
handler, supplying two synthetic local identities, not an external login. It compares 17 outcomes:
parent and sibling profiles, someone else's access at the root and below it, whole-path
versus sibling matching, and conflict behavior before any grants are changed. The platform-block
check sends the same four schema-shaped texts through a native post and an external import:
native content is validated, while external content remains literal. Rust-only tests separately
cover competing writers, transaction rollback, provider edits and upgrades of older databases.

## Prove local operation

On Linux with Bubblewrap and a working local software Vulkan driver, run the
three-process round trip with **only loopback** available. This is optional;
neither Bubblewrap nor Node is an application dependency. From the workspace:

```sh
t=$(mktemp -d)
install -d -m700 "$t/home" "$t/runtime"
bwrap --ro-bind / / --dev /dev --bind "$t" "$t" --unshare-net \
  --setenv HOME "$t/home" --setenv TMPDIR "$t" --setenv XDG_RUNTIME_DIR "$t/runtime" \
  -- python3 scripts/roundtrip.py --bin-dir "$PWD/target/release" --artifacts "$t/evidence"
```

The check posts through the CLI, reads and replies through actual HTTP,
waits for an already-open Rune inspector, closes the clients and verifies
the fresh processes and a separately opened backup. The two PNGs and Rune
element trees are kept alongside the disposable DB. On a machine with a
different software driver, set `VK_DRIVER_FILES` to its local ICD manifest.

## Bounded native test runs

The normal release workspace command is in [PARITY](../PARITY.md#verification-record).
For runners with a per-job wall-clock limit, split native rendering without dropping
tests. Build the release binaries first. Run the non-UI workspace and the native
binary's unit tests, then run the native integration cases in bounded batches:

```sh
export RUSTUP_TOOLCHAIN=stable
cargo build --release --offline --locked -j 4 --workspace --bins
cargo test --release --offline --locked -j 4 --workspace --exclude board-ui -- --include-ignored
cargo test --release --offline --locked -j 4 -p board-ui --bin board-ui -- --include-ignored
python3 scripts/test-inspector.py
```

Alternatively run `python3 scripts/test-inspector.py --shard N --shards 12` for **every**
N from 1 through 12. The script discovers tests from Cargo's built executable and
partitions exact test names; it never silently skips a case marked ignored.
