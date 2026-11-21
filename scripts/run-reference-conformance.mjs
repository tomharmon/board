// Controls only the reference: do not count these as passes for the Rust port.
import assert from 'node:assert/strict';
import {resolve, join} from 'node:path';
import {pathToFileURL} from 'node:url';

if (process.argv.length !== 3) throw Error('usage: node scripts/run-reference-conformance.mjs ORIGINAL_PACKAGE_WITH_DEPS');
const load = f => import(pathToFileURL(join(resolve(process.argv[2]), 'src', f)).href);
const {local} = await load('backend-local/index.ts');
const {createBoard} = await load('server/board.ts');
const {createHttp} = await load('server/backend-http.ts');
const {conformance} = await load('conformance/index.ts');
const db = local({memory: true}), url = 'http://127.0.0.1/board';
const internal = {serving: null, sandbox: null, backend() { throw Error('unexpected external backend'); }};
try {
  const server = createBoard({backend: db, url, basePath: '/board',
    authenticate: () => ({id: 'local:user:operator', kind: 'person', handle: 'operator'})}, internal);
  function client(headers = {}) {
    const backend = createHttp(url, {fetch(input, init) {
      const request = new Request(input, init);
      for (const [k, v] of Object.entries(headers)) request.headers.set(k, v);
      return server.fetch(request);
    }}, null);
    return createBoard({backend}, internal).as(null);
  }
  const report = await conformance(client(), {withHeaders: client, withCan: can => client({'x-board-can': can})});
  const counts = {}, groups = {};
  for (const check of report.checks) {
    counts[check.status] = (counts[check.status] ?? 0) + 1;
    groups[check.group] = (groups[check.group] ?? 0) + 1;
    if (check.status !== 'passed') console.log(check.status, check.group + ':', check.name, '-', check.detail);
  }
  console.log('Original conformance kit against original SQLite/HTTP, NOT the Rust port:', counts, groups);
  assert.ok(report.ok);
} finally { await db.close(); }
