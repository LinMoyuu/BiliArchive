const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const root=path.join(__dirname,'..'),read=p=>fs.readFileSync(path.join(root,p),'utf8');
function valid(s,v){if(!s||typeof s!=='object')return true;const type=x=>x==='null'?v===null:x==='array'?Array.isArray(v):x==='object'?v!==null&&!Array.isArray(v)&&typeof v==='object':x==='integer'?Number.isInteger(v):typeof v===x;
if(s.type&&!(Array.isArray(s.type)?s.type:[s.type]).some(type))return false;if('const'in s&&JSON.stringify(s.const)!==JSON.stringify(v))return false;if(s.enum&&!s.enum.some(x=>JSON.stringify(x)===JSON.stringify(v)))return false;
if(typeof v==='string'){if(s.pattern&&!new RegExp(s.pattern).test(v))return false;if(s.minLength&&v.length<s.minLength)return false;if(s.format==='date-time'&&(!/^\d{4}-\d\d-\d\dT.*(?:Z|[+-]\d\d:\d\d)$/.test(v)||!Number.isFinite(Date.parse(v))))return false;}
if(s.anyOf&&!s.anyOf.some(x=>valid(x,v)))return false;if(typeof v==='number'&&s.maximum!==undefined&&v>s.maximum)return false;
if(typeof v==='number'&&s.minimum!==undefined&&v<s.minimum)return false;if(Array.isArray(v)&&s.items&&!v.every(x=>valid(s.items,x)))return false;
if(v!==null&&typeof v==='object'&&!Array.isArray(v)){if(s.required&&!s.required.every(k=>Object.hasOwn(v,k)))return false;for(const[k,x]of Object.entries(v)){if(s.propertyNames&&!valid(s.propertyNames,k))return false;if(s.properties?.[k]){if(!valid(s.properties[k],x))return false;}else if(s.additionalProperties===false)return false;else if(s.additionalProperties&&typeof s.additionalProperties==='object'&&!valid(s.additionalProperties,x))return false;}}
if(s.allOf&&!s.allOf.every(x=>valid(x,v)))return false;if(s.if&&valid(s.if,v)&&s.then&&!valid(s.then,v))return false;return true;}

const schema=JSON.parse(read('schemas/collection-report-v1.schema.json'));
const ng=JSON.parse(read('docs/examples/collection-report-ng.json'));
assert(valid(schema,ng));
for(const edit of [d=>d.issues[0].retries=2,d=>d.issues[0].resolved=true,d=>d.status='completed',d=>d.paging_ng=-1,d=>d.issues[0].paging_key='invalid',d=>d.issues[0].pagination={reason:'invalid_paging',sequence:-1}]){
 const copy=structuredClone(ng);edit(copy);assert(!valid(schema,copy));
}
for(const reason of ['invalid_paging','non_advancing_cursor']){
 const copy=structuredClone(ng);copy.issues[0].pagination={reason,cursor_id:'18446744073709551614',cursor_time:1};assert(valid(schema,copy));
}
const recovered=structuredClone(ng);recovered.status='completed';recovered.deferred_likes=0;recovered.paging_ng=0;recovered.paging_recovered=1;recovered.paging_unresolved=0;recovered.issues[0].disposition='recovered';recovered.issues[0].resolved=true;assert(valid(schema,recovered));
const deferred=structuredClone(ng);deferred.issues[0].disposition='deferred';deferred.issues[0].retries=0;assert(valid(schema,deferred));
const messages=read('src/messages.rs'),paging=read('src/messages_paging.rs'),run=read('src/messages_run.rs'),ui=read('src/messages_ui.rs'),slint=read('ui/app.slint'),likes=read('src/messages_likes.rs');
assert(!run.includes('ImmediateRetry'));assert(!likes.includes('retried_page'));assert(!run.includes('defer_like'));
for(const source of ['Feed','List','Session'])assert(messages.includes('paging::Position::'+source));assert(likes.includes('paging::Position::Like'));
assert(messages.indexOf('集中补试')===-1); // centralized in event/report context
assert(messages.indexOf('while !cancel.load(Ordering::SeqCst)',messages.indexOf('fn crawl_run'))<messages.indexOf('处理消息附件'));
assert(messages.includes('if ctx.manual_paging'));assert(messages.includes('with_extension("ng.json")'));
assert(paging.includes('self.active.as_ref() == Some(&key) && !continuation'));assert(paging.includes('self.attempted.contains'));
assert(/registry\.library_id\s*==\s*library_id/.test(paging));assert(/data\s*\.join\("ng"\)\s*\.join\(id\)/.test(paging));assert(/saved\s*==\s*value/.test(paging));assert(paging.search(/saved\s*==\s*value/)<paging.indexOf('crate::library::backup_remove(&legacy)'));
assert(paging.includes('retries: 0'));assert(paging.includes('pub fn suspend'));
assert(ui.includes('fn snapshot_is_current'));assert(!ui.includes('w.get_account_uid()'));assert(ui.includes('request_view(&shared, &tx)'));assert(mainWorkerSource().includes('Cmd::EnsureMessageView'));function mainWorkerSource(){return read('src/main.rs')}assert(ui.includes('on_message_retry_ng'));assert(ui.includes('if !retry_ng'));assert(slint.includes('root.message-ng-count > 0'));
assert(ui.includes('messages::pause(client.next_pause(), &shared.cancel)?'));
assert(run.includes('reason != crate::bili::RetryReason::Paging'));
console.log('PASS: NG/deferred/recovered report contracts, bounded retry policy, all-source queue wiring, separate manual task, cancellation persistence, account/root isolation and unchanged request pacing');

assert(ui.includes('messages::has_resumable_task(uid, &state)'));assert(!ui.includes('messages::has_task('));assert(messages.includes('if !resumable'));assert(messages.includes('Task::fresh'));assert(paging.includes('pub(super) fn has_resumable_at'));
