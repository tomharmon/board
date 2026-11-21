// Developer-only: exercise the public APIs against two real, isolated SQLite boards.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

if (process.argv.length !== 4) throw Error('usage: node scripts/compare-platform-blocks.mjs ORIGINAL_PACKAGE_WITH_DEPS BOARD_BINARY');
const [pkg, exe] = process.argv.slice(2).map(p => resolve(p));
const load = f => import(pathToFileURL(join(pkg, 'src', f)).href);
const { local } = await load('backend-local/index.ts'), { createBoard } = await load('server/board.ts');
const dir = mkdtempSync(join(tmpdir(), 'board-platform-text-'));
const db = local({ dir: join(dir, 'original') });
const original = createBoard({ backend: db }, { serving: null, sandbox: null, backend() { throw Error('unexpected external backend'); } });
const cli = (args, input) => {
  const r = spawnSync(exe, ['--json', ...args], { encoding: 'utf8', timeout: 10000, input: input && JSON.stringify(input),
    env: { PATH: process.env.PATH, HOME: dir, BOARD_DB: join(dir, 'port.db'), BOARD_USER: 'writer' } });
  if (r.error) throw r.error;
  const value = JSON.parse(r.status === 0 ? r.stdout : r.stderr);
  if (r.status !== 0) throw Error(value.code);
  return value;
};
const shape = { type: 'object', required: ['count'], properties: { count: { type: 'integer' } } };
const view = m => ({ text: m.text, blocks: m.blocks });
async function compareAction(op) {
  try { return { ok: true, value: view(await op()) }; }
  catch (e) { return { ok: false, code: e.code ?? e.message }; }
}
try {
  await original.register({ watch: {}, sync: [{ key: 'inbound', platform: 'inbound' }] });
  cli(['sources','add','inbound','local','inbound:room','samples']);
  await original.createSchema({ name: 'metric', version: 1, description: 'Sample', schema: shape });
  cli(['schemas','create','metric@1','--description','Sample','--data',JSON.stringify(shape)]);
  const author = { id: 'inbound:u1', handle: 'reader', name: 'Reader', avatar: null, isBot: false };
  const fence = '`'.repeat(3);
  for (const [i, [name, value]] of [['metric@1','{"count":5}'],['metric@1','{"count":"wrong"}'],['absent@1','{}'],['metric@1','broken']].entries()) {
    const text = [fence + 'json#' + name, value, fence].join('\n');
    const key = 'source-' + i;
    const first = await original.syncImport({ op: 'post', id: key, author, text, from: 'inbound', channel: 'samples',
      origin: { channel: 'inbound:room', thread: 'inbound:' + i, subthread: null, message: 'item', context: null } });
    const second = cli(['sync','import','inbound','-'],{ key, import: { thread: key, external: 'item', author: 'u1', change: { op: 'post', text } } });
    assert.deepEqual(view(second), view((await original.thread(first.message)).message));
    assert.deepEqual(view(second), { text, blocks: [] });
    const expected = await compareAction(() => original.post('samples', text));
    const got = await compareAction(() => cli(['post','samples',text]).message);
    assert.deepEqual(got, expected);
    assert.equal(got.ok, i === 0);
  }
  console.log('Both local backends agree on eight native/platform block outcomes (registered, mismatched, unknown and malformed).');
} finally {
  // The reference documents close() for an idle board; its local scheduler drains after a write.
  await delay(500);
  await db.close();
  rmSync(dir, { recursive: true, force: true });
}
