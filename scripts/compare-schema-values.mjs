// Development check only. Exercise the installed Rust binary against the reference's pure value oracle.
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {mkdtempSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';

assert.equal(process.argv.length, 4, 'node scripts/compare-schema-values.mjs REFERENCE_CHECKOUT BOARD_BINARY');
const source = join(resolve(process.argv[2]), 'packages/board/src/contract/data-values.ts');
const {checkValue} = await import(pathToFileURL(source));
const binary = resolve(process.argv[3]);
const tmp = mkdtempSync(join(tmpdir(), 'board-values-'));
const run = (...args) => {
  const p = spawnSync(binary, ['--json', ...args], {timeout:15000, encoding:'utf8',
    env:{PATH:process.env.PATH, BOARD_DB:join(tmp,'values.sqlite'), BOARD_USER:'audit'}});
  if (p.error) throw p.error;
  return {ok:p.status===0, output:p.stderr || p.stdout};
};
const samples = {
  date: ['2000-02-29','1900-02-29','2024-02-30','0000-02-29','2024-1-01','2024-01-00'],
  time: ['22:15:00','23:59:60','00:30:60Z','13:00:00+88:99','12:00:00+12','12:00:00-2230','24:00:00','07:08:09.012Z','7:08:09'],
  'date-time': ['2000-02-29t23:59:60Z','2000-02-29 12:00:00','2000-02-29\t12:00:00+99','1900-02-29T12:00:00Z','2013-12-01T14:58:12-2412','2025-01-01'],
  email: ['a@example.org','a@localhost','name+tag@test.local','"name"@example.org','a..b@example.org','hi@-example.org','one@two.3'],
  hostname: ['x','a.b.','a_b','example.org/','label.'.repeat(50)+'x','z'.repeat(64)+'.org','z'.repeat(63)+'.org'],
  ipv4: ['127.0.0.1','001.002.003.004','192.168.00.255','255.255.255.256','+1.2.3.4','1.2.3','192.0168.1.1'],
  ipv6: ['::1','2001:db8::1','::ffff:192.168.1.1','::ffff:192.168.01.1','2001::0::1','fe80::1%eth0','2001:db8:0:0:0:0:0:1'],
  uuid:['00000000-0000-0000-0000-000000000000','urn:uuid:aabbccdd-1234-1234-1234-aabbccddeeff','URN:UUID:AABBCCDD-1234-1234-1234-AABBCCDDEEFF','{aabbccdd-1234-1234-1234-aabbccddeeff}','aabbccdd123412341234aabbccddeeff'],
  uri: ['https://example.invalid/a?x=1#f','file:///tmp/example','mailto:ada@example.invalid','#anchor','/path','http://[::1]:81','https://bad space','http://abc/%xx','urn:example:thing','http:'],
};
const cases = Object.entries(samples).map(([key, values]) => [{type:'string',format:key},values]);
cases.push([{multipleOf:0.1},[0,0.1,0.2,0.3,0.6,-0.3,4.5,4.51]],[{enum:[1,{},[0]]},[1,1.1,{},[0],{k:0},null]],[{oneOf:[{type:'integer'},{minimum:2}]},[1,2,1.5,'x',null]]);
let count = 0;
const differences = [];
try {
  for (const [i,[field, values]] of cases.entries()) {
    const schema = {type:'object',properties:{x:field},required:['x']};
    const name='value'+i+'@1';
    const reg=run('schemas','create',name,'--data',JSON.stringify(schema));
    assert.equal(reg.ok,true,reg.output);
    for(const x of values) {
      const data={x};
      const found=run('post','values','--schema',name,JSON.stringify(data));
      const expected=checkValue(schema,data)===null;
      if(found.ok!==expected) differences.push({field,x,expected,got:found.ok});
      count++;
    }
  }
  assert.deepEqual(differences,[],JSON.stringify(differences));
  console.log('Reference and Rust agree on '+count+' selected schema values (not an exhaustive validator proof).');
} finally { rmSync(tmp,{recursive:true,force:true}); }
