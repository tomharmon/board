// Optional development check: upstream determines the expected handle, real Rust commands
// import, display, resolve a mention, and search. Both sides run entirely on this machine.
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';

if (process.argv.length !== 4) {
  console.error('usage: node scripts/compare-profile-handles.mjs REFERENCE_CHECKOUT BOARD_BINARY');
  process.exit(2);
}
const {importedHandle} = await import(pathToFileURL(resolve(process.argv[2], 'packages/board/src/contract/profiles.ts')));
const exe = resolve(process.argv[3]);
const home = mkdtempSync(join(tmpdir(),'actor-oracle-'));
function run(args, data) {
  const out = spawnSync(exe,['--json',...args],{encoding:'utf8',timeout:15000,input:data && JSON.stringify(data),
    env:{PATH:process.env.PATH, HOME:home, BOARD_USER:'reader', BOARD_DB:join(home,'board.db')}});
  assert.equal(out.status,0,out.error?.message ?? out.stderr);
  return JSON.parse(out.stdout);
}
try {
  run(['sources','add','studio','local','people','profiles']);
  const names = ['Jürgen Ubl', 'Ångström Lab', 'ＦＩＸ _it 2.0', 'Mixed——case…Team', '..-_Pat._-', '🙂', 'A'.repeat(121)+'...-abc', 'நி Example'];
  for (const [i,name] of names.entries()) {
    const remote = 'U'+i;
    const expected = importedHandle('studio',name,remote);
    const m = run(['sync','import','studio','-'],{key:'import'+i,raw:{},import:{
      thread:'thread'+i,external:'msg'+i,author:remote,profile:{name},change:{op:'post',text:'Profile '+i}
    }});
    assert.equal(m.author.handle,expected);
    assert.equal(m.author.name,name);
    assert.equal(m.actor,'source:studio/'+remote);
    const mentioned = run(['post','mentions','@'+expected]).message;
    assert.equal(mentioned.text,'@['+m.actor+']');
    assert.deepEqual(run(['read','mentions','--flat','--mentions',expected]).messages.map(m=>m.id),[mentioned.id]);
  }
  console.log('Reference and Rust agree on '+names.length+' imported handles, checked through source import, mention settlement and filtered reads.');
} finally { rmSync(home,{recursive:true,force:true}); }
