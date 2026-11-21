// Development only. Run equivalent public operations against two independent, disposable SQLite boards.
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {mkdtempSync,readFileSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
if(process.argv.length!==4) throw Error('usage: node scripts/compare-workflows.mjs ORIGINAL_PACKAGE_WITH_DEPS BOARD_BINARY');
const pkg=resolve(process.argv[2]), bin=resolve(process.argv[3]), dir=mkdtempSync(join(tmpdir(),'board-behavior-'));
const load=file=>import(pathToFileURL(join(pkg,'src',file)).href);
const {createBoard}=await load('server/board.ts'),{local}=await load('backend-local/index.ts');
const options={serving:null,sandbox:null,backend:()=>{throw Error('unexpected external backend')}};
const author={id:'desk:U1',handle:'ana',name:'Ana',avatar:null,isBot:false};
const cc={...author,id:'desk:U2',handle:'vic',name:'Vic'};
const bytes=Uint8Array.from([0,2,128,255]);
const def={type:'object',properties:{ok:{type:'boolean'}},required:['ok']};
const orig=()=>{
  const db=local({dir:join(dir,'original')}),b=createBoard({backend:db},options);
  return {
    post:(ch,t,id)=>b.post(ch,{text:t,id,noSync:true}),posted:id=>b.posted(id),
    reply:(id,t,after)=>b.reply(id,{text:t,after}),read:id=>b.thread(id),
    edit:(id,t,r)=>b.edit(id,t,{revision:r}),del:(id,r)=>b.delete(id,{revision:r}),
    react:id=>b.react(id,'eyes'),label:(id,v,expected)=>b.label(id,{set:{phase:v},if:{phase:expected}}),
    wait:async id=>(await b.wait({re:id,after:id},{timeout:'2s'})).message.id,
    search:async ch=>(await b.threads({channel:ch,text:'needle'})).threads.map(v=>v.message.text),
    schema:async schema=>(await b.createSchema({name:'auditrecord',version:1,description:'audit',schema})).created,
    file:async()=>{const f=await b.upload({data:bytes,name:'payload.bin'});const p=await b.post('audit/files',{text:'file',attachments:[f]});return Buffer.from(await(await b.download(p.attachments[0].id)).arrayBuffer()).toString('hex');},
    source:()=>b.register({watch:{},sync:[{key:'desk',platform:'desk'}]}),
    import:async(key,ext='root')=>(await b.syncImport({op:'post',channel:'audit/external',from:'desk',text:'for @[desk:U2]',author,mentioned:[cc],id:key,origin:{channel:'desk:in',thread:'desk:A',subthread:null,message:ext,context:null}})).message,
    find:async()=>(await b.syncFind({thread:'desk:A',message:'root'})).message,
    hasMentioned:async()=>(await b.actors()).some(a=>a.name==='Vic'),
    push:(key,msg)=>b.inboxPush('desk',[{id:key,event:{kind:'message',channel:'desk:in',thread:'desk:B',subthread:null,message:msg,isRoot:msg==='1',text:'queued',context:null,author},raw:{raw:msg}}]),
    next:async()=>(await b.inboxNext('desk',{limit:10}))[0],
    ack:l=>b.inboxAck('desk',l.id,{lease:l.lease}),
    task:async(t,needs)=>(await b.tasks.create('audit/jobs',{text:t,needs})).task.id,
    needs:(id,other)=>b.tasks.needs(id,[other]),claim:id=>b.tasks.claim(id),
    complete:claim=>claim.complete('result'),taskState:async id=>(await b.thread(id)).message.task,
    close:()=>db.close()
  };
};
const env={...process.env,BOARD_DB:join(dir,'rust.db'),BOARD_USER:'auditor',XDG_STATE_HOME:dir,XDG_CACHE_HOME:dir,HOME:dir};
for(const k of Object.keys(env)) if(k.startsWith('BOARD_')&&!['BOARD_DB','BOARD_USER'].includes(k))delete env[k];
function cli(...args){const r=spawnSync(bin,['--json',...args.filter(x=>x!==undefined).map(String)],{env,encoding:'utf8',timeout:8000});if(r.error)throw r.error;if(r.status!==0)throw Error(r.stderr);return JSON.parse(r.stdout);}
function dataCli(obj,...args){const r=spawnSync(bin,['--json',...args,'-'],{env,input:JSON.stringify(obj),encoding:'utf8',timeout:8000});if(r.error)throw r.error;if(r.status!==0)throw Error(r.stderr);return JSON.parse(r.stdout);}
const rust=()=>({
  post:(ch,t,id)=>cli('post',ch,t,'--no-sync','--id',id).message,posted:id=>cli('posted',id).message,
  reply:(id,t,after)=>cli('reply',id,t,...after===undefined?[]:['--after',after]).message,
  read:id=>cli('read',id), edit:(id,t,r)=>cli('edit',id,t,'--if-revision',r),
  del:(id,r)=>cli('delete',id,'--if-revision',r),react:id=>cli('react',id,'eyes'),
  label:(id,v,expected)=>cli('label',id,'phase='+v,...expected===null?['--if-unset','phase']:['--if','phase='+expected]),
  wait:id=>cli('wait',id,'--timeout','2s').event.message.id,
  search:ch=>cli('read',ch,'--text','needle').threads.map(v=>v.message.text),
  schema:schema=>cli('schemas','create','auditrecord@1','--description','audit','--data',JSON.stringify(schema)).created,
  file:()=>{const src=join(dir,'payload.bin'),dest=join(dir,'downloaded.bin');writeFileSync(src,bytes);const f=cli('upload','audit/files',src).attachment;
    const p=cli('post','audit/files','file','--attachment-id',f.key).message;cli('download',p.attachments[0].id,dest);return readFileSync(dest).toString('hex');},
  source:()=>cli('sources','add','desk','local','desk:in','audit/external'),
  import:(key,external='root')=>dataCli({key,import:{thread:'desk:A',external,author:'U1',profile:{name:'Ana'},mentioned:{U2:{name:'Vic'}},change:{op:'post',text:'for @[source:desk/U2]'}}},'sync','import','desk').id,
  find:()=>cli('sync','find','desk','desk:A','root').id,
  hasMentioned:()=>cli('actors','--details','--query','Vic').actors.some(a=>a.name==='Vic'),
  push:(key,external)=>dataCli({key,raw:{raw:external},import:{thread:'desk:B',external,author:'U1',parent:external==='1'?null:'1',change:{op:'post',text:'queued'}}},'inbox','push','desk'),
  next:()=>{let q=cli('inbox','next','desk');return q?{...q,raw:q.job.raw}:null;},ack:l=>cli('inbox','ack','desk',l.job.id,'--receipt',l.receipt),
  task:(t,needs)=>cli('tasks','create','audit/jobs',t,...needs.flatMap(n=>['--needs',n])).id,
  needs:(id,other)=>cli('tasks','needs',id,other),claim:id=>cli('tasks','claim',id),
  complete:c=>cli('tasks','complete',c.id,'result','--epoch',c.epoch,'--token',c.claim_token),
  taskState:id=>cli('read',id).message.task,close:()=>{}
});
async function transcript(open){
  let b=open(); const trace={};
  const denied=async f=>{try{await f();return false;}catch{return true;}};
  try{
    const root=await b.post('audit/threads','root needle','root-key'),id=root.id;
    trace.replay=(await b.post('audit/threads','root needle','root-key')).id===id;
    trace.recovery=(await b.posted('root-key')).id===id;
    trace.localOnly=(root.noSync??root.no_sync)===true;
    const r=await b.reply(id,'first',id),nested=await b.reply(r.id,'nested needle',r.id);
    trace.exactParent=(nested.replyTo??nested.reply_to)===r.id&&nested.re===id;
    trace.newestGuard=await denied(()=>b.reply(id,'stale',r.id));
    trace.waitAfterReply=await b.wait(id)===r.id;
    let before=(await b.read(id)).message;
    await b.react(id);await b.label(id,'open',null);
    trace.labelGuard=await denied(()=>b.label(id,'closed','wrong'));
    let after=(await b.read(id)).message;
    trace.nonTextRevision=after.revision===before.revision&&after.version>before.version;
    trace.reacted=after.reactions.eyes.length===1;trace.phase=after.labels.phase;
    await b.edit(id,'updated needle',before.revision);
    trace.staleEdit=await denied(()=>b.edit(id,'lost',before.revision));
    trace.search=(await b.search('audit/threads')).sort();
    await b.del(nested.id,nested.revision);
    trace.deletedCountsForNewest=await denied(()=>b.reply(id,'late',r.id));
    let thread=await b.read(id);trace.tombstone=thread.replies.find(m=>m.id===nested.id).deleted;
    const one=await b.task('first task',[]),two=await b.task('second task',[one]);
    trace.dependencyWait=(await b.taskState(two)).state;
    trace.cycle=await denied(()=>b.needs(one,two));
    let claim=await b.claim(one);trace.competingClaim=await denied(()=>b.claim(one));
    await b.complete(claim);trace.fenced=await denied(()=>b.complete(claim));
    trace.done=(await b.taskState(one)).state;trace.dependencyReleased=(await b.taskState(two)).state;
    trace.schemaNew=await b.schema(def);trace.schemaReplay=await b.schema(def);trace.schemaImmutable=await denied(()=>b.schema({type:'object'}));
    trace.fileBytes=await b.file();
    await b.source();const imported=await b.import('remote-key');trace.sourceLookup=await b.find()===imported;trace.sourceReplay=await b.import('remote-key')===imported;
    const external=(await b.read(imported)).message;trace.externalName=external.author.name;trace.sourceName=external.from;trace.mentionedProfile=await b.hasMentioned();
    trace.secondRoot=await denied(()=>b.import('other-root','other'));
    let max='a'.repeat(300);await b.push(max,'1');await b.push(max,'1');await b.push('delivery-two','2');trace.overlongDeliveryDenied=await denied(()=>b.push(max+'a','3'));
    const first=await b.next();trace.rawReceipt=first.raw;trace.inboxFenced=!await b.next();await b.ack(first);
    const last=await b.next();trace.secondReceipt=last.raw;await b.ack(last);trace.inboxDrained=!await b.next();
    await b.close();b=open();
    thread=await b.read(id);trace.restart=[thread.message.text,thread.replies.length,thread.replies.find(m=>m.id===nested.id).deleted,(await b.taskState(one)).result];
    return trace;
  }finally{await b.close();}
}
try{
  const expected=await transcript(orig),got=await transcript(rust);
  assert.deepEqual(got,expected);
  for(const check of ['replay','recovery','localOnly','exactParent','newestGuard','waitAfterReply','labelGuard','nonTextRevision','reacted','staleEdit','deletedCountsForNewest','tombstone','cycle','competingClaim','fenced','schemaNew','schemaImmutable','sourceLookup','sourceReplay','mentionedProfile','secondRoot','overlongDeliveryDenied','inboxFenced','inboxDrained'])assert.equal(got[check],true,check);
  assert.equal(got.fileBytes,'000280ff');
  assert.deepEqual(got.restart,['updated needle',2,true,'result']);
  console.log('Original SQLite backend and Rust agree on '+Object.keys(got).length+' outcomes:\n'+JSON.stringify(got,null,2));
}finally{rmSync(dir,{recursive:true,force:true});}
