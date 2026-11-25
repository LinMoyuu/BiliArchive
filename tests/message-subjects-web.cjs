const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict'),path=require('node:path');
const source=fs.readFileSync(path.join(__dirname,'../web/messages.js'),'utf8').replace(/\r\n/g,'\n').match(/BEGIN WORKSPACE LOGIC\n([\s\S]*?)\n\/\/ END WORKSPACE LOGIC/)[1];
const nodes=new Map(),slots=[],requests=[];let active=0,peak=0,pending=[],hold=false,fail=false;
const node=id=>{if(!nodes.has(id))nodes.set(id,{value:'',innerHTML:'',textContent:'',querySelectorAll(selector){return id==='items'&&selector==='[data-subject-key]'?slots:[]},insertAdjacentHTML(_,html){this.innerHTML+=html}});return nodes.get(id)};
const video={key:'video:aid:170001',type:'video',id:'170001',title:'Title <unsafe>',text:'',bvid:'BV17x411w7KC',url:'https://www.bilibili.com/video/BV17x411w7KC',error:''};
const dynamic={key:'dynamic:18446744073709551614',type:'dynamic',id:'18446744073709551614',title:'',text:'dynamic topic <script>'+(' long text'.repeat(30)),bvid:'',url:'https://t.bilibili.com/18446744073709551614',error:''};
const fixture={type:'replies',id:'1',sender_uid:'3',sender_name:'user',text:'old combined content',timestamp:1,raw:{item:{type:'video',source_content:'received reply',target_reply_content:'original comment'}},subject:video};
const ctx=vm.createContext({URL,URLSearchParams,setTimeout,clearTimeout,location:{href:'http://localhost/messages'},document:{getElementById:node,querySelectorAll(){return[]}},fetch:async url=>{
 if(!url.includes('/subjects?'))return new Promise(()=>{});
 requests.push(url);++active;peak=Math.max(peak,active);const params=new URL(url,'http://local').searchParams,uid=params.get('account_uid'),key=params.get('keys');
 const respond=()=>{--active;if(fail){fail=false;return{ok:false,status:500}}return{ok:true,json:async()=>({account_uid:uid,subjects:[{...video,key,title:'fetched title',error:''}]})}};
 if(hold)return new Promise(resolve=>pending.push(()=>resolve(respond())));return respond();
}});vm.runInContext(source,ctx);const run=code=>vm.runInContext(code,ctx);const flush=async()=>{for(let i=0;i<5;i++)await new Promise(r=>setImmediate(r))};
function render(value){ctx.fixture=value;return run('renderReply(fixture)')}
function slot(subject,mode='body'){
 const n={dataset:{subjectKey:subject.key,subjectMode:mode},detail:{open:false},button:{},innerHTML:'',querySelector(selector){return selector==='details'?this.detail:selector==='[data-subject-retry]'&&this.innerHTML.includes('data-subject-retry')?this.button:null}};
 slots.push(n);return n;
}
(async()=>{
 let html=render(fixture),heading=html.match(/class="reply-heading">([\s\S]*?)<\/div>/)[1];assert(heading.indexOf('对我的视频发表了评论')<heading.indexOf('Title &lt;unsafe&gt;'));assert(heading.includes('BV17x411w7KC'));assert(heading.includes('noopener noreferrer'));assert(heading.includes('no-referrer'));assert(html.includes('class="reply-body">received reply'));
 html=render({...fixture,subject:dynamic,raw:{item:{type:'reply',source_content:'received reply',target_reply_content:'original comment',root_reply_content:'root comment'}}});assert(html.includes('动态主题'));assert(html.includes('dynamic topic &lt;script&gt;'));assert(html.includes('original comment'));assert(!html.includes('class="reply-body">dynamic'));assert(html.includes('<details class="reply-subject">'));assert(!html.includes('<details class="reply-subject" open'));
 const dynamicSlot=slot(dynamic);dynamicSlot.detail.open=true;node('items').innerHTML='retained message DOM';run('refreshSubjectNodes()');assert.equal(dynamicSlot.detail.open,true);assert.equal(node('items').innerHTML,'retained message DOM');
 run("resetSubjects();state.uid='12'");slots.length=0;const incomplete={...video,title:''};ctx.incomplete=incomplete;run('rememberSubject(incomplete)');const v=slot(incomplete,'heading');hold=true;
 run('hydrateSubjects();hydrateSubjects()');await flush();assert.equal(requests.length,1,'same subject shares one request');assert.equal(active,1);pending.shift()();await flush();assert(v.innerHTML.includes('fetched title'));assert.equal(node('items').innerHTML,'retained message DOM');run('hydrateSubjects()');await flush();assert.equal(requests.length,1,'completed result is reused');
 run('resetSubjects()');slots.length=0;for(let n=1;n<=4;n++){ctx.subject={...incomplete,key:'video:aid:'+n,id:String(n)};run('rememberSubject(subject)');slot(ctx.subject,'heading');}
 peak=0;run('hydrateSubjects()');await flush();assert.equal(pending.length,2);assert.equal(peak,2);pending.shift()();await flush();assert.equal(pending.length,2);pending.shift()();await flush();while(pending.length){pending.shift()();await flush()}assert.equal(peak,2);
 run('resetSubjects()');slots.length=0;run('rememberSubject(incomplete)');const failed=slot(incomplete,'heading');fail=true;hold=false;run('hydrateSubjects()');await flush();assert(failed.innerHTML.includes('data-subject-retry'));const before=requests.length;run('hydrateSubjects()');await flush();assert.equal(requests.length,before,'failure must wait for manual retry');let prevented=0;failed.button.onclick({preventDefault(){prevented++},stopPropagation(){prevented++}});await flush();assert.equal(prevented,2);assert.equal(new URL(requests.at(-1),'http://local').searchParams.get('retry'),'1');assert(failed.innerHTML.includes('fetched title'));
 run('resetSubjects()');slots.length=0;run('rememberSubject(incomplete)');slot(incomplete,'heading');hold=true;run('hydrateSubjects()');await flush();run("resetSubjects();state.uid='99'");slots.length=0;pending.shift()();await flush();assert.equal(run('subjectData.size'),0,'old-account results must not restore subject data');
 run("state.uid='12';rememberSubject(incomplete);workspaceVisible=false");slot(incomplete,'heading');const hiddenCount=requests.length;run('hydrateSubjects()');await flush();assert.equal(requests.length,hiddenCount);run('workspaceVisible=true;pumpSubjects()');await flush();assert.equal(requests.length,hiddenCount+1);pending.shift()();await flush();
 assert(fs.readFileSync(path.join(__dirname,'../web/index.html'),'utf8').includes('>按主题</button>'));
 console.log('PASS: video title/BV, dynamic context, original comment separation, safe links, retained disclosure/DOM, deduplicated requests, cache reuse, two-topic limit, manual retry, stale-account isolation and hidden pause');
})().catch(error=>{console.error(error);process.exitCode=1});
