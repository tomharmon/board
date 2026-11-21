// Optional developer audit. Load pure contracts from the read-only reference; do not install them into Board.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

if (process.argv.length !== 4) {
  console.error('usage: node scripts/compare-reference.mjs REFERENCE_CHECKOUT BOARD_BINARY');
  process.exit(2);
}
const ref = resolve(process.argv[2], 'packages/board/src/contract');
const binary = resolve(process.argv[3]);
const load = file => import(pathToFileURL(join(ref, file)).href);
const { checkDefinition } = await load('data-schemas.ts');
const { findBlocks, blocksIn } = await load('blocks.ts');
const { mentionTerms } = await load('mentions.ts');
const tmp = mkdtempSync(join(tmpdir(), 'board-reference-'));
const env = { ...process.env, BOARD_DB: join(tmp, 'scratch.db'), BOARD_USER: 'auditor' };
for (const key of ['BOARD_TOKEN','BOARD_CAN','BOARD_PROFILE','BOARD_NAME']) delete env[key];
const cli = (...args) => {
  const run = spawnSync(binary, ['--json', ...args], { env, encoding: 'utf8', timeout: 10000 });
  if (run.error) throw run.error;
  return { ok: run.status === 0, json: JSON.parse(run.status === 0 ? run.stdout : run.stderr) };
};
try {
  const field = schema => ({type:'object',properties:{x:schema}});
  const object = {type:'object'};
  const definitions = [object, [], {}, {type:'array'}, {...object,required:['x']},
    field({type:'string',maxLength:3}), field({pattern:'.+'}), field({format:'date-time'}),
    field({format:'custom'}), field({multipleOf:0}), field({enum:[]}),
    field({enum:Array(530).fill(0)}), field({const:{x:[1,2]}}),
    field({oneOf:[{type:'boolean'},{type:'null'}]}), field({$ref:'https://example.invalid/x'}),
    field({const:'x\u0000y'}), {...object,additionalProperties:false}];
  for (const [i, schema] of definitions.entries()) {
    const res = cli('schemas','create',`shape${i}@1`,'--data',JSON.stringify(schema));
    assert.equal(res.ok,checkDefinition(schema) === null,`schema fixture ${i}: ${JSON.stringify(res.json)}`);
  }
  const fence = (info, body, ticks='\x60\x60\x60') => [ticks+info,body,ticks].join('\n');
  const tag='json#shape0@1';
  const blocks=['plain prose',fence(tag,'{"a":1}'),fence(tag,'{}','~~~~'),
    fence(tag,'[]'),fence(tag,'0'),fence(tag,'bad'),fence('json#@1','{}'),
    '\x60\x60\x60'+tag+'\n{}','    '+fence(tag,'bad').replaceAll('\n','\n    '),
    fence('text',fence(tag,'bad'),'\x60\x60\x60\x60'),fence(tag,'{}').split('\n').map(s=>'> '+s).join('\n'),
    '<div>\n'+fence(tag,'bad')+'\n</div>',fence(tag,'{}')+'\n'+fence(tag,'{"a":2}')];
  for(const [i,text] of blocks.entries()){
    const res=cli('post','audit/blocks',text);const expected=findBlocks(text);
    assert.equal(res.ok,expected.every(b=>!b.problem),`block fixture ${i}: ${JSON.stringify(res.json)}`);
    if(res.ok) assert.deepEqual(res.json.message.blocks??[], blocksIn(text),`block projection ${i}`);
  }
  const prose=['Hello @Ada','email ada@example.org vs @bob','Literal \x60@ada\x60',
    'inner '+fence('text','@ada'),'Code:\n'+fence('text','@ada'),'@Ada and @BOB again @ada',
    'A separate word (@bob).','Do not ping b@ada.example.org'];
  for (const [i,text] of prose.entries()) {
    const channel='audit/prose'+i;assert.equal(cli('post',channel,text).ok,true);
    const expected=mentionTerms(text);
    for(const q of ['ada','bob']){
      const page=cli('read',channel,'--flat','--mentions',q).json;
      assert.equal(page.messages.length>0,expected.includes(q),`mention fixture ${i} (${q})`);
    }
  }
  console.log(`Reference and Rust agree: ${definitions.length} schema definitions, ${blocks.length} block inputs, ${2*prose.length} mention-query results. Not the upstream wire conformance suite.`);
} finally { rmSync(tmp,{recursive:true,force:true}); }
