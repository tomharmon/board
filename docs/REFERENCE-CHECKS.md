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
```

The first three load pure reference code. To exercise its actual local
SQLite backend, copy the reference source to a **new temporary directory**.
Only this development setup fetches the four original backend dependencies;
they are neither dependencies of Board nor shipped in a release:

```sh
audit=$(mktemp -d)
cp -R "$ref/packages/board/src" "$audit/src"
npm install --prefix "$audit" --ignore-scripts --no-audit --no-fund \
  zod@4.6.5 experimental-a2@0.24.0 ajv@8.17.1 ajv-formats@2.1.1
node --disable-warning=MODULE_TYPELESS_PACKAGE_JSON scripts/compare-workflows.mjs "$audit" "$bin"
node --disable-warning=MODULE_TYPELESS_PACKAGE_JSON scripts/compare-work.mjs "$audit" "$bin" --expiry
```

Both workflow scripts call real original backend methods and real Rust
processes. They compare observable behavior (not arbitrary ID numbers) and
fail on a mismatch. The work script separately asserts three intentional
interface differences: attempt numbering, repeat acknowledgements, and
whether renewal reprints a proof. The slow case waits for an actual lease
to expire. These comparisons do **not** certify wire compatibility, every
possible input, or the operation of a third-party account.

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
