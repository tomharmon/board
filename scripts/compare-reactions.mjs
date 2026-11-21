// Dev-only oracle: compare reactions through the real CLI, not through a copied fixture.
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';

assert.equal(process.argv.length,4,'node scripts/compare-reactions.mjs REFERENCE_CHECKOUT BOARD_BINARY');
const contract=join(resolve(process.argv[2]),'packages/board/src/contract');
const {EMOJI_TABLE}=await import(pathToFileURL(join(contract,'emoji-table.ts')));
const {normalizeEmoji}=await import(pathToFileURL(join(contract,'emoji.ts')));
const binary=resolve(process.argv[3]);
const tmp=mkdtempSync(join(tmpdir(),'board-reactions-'));
const run=(user,...args)=>{
  const p=spawnSync(binary,['--json',...args],{encoding:'utf8',timeout:15000,env:{PATH:process.env.PATH,BOARD_DB:join(tmp,'reactions.db'),BOARD_USER:user}});
  if(p.error)throw p.error;
  assert.equal(p.status,0,p.stderr || String(p.signal));
  return JSON.parse(p.stdout);
};
try {
  const id=String(run('audit','post','compat','Reaction vocabulary').message.id);
  const spellings=new Set(['🚨','🚢',':BESPOKE:',':THUMBS_UP:',' -1 ']);
  for(const [name,{characters,aliases}] of Object.entries(EMOJI_TABLE)){
    for(const s of [name,...characters,...aliases])spellings.add(s);
    for(const s of characters)spellings.add(s.replaceAll('\uFE0F',''));
  }
  let i=0;
  for(const input of spellings) {
    const user='case'+(++i);
    const m=run(user,'react',id,input).message;
    assert.equal(m.reactions[normalizeEmoji(input)]?.includes(user),true,'mismatched reaction: '+input);
  }
  console.log('Reference and Rust agree on '+i+' reaction spellings.');
} finally {rmSync(tmp,{recursive:true,force:true});}
