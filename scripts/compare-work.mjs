// Development only: exercise the public interfaces of two independent, disposable SQLite boards.
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {randomBytes} from 'node:crypto';
import {mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {setTimeout as pause} from 'node:timers/promises';
import {pathToFileURL} from 'node:url';

const expiry = process.argv.length === 5 && process.argv[4] === '--expiry';
if (process.argv.length !== 4 && !expiry) throw Error('usage: node scripts/compare-work.mjs ORIGINAL_PACKAGE_WITH_DEPS BOARD_BINARY [--expiry]');
const pkg = resolve(process.argv[2]), bin = resolve(process.argv[3]);
const load = file => import(pathToFileURL(join(pkg, 'src', file)).href);
const {createBoard} = await load('server/board.ts'), {local} = await load('backend-local/index.ts');
globalThis.fetch = async () => { throw Error('network fetch is not part of this local comparison'); };
const dir = mkdtempSync(join(tmpdir(), 'board-work-'));
const token = () => randomBytes(32).toString('base64url');
const wch = 'comparison/triage', sch = 'comparison/timers', tch = 'comparison/jobs';
const labels = {project: 'comparison'};

function reference() {
  const db = local({dir: join(dir, 'reference')});
  const b = createBoard({backend: db}, {serving: null, sandbox: null, backend() { throw Error('external backend'); }});
  const claim = c => ({id: c.task.id, epoch: c.epoch, proof: c.token, deadline: Date.parse(c.until)});
  return {
    post: (channel, text, tagged) => b.post(channel, {text, ...(tagged && {labels})}),
    reply: (id, text) => b.reply(id, {text, labels}), thread: id => b.thread(id),
    register: on => b.register({watch: on ? {triage: {tail: wch + ' --label project=comparison'}} : {}}),
    names: async () => (await b.watchers()).flatMap(row => Object.keys(row.watch)).sort(),
    pull: async () => (await b.matches()).map(d => ({
      watcher: d.watcher, message: d.event.message.id, key: d.id, receipt: d.lease, attempt: d.attempt,
    })),
    ack: (d, options) => b.ack(d.key, {lease: d.receipt, ...options}),
    ackPending: d => b.ack(d.key, {lease:d.receipt}),
    schedule: (id, text, timing) => b.schedule({re: id}, {text, ...timing, labels, noSync: true}),
    schedules: (id, all) => b.schedules({re: id, all}),
    cancel: async id => (await b.cancelSchedule(id)).schedule, tick() {},
    task: async (text, needs = []) => (await b.tasks.create(tch, {text, needs})).task.id,
    profileTask: text => b.as('runner').tasks.create(tch, {text}),
    mine: async () => (await b.tasks.list({channel: tch, by: 'me', state: 'all'})).tasks.map(t => t.message.text).sort(),
    claim: async (id, proof, seconds) => claim(await b.tasks.claim(id, {token: proof, lease: seconds})),
    renew: async (c, seconds) => claim(await b.tasks.held(c.id, {epoch: c.epoch, token: c.proof}).renew(seconds)),
    held: (c, action, arg) => b.tasks.held(c.id, {epoch: c.epoch, token: c.proof})[action](arg),
    reopen: id => b.tasks.reopen(id), block: id => b.tasks.block(id, 'review required', {kind: 'approval'}),
    unblock: id => b.tasks.unblock(id),
    // Its local scheduler arms timers just after a response. Let outstanding
    // local callbacks settle before closing the underlying SQLite store.
    close: async () => { await pause(100); await db.close(); },
  };
}

const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => !k.startsWith('BOARD_')));
Object.assign(env, {BOARD_DB: join(dir, 'rust.db'), BOARD_USER: 'auditor', HOME: dir, XDG_STATE_HOME: dir,
  XDG_CACHE_HOME: dir, XDG_CONFIG_HOME: dir, XDG_DATA_HOME: dir});
function cli(args, input) {
  const r = spawnSync(bin, ['--json', ...args.map(String)], {input, env, encoding: 'utf8', timeout: 10000});
  if (r.error) throw r.error;
  if (r.status !== 0) {
    const e = JSON.parse(r.stderr);
    throw Object.assign(Error(e.message), {code: e.code});
  }
  return JSON.parse(r.stdout);
}
function port() {
  const claim = c => ({id: c.id, epoch: c.epoch, proof: c.claim_token, deadline: c.lease_until * 1000});
  const proof = c => ['--epoch', c.epoch, '--token', c.proof];
  return {
    post: (ch, text, tagged) => cli(['post', ch, text, ...(tagged ? ['--label', 'project=comparison'] : [])]).message,
    reply: (id, text) => cli(['reply', id, text, '--label', 'project=comparison']).message,
    thread: id => cli(['read', id]),
    register: on => cli(['watchers', 'replace', '-'], JSON.stringify(on ? [{name: 'triage', filter: {channel: wch, labels}}] : [])),
    names: () => cli(['watchers', 'list']).map(w => w.name).sort(),
    pull: () => cli(['watchers', 'matches']).map(d => ({
      watcher: d.watcher, message: d.event.message.id, key: d.trigger, cursor: d.event.cursor, receipt: d.receipt, attempt: d.attempt,
    })),
    ack: (d, options = {}) => cli(['watchers', 'ack', d.watcher, d.cursor, '--receipt', d.receipt,
      ...(options.error ? ['--error', options.error] : []), ...(options.skip ? ['--skip'] : [])]),
    ackPending: d => cli(['watchers','ack',d.watcher,d.cursor,'--receipt',d.receipt,'--if-pending']),
    schedule: (id, text, timing) => cli(['schedule', id, text, '--no-sync', '--label', 'project=comparison',
      ...Object.entries(timing).flatMap(([k, v]) => ['--' + k, v])]),
    schedules: (id, all) => cli(['schedules', id, ...(all ? ['--all'] : [])]),
    cancel: id => cli(['schedule', 'cancel', 's' + id]), tick: () => cli(['worker', '--once']),
    task: (text, needs = []) => cli(['tasks', 'create', tch, text, ...needs.flatMap(id => ['--needs', id])]).id,
    profileTask: text => cli(['--as', 'runner', 'tasks', 'create', tch, text]),
    mine: () => cli(['tasks', tch, '--by', 'me', '--state', 'all']).map(t => t.text).sort(),
    claim: (id, p, seconds) => claim(cli(['tasks', 'claim', id, '--token', p, '--lease', seconds + 's'])),
    renew: (c, seconds) => claim(cli(['tasks', 'renew', c.id, ...proof(c), '--lease', seconds + 's'])),
    held: (c, action, arg) => cli(['tasks', action, c.id, ...(arg === undefined ? [] : [arg]), ...proof(c)]),
    reopen: id => cli(['tasks', 'reopen', id]), block: id => cli(['tasks', 'block', id, 'review required', '--kind', 'approval']),
    unblock: id => cli(['tasks', 'unblock', id]), close() {},
  };
}

async function failure(f) {
  try { await f(); } catch (e) { if (typeof e.code === 'string') return e.code; throw e; }
  return null;
}
async function until(label, f, ready) {
  const deadline = Date.now() + 10000;
  let value;
  do {
    value = await f();
    if (ready(value)) return value;
    await pause(80);
  } while (Date.now() < deadline);
  throw Error('timed out: ' + label);
}

async function transcript(open) {
  const out = {watchers: {}, schedules: {}, tasks: {}}, differences = {};
  let b = open();
  function check(group, name, actual, expected) {
    assert.deepEqual(actual, expected, open.name + ': ' + group + '.' + name);
    out[group][name] = actual;
  }
  function rejected(group, name, code, ...allowed) {
    assert.ok(allowed.includes(code), open.name + ': ' + name + ': ' + code);
    check(group, name, code !== null, true);
  }
  async function restart() { await b.close(); b = open(); }
  const batch = async wanted => {
    const matches = [];
    return until('watcher deliveries (' + wanted + ')', async () => { matches.push(...await b.pull()); return matches; }, ds => ds.length >= wanted);
  };
  try {
    const watch = (name, a, e) => check('watchers', name, a, e);
    await b.post(wch, 'before registration', true);
    await b.register(true);
    watch('registeredSet', await b.names(), ['triage']);
    await b.post(wch, 'wrong label', false);
    await b.post('comparison/unwatched', 'wrong channel', true);
    const left = await b.post(wch, 'left', true);
    const next = await b.reply(left.id, 'left second');
    const right = await b.post(wch, 'right', true);
    const offered = await batch(2), l = offered.find(d => d.message === left.id), r = offered.find(d => d.message === right.id);
    watch('independentFilteredThreadHeads', [offered.length, l?.watcher, r?.watcher], [2, 'triage', 'triage']);
    watch('firstDeliveryAttemptStartsAtZero',l.attempt,0);
    watch('leaseBlocksOtherPull', await b.pull(), []);
    await b.register(true);
    await restart();
    watch('unchangedRegistrationAndRestartKeepLeases', [await b.names(), await b.pull()], [['triage'], []]);
    rejected('watchers', 'wrongReceipt', await failure(() => b.ack({...l, receipt: token()})), 'stale_lease', 'delivery_conflict');
    await b.ack(l);
    const [follow] = await batch(1);
    watch('ackAdvancesThatThread', follow.message === next.id, true);
    await b.ack(follow, {error: 'retry this input'});
    const [retry] = await batch(1);
    watch('redeliveryPreservesInput', retry.message === next.id && retry.key === follow.key, true);
    watch('redeliveryRotatesProofAndCountsAttempt', retry.receipt !== follow.receipt && retry.attempt === follow.attempt + 1, true);
    rejected('watchers', 'staleAttemptCannotFinish', await failure(() => b.ack(follow)), 'stale_lease', 'delivery_conflict');
    await b.ack(retry, {skip: true}); await b.ackPending(r);
    watch('skipAndAckDrainQueue', await b.pull(), []);
    differences.repeatedAck = await failure(() => b.ack(r));
    rejected('watchers','strictRepeatReportsNoMatch',await failure(()=>b.ackPending(r)),'no_match');
    await b.register(false);
    watch('emptySetRemovesSubscriptions', await b.names(), []);

    const sched = (name, a, e) => check('schedules', name, a, e);
    const root = await b.post(sch, 'root'), other = await b.post(sch, 'other thread');
    const future = await b.schedule(root.id, 'future', {in: '1d'});
    const nosync = v => v.noSync ?? v.no_sync;
    sched('pendingReplyMetadata', [future.state, future.re === root.id, future.sent, nosync(future), future.labels], ['pending', true, 0, true, labels]);
    const pending = await b.schedules(root.id, false);
    sched('pendingListKeepsFlag', pending.map(s => [s.id === future.id, nosync(s)]), [[true, true]]);
    sched('differentThreadExcludesSchedule', await b.schedules(other.id, false), []);
    sched('threadListsPendingSchedule', (await b.thread(root.id)).schedules.some(s => s.id === future.id), true);
    await restart();
    sched('pendingSurvivesRestart', (await b.schedules(root.id, false)).map(s => s.text), ['future']);
    const canceled = await b.cancel(future.id);
    sched('cancelClosesDeadline', [canceled.state, canceled.next, canceled.canceled.reason], ['canceled', null, 'canceled']);
    sched('pendingOmitsButHistoryKeepsCanceled', [(await b.schedules(root.id, false)).length, (await b.schedules(root.id, true))[0].state], [0, 'canceled']);
    sched('secondCancelKeepsState', (await b.cancel(future.id)).state, 'canceled');
    const alarm = await b.schedule(root.id, 'fires once', {in: '1s'});
    const sent = await until('public scheduler dispatch', async () => {
      await b.tick(); return (await b.schedules(root.id, true)).find(s => s.id === alarm.id);
    }, s => s?.state === 'done');
    const replies = (await b.thread(root.id)).replies;
    sched('oneShotPostsExactlyOnce', [sent.sent, sent.next, replies.length, replies[0]?.id === sent.last], [1, null, 1, true]);
    sched('sendRetainsOriginLabelsAndNoSync', [replies[0].schedule.id === alarm.id, replies[0].schedule.n, nosync(replies[0]), replies[0].labels], [true, 0, true, labels]);
    await restart(); await b.tick();
    sched('restartDoesNotResend', (await b.thread(root.id)).replies.map(m => m.text), ['fires once']);

    const task = (name, a, e) => check('tasks', name, a, e);
    const id = await b.task('prerequisite'), dependent = await b.task('dependent', [id]);
    await b.profileTask('profile-owned');
    task('byMeExcludesMyProfile', await b.mine(), ['dependent', 'prerequisite']);
    const view = async id => (await b.thread(id)).message.task;
    task('dependentStartsWaiting', [(await view(dependent)).state, (await view(dependent)).needs.includes(id)], ['waiting', true]);
    rejected('tasks', 'unreadyCannotClaim', await failure(() => b.claim(dependent, token(), 60)), 'task_waiting');
    rejected('tasks', 'shortLeaseRefused', await failure(() => b.claim(id, token(), 5)), 'invalid_request', 'invalid_input');
    const chosen = token(), first = await b.claim(id, chosen, 60);
    task('requestedProofAndDeadline', [first.proof === chosen, Math.abs(first.deadline - Date.now() - 60000) < 5000], [true, true]);
    const replay = await b.claim(id, chosen, 60);
    task('claimRetryLeavesLeaseIntact', replay.epoch === first.epoch && replay.deadline === first.deadline && replay.proof === first.proof, true);
    rejected('tasks', 'concurrentHolderRefused', await failure(() => b.claim(id, token(), 60)), 'task_claimed');
    task('proofNotInPublicThread', !JSON.stringify(await b.thread(id)).includes(chosen), true);
    const renewed = await b.renew(first, 300);
    task('renewReturnsOnlyTheHoldersPrivateProof',renewed.proof === chosen,true);
    const afterRenew = await b.claim(id, chosen, 60);
    task('renewKeepsProofAndEpoch', [afterRenew.proof === chosen && afterRenew.deadline === renewed.deadline, renewed.epoch === first.epoch, renewed.deadline - first.deadline >= 239000], [true, true, true]);
    await restart();
    rejected('tasks', 'wrongProofAfterRestart', await failure(() => b.held({...first, proof: token()}, 'complete', 'wrong')), 'claim_lost');
    await b.held(first, 'release');
    task('releaseRestoresReady', (await view(id)).state, 'ready');
    const second = await b.claim(id, token(), 60);
    task('newClaimIncrementsEpoch', second.epoch - first.epoch, 1);
    rejected('tasks', 'staleRelease', await failure(() => b.held(first, 'release')), 'claim_lost');
    rejected('tasks', 'staleComplete', await failure(() => b.held(first, 'complete', 'too late')), 'claim_lost');
    await b.held(second, 'fail', 'needs repair');
    const failed = await view(id);
    task('failureRecordsReasonWithoutUnblockingDependents', [failed.state, failed.reason ?? failed.result, (await view(dependent)).state], ['failed', 'needs repair', 'waiting']);
    await b.reopen(id); await b.block(id);
    const blocked = await view(id);
    task('ownerHoldPersistsKind', [blocked.state, blocked.blocked?.kind ?? blocked.hold_kind], ['blocked', 'approval']);
    rejected('tasks', 'blockedCannotClaim', await failure(() => b.claim(id, token(), 60)), 'task_blocked');
    await b.unblock(id); await b.held(await b.claim(id, token(), 60), 'complete', 'verified');
    await restart();
    task('completionUnblocksAfterRestart', [(await view(id)).state, (await view(id)).result, (await view(dependent)).state], ['completed', 'verified', 'ready']);
    rejected('tasks', 'terminalCannotClaim', await failure(() => b.claim(id, token(), 60)), 'task_finished');
    if (expiry) {
      const timed = await b.task('wall-clock takeover'), old = await b.claim(timed, token(), 30);
      await restart();
      await pause(Math.max(0, old.deadline - Date.now() + 500));
      const live = await b.claim(timed, token(), 60);
      task('expiredLeaseCanBeTakenOverAfterRestart', [live.epoch - old.epoch, old.proof !== live.proof], [1, true]);
      rejected('tasks', 'expiredHolderCannotCompleteAfterTakeover', await failure(() => b.held(old, 'complete', 'too late')), 'claim_lost');
      await b.held(live, 'complete', 'new holder');
      await restart();
      task('takeoverResultSurvives', [(await view(timed)).state, (await view(timed)).result], ['completed', 'new holder']);
    }
    return {out, differences};
  } finally { await b.close(); }
}

try {
  // Independent boards can wait on the same wall-clock interval. Always close both before removing the directory.
  const results = await Promise.allSettled([transcript(reference), transcript(port)]);
  for (const r of results) if (r.status === 'rejected') throw r.reason;
  const [ref, rust] = results.map(r => r.value);
  assert.deepEqual(rust.out, ref.out);
  assert.deepEqual(ref.differences, {repeatedAck: 'no_match'});
  assert.deepEqual(rust.differences, {repeatedAck: null});
  const counts = Object.fromEntries(Object.entries(rust.out).map(([group, rows]) => [group, Object.keys(rows).length]));
  console.log('Equivalent measured outcomes:', counts, 'total', Object.values(counts).reduce((a, b) => a + b, 0));
  console.log('Measured differences (original, Rust):', ref.differences, rust.differences);
} finally { rmSync(dir, {recursive: true, force: true}); }
