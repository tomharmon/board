// Development only: exercise the original client + handler and the installed Rust CLI.
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';

if (process.argv.length !== 4) throw Error('usage: node scripts/compare-private-channels.mjs ORIGINAL_PACKAGE_WITH_DEPS BOARD_BINARY');
const pkg = resolve(process.argv[2]), bin = resolve(process.argv[3]);
const load = file => import(pathToFileURL(join(pkg, 'src', file)).href);
const {createBoard} = await load('server/board.ts');
const {local} = await load('backend-local/index.ts');
const {createBoardClient} = await load('client/client.ts');
const dir = mkdtempSync(join(tmpdir(), 'board-private-'));
const original = local({dir: join(dir, 'original')});
try {
  const url = 'http://127.0.0.1/board';
  // Only identity is supplied by this fixture. Grant checks run in the unmodified handler.
  const handler = createBoard({backend: original, url, basePath: '/board', authenticate: ({credential}) => {
    if (!['ada', 'bea'].includes(credential)) return null;
    return {id: 'local:user:' + credential, kind: 'person', handle: credential};
  }}, {serving: null, sandbox: null, backend() { throw Error('unexpected network backend'); }});
  function reference(person, profile) {
    const c = createBoardClient({url, token: person, profile, fetch: (request, init) => handler.fetch(new Request(request, init))});
    return {
      create: (name, privateMode) => c.createChannel(name, {private: privateMode}),
      post: (name, text) => c.post(name, {text}),
      channels: async prefix => (await c.channels({prefix})).channels.map(ch => ch.path),
      reserve: name => c.createGrant({principals: [{kind: 'user', userId: 'bea'}], can: {[name]: ['read']}}),
      count: async () => (await c.grants()).length,
    };
  }
  const clean = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('BOARD_')));
  function cli(token, ...args) {
    const r = spawnSync(bin, ['--json', ...args], {env: {...clean, BOARD_DB: join(dir, 'rust.db'), BOARD_USER: 'owner',
      ...(token ? {BOARD_TOKEN: token} : {})}, encoding: 'utf8', timeout: 10000});
    if (r.error) throw r.error;
    if (r.status !== 0) { const body = JSON.parse(r.stderr); throw Object.assign(Error(body.message), {code: body.code}); }
    return JSON.parse(r.stdout);
  }
  for (const user of ['ada', 'bea']) cli(null, 'grants', 'create', user, '--can', '*=all');
  function rust(person, profile) {
    const {token} = cli(null, 'credentials', 'issue', profile ? person + '/' + profile : person);
    return {
      create: (name, priv) => cli(token, 'channels', 'create', name, ...(priv ? ['--private'] : [])),
      post: (name, text) => cli(token, 'post', name, text).message,
      channels: prefix => cli(token, 'channels', '--prefix', prefix).channels.map(ch => ch.name),
      reserve: name => cli(token, 'grants', 'create', 'bea', '--can', name + '=read'),
      count: () => cli(token, 'grants', 'list').grants.length,
    };
  }
  async function rejection(f) {
    try { await f(); } catch (e) { if (!e.code) throw e; return e.code; }
    throw Error('write unexpectedly succeeded');
  }
  async function run(bind) {
    const ada = bind('ada'), desk = bind('ada', 'desk'), reviewer = bind('ada', 'review'), bea = bind('bea');
    const result = {};
    result.emptyCreated = (await ada.create('nearby', false)).created;
    result.emptyReplay = (await ada.create('nearby', false)).created;
    await bea.create('vault-old/notes', false);
    const start = await ada.count();
    const made = await desk.create('vault', true);
    result.privateCreated = made.created;
    result.grantsReturned = made.grants.length;
    result.grantsAdded = await ada.count() - start;
    result.familyCanWrite = (await ada.post('vault', 'by person')).text;
    result.siblingProfileCanWrite = (await reviewer.post('vault/notes', 'by profile')).text;
    result.familyCanList = (await desk.channels('vault')).sort();
    result.unrelatedCanList = (await bea.channels('vault')).sort();
    // Failure codes for denied writes differ by interface. Require an explicit authorization denial.
    for (const [key, path] of [['cannotPost', 'vault'], ['cannotPostBelow', 'vault/notes']]) {
      const code = await rejection(() => bea.post(path, 'unwanted'));
      assert.ok(['forbidden', 'missing_permission'].includes(code), 'unexpected denial: ' + code);
      result[key] = true;
    }
    const before = await ada.count();
    result.repeatPrivate = await rejection(() => ada.create('vault', true));
    result.exactPublic = await rejection(() => ada.create('nearby', true));
    result.publicChild = await rejection(() => ada.create('vault-old', true));
    result.noGrantsOnConflict = await ada.count() === before;
    await ada.reserve('reserved/child');
    result.descendantGrant = await rejection(() => ada.create('reserved', true));
    result.noReservedChannel = (await ada.channels('reserved')).length === 0;
    return result;
  }
  const expected = await run(reference), actual = await run(rust);
  assert.deepEqual(actual, expected);
  assert.deepEqual(actual, {emptyCreated:true, emptyReplay:false, privateCreated:true, grantsReturned:2, grantsAdded:2,
    familyCanWrite:'by person', siblingProfileCanWrite:'by profile', familyCanList:['vault','vault-old/notes','vault/notes'],
    unrelatedCanList:['vault-old/notes'], cannotPost:true, cannotPostBelow:true, repeatPrivate:'channel_granted',
    exactPublic:'channel_exists', publicChild:'channel_exists', noGrantsOnConflict:true, descendantGrant:'channel_granted', noReservedChannel:true});
  console.log('Original client/handler and Rust CLI agree on ' + Object.keys(actual).length + ' private-channel outcomes:\n' + JSON.stringify(actual, null, 2));
} finally { await original.close(); rmSync(dir, {recursive: true, force: true}); }
