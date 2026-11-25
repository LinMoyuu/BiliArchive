const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict'),path=require('node:path');
const source=fs.readFileSync(path.join(__dirname,'../web/messages.js'),'utf8').replace(/\r\n/g,'\n').match(/BEGIN WORKSPACE LOGIC\n([\s\S]*?)\n\/\/ END WORKSPACE LOGIC/)[1];
const nodes=new Map(),requests=[],observers=[];let fail=false,empty=false,defer=false,pending=[],active=0,peak=0;
const node=id=>{if(!nodes.has(id))nodes.set(id,{value:'',innerHTML:'',textContent:'',isConnected:true,querySelectorAll(){return[]},insertAdjacentHTML(_,html){this.innerHTML+=html}});return nodes.get(id)};
class Observer{constructor(cb,options){this.cb=cb;this.options=options;this.targets=new Set();observers.push(this)}observe(n){this.targets.add(n)}unobserve(n){this.targets.delete(n)}disconnect(){this.targets.clear()}fire(n){if(this.targets.has(n))this.cb([{target:n,isIntersecting:true}])}}
const item=id=>({id:String(id),type:'private',text:'row '+id,raw:{}});
const context=vm.createContext({console,URL,URLSearchParams,setTimeout,clearTimeout,IntersectionObserver:Observer,location:{href:'http://local/messages'},document:{getElementById:node,querySelectorAll(){return[]}},fetch:async (url,{signal}={})=>{
 requests.push(url);if(url.includes('/summary'))return{ok:true,json:async()=>({account_uid:'1',counts:{},sessions:[],attachments_saved:0,attachments_pending:0})};
 ++active;peak=Math.max(peak,active);const offset=Number(new URL(url,'http://local').searchParams.get('offset'));
 const response=()=>{--active;if(fail){fail=false;return{ok:false,status:503}}return{ok:true,json:async()=>({account_uid:'1',total:150,items:empty?[]:offset===0?Array.from({length:50},(_,i)=>item(i)):Array.from({length:50},(_,i)=>item(offset+i-1))})}};
 if(defer)return new Promise((resolve,reject)=>{
  const release=()=>resolve(response());
  pending.push(release);
  signal?.addEventListener('abort',()=>{
   pending.splice(pending.indexOf(release),1);--active;
   reject(Object.assign(new Error('Aborted'),{name:'AbortError'}));
  },{once:true});
 });return response();
}});
vm.runInContext(source,context);const run=code=>vm.runInContext(code,context);const flush=async()=>{for(let i=0;i<5;i++)await new Promise(r=>setImmediate(r))};
(async()=>{
 await run('ready');assert.equal(run('stream.loaded'),50);assert.equal(node('more').hidden,false);assert.equal(observers[0].options.rootMargin,'0px 0px 240px 0px');
 const first=node('items').innerHTML;observers[0].fire(node('sentinel'));observers[0].fire(node('sentinel'));await flush();
 assert.equal(requests.filter(u=>u.includes('offset=50')).length,1);assert(node('items').innerHTML.startsWith(first));assert.equal(run('stream.loaded'),99);assert.equal(run('stream.next'),100,'advance raw offset even when one row repeats');
 fail=true;observers[0].fire(node('sentinel'));await flush();assert(node('error').textContent.includes('503'));assert.equal(node('more').textContent,'重试加载');const failedCount=requests.length;
 observers[0].fire(node('sentinel'));await flush();assert.equal(requests.length,failedCount,'failed load stops automatic retry');
 await node('more').onclick();assert.equal(run('stream.done'),true);assert.equal(node('more').hidden,true);assert.equal(run('stream.loaded'),149);
 await run('load()');run('workspaceVisible=false;pageObserver.disconnect()');const hiddenCount=requests.length;await run('loadMore(true)');assert.equal(requests.length,hiddenCount);
 run('workspaceVisible=true;resumePages()');observers[0].fire(node('sentinel'));await flush();assert.equal(run('stream.next'),100);
 empty=true;await run('load()');assert.equal(run('stream.done'),true);assert.equal(run('stream.loaded'),0);assert(node('items').innerHTML.includes('没有符合条件'));empty=false;
 // A reset invalidates an in-flight result and prevents old queued requests from being sent.
 defer=true;const stale=run('load()');await flush();const fresh=run("state.q='fresh';load()");await flush();assert.equal(pending.length,2);pending.shift()();await stale;assert.equal(node('items').innerHTML,'');pending.shift()();await fresh;assert.equal(run('stream.loaded'),50);
 // Paging work from outer lists and expanded groups shares this same two-slot FIFO queue.
 peak=0;run("globalThis.queueResults=[0,1,2,3].map(n=>pageRequest('/queue?offset='+n,()=>true,true))");await flush();assert.equal(pending.length,2);assert.equal(peak,2);
 run('workspaceVisible=false');pending.shift()();pending.shift()();await flush();assert.equal(pending.length,0,'hidden workspace keeps automatic work queued');
 run('workspaceVisible=true;resumePages()');await flush();assert.equal(pending.length,2);assert(requests.at(-2).includes('offset=2'));assert(requests.at(-1).includes('offset=3'));pending.shift()();pending.shift()();await run('Promise.all(queueResults)');await flush();assert.equal(peak,2);
 run("globalThis.validTask=true;globalThis.hold=[0,1].map(n=>pageRequest('/hold?offset='+n,()=>true));globalThis.cancelled=pageRequest('/cancelled',()=>validTask,true)");await flush();run('validTask=false;pumpPages()');assert.equal(await run('cancelled'),null);assert(!requests.includes('/cancelled'));pending.shift()();pending.shift()();await run('Promise.all(hold)');
 // Switching categories cancels an already-running private list request.
 context.AbortController=AbortController;
 defer=true;
 const privatePage=run("switchCategory('private')");
 await flush();assert.equal(pending.length,1);
 const summaries=requests.filter(url=>url.endsWith('/summary')).length;
 const target=run("switchCategory('likes')");
 await flush();await privatePage;
 assert.equal(pending.length,1,'obsolete request releases its slot');
 assert.equal(new URL(requests.at(-1),'http://local').searchParams.get('type'),'likes');
 assert.equal(requests.filter(url=>url.endsWith('/summary')).length,summaries);
 pending.shift()();await target;
 defer=false;await run("switchCategory('replies')");
 const requestsBeforeCache=requests.length;
 await run("switchCategory('likes')");
 assert.equal(requests.length,requestsBeforeCache,'returning to a category reuses its recent first page');
 run('firstPages.forEach(entry=>{entry.expires=0;})');
 await run("switchCategory('mentions')");
 await run("switchCategory('likes')");
 assert.equal(requests.length,requestsBeforeCache+2,'an expired first page is fetched again');
 // Toolbar replaces duplicate header refresh and page-switch controls.
 const html=fs.readFileSync(path.join(__dirname,'../web/index.html'),'utf8');assert(!html.includes('id="message-refresh"'));assert(!html.includes('id="m-prev"'));assert(html.includes('class="message-toolbar-right"><div id="m-status"'));assert(html.includes('id="m-more"'));
 console.log('PASS: automatic append, retained content, duplicate offsets, terminal/empty pages, manual retry, hidden pause/resume, stale isolation, two-request FIFO limit, cancelled queued work and toolbar structure');
})().catch(e=>{console.error(e);process.exitCode=1});
