const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict'),path=require('node:path');
const repo=path.join(__dirname,'..'),source=fs.readFileSync(path.join(repo,'web/messages.js'),'utf8').replace(/\r\n/g,'\n').match(/BEGIN WORKSPACE LOGIC\n([\s\S]*?)\n\/\/ END WORKSPACE LOGIC/)[1];
const nodes=new Map(),requests=[];let uid='12',fail=false,delay=false,release;
class Element {
 constructor(id=''){this.id=id;this.html='';this.rows=[];this.mounted=[];this.dataset={};this.hidden=false;this.value='';this.scrollTop=0;this.timer={hidden:false};const classes=new Set();this.classList={add:n=>classes.add(n),remove:n=>classes.delete(n),contains:n=>classes.has(n)};}
 get children(){return this.id==='chat-messages'?this.mounted:this.rows}
 get innerHTML(){return this.html}set innerHTML(value){this.html=String(value);this.rows=[]}
 get textContent(){return this.plain||''}set textContent(value){this.plain=String(value);this.html=''}
 get scrollHeight(){return this.id==='chat-scroll'?50+(node('chat-messages').children[0]?.rows.length||0)*90:500}
 get clientHeight(){return 300}
 getBoundingClientRect(){if(this.dataset.chatKey){const index=this.parent.rows.indexOf(this),top=50+index*90-node('chat-scroll').scrollTop;return{top,bottom:top+90}}return{top:0,bottom:300}}
 querySelectorAll(selector){return selector==='[data-chat-key]'?this.rows:[]}
 querySelector(selector){return selector==='[data-chat-time]'?this.timer:null}
 insertAdjacentHTML(position,html){this.html+=html;const added=[];for(const m of html.matchAll(/<article class="chat-message[^"]*" data-chat-key="([^"]+)"/g)){const row=new Element();row.dataset.chatKey=m[1];row.parent=this;added.push(row)}if(position==='afterbegin')this.rows.unshift(...added);else this.rows.push(...added)}
 appendChild(row){this.reorders=(this.reorders||0)+1;const at=this.rows.indexOf(row);if(at>=0)this.rows.splice(at,1);this.rows.push(row)}
 replaceChildren(...children){this.mounted=children}
 setAttribute(k,v){this[k]=v}
}
function node(id){if(!nodes.has(id))nodes.set(id,new Element(id));return nodes.get(id)}
const message=n=>({id:String(n),type:'private',session:'user_2',sender_uid:n%2?'2':'12',sender_name:'成员',text:'消息 '+n,timestamp:1700000000+n*60,raw:{},attachments:[]});
const context=vm.createContext({URL,URLSearchParams,setTimeout,clearTimeout,location:{href:'http://localhost/messages'},document:{getElementById:node,createElement(){return new Element()},querySelectorAll(){return[]}},fetch:async url=>{
 requests.push(url);const p=new URL(url,'http://local').searchParams;
 if(url.includes('/summary'))return{ok:true,json:async()=>({account_uid:uid,counts:{private:121,groups:1},attachments_saved:1,attachments_pending:2,sessions:[{key:'user_2',session_type:1,talker_id:'2',name:'会话二',last_message:'最近消息',last_time:1700007200},{key:'user_3',session_type:1,talker_id:'3',name:'会话三'},{key:'group_4',session_type:2,talker_id:'4',name:'粉丝团'}]})};
 const session=p.get('session'),offset=Number(p.get('offset')),empty=uid!=='12'||p.get('q')==='nothing';
 const total=empty?0:session==='user_2'?120:session?1:0;
 const items=empty?[]:session==='user_2'?Array.from({length:Math.min(50,total-offset)},(_,i)=>message(total-offset-i)):session?[{...message(1),session}]:[];
 const response=()=>fail?{ok:false,status:500}:{ok:true,json:async()=>({account_uid:uid,total,items})};
 if(delay){delay=false;return new Promise(resolve=>release=()=>resolve(response()))}return response();
}});vm.runInContext(source,context);const run=s=>vm.runInContext(s,context),flush=async()=>{for(let n=0;n<4;n++)await new Promise(r=>setImmediate(r))};
(async()=>{
 await run('ready');await run("state.type='private';state.session='user_2';load()");
 assert(node('workspace-scroll').classList.contains('chat-active'));assert.equal(node('chat-view').hidden,false);assert.equal(node('chat-title').textContent,'会话二');assert.equal(node('order').hidden,true);
 assert.equal(run('currentChat.items.length'),50);assert.equal(run('currentChat.items[0].id'),'71');assert.equal(run('currentChat.items.at(-1).id'),'120');assert.equal(node('chat-scroll').scrollTop,node('chat-scroll').scrollHeight);assert.equal(new URL(requests.at(-1),'http://local').searchParams.get('order'),'new');assert.equal(run('state.order'),'new');
 node('chat-scroll').scrollTop=140;const anchor=run('chatAnchor(currentChat)'),before=anchor.node.getBoundingClientRect().top;const identity=anchor.node;
 fail=true;await run('loadChatEarlier()');assert(node('chat-error').textContent.includes('500'));const count=requests.length;await run('loadChatEarlier(true)');assert.equal(requests.length,count);
 const beforeReorders=run('currentChat.element.reorders');
 fail=false;await node('chat-more').onclick();assert.equal(run('currentChat.items.length'),100);assert.equal(run('currentChat.items[0].id'),'21');assert.equal(identity.getBoundingClientRect().top,before,'prepend keeps the current visual anchor');assert.equal(run('chatAnchor(currentChat).node'),identity,'existing message nodes survive history loading');
 assert.equal(run('currentChat.element.reorders'),beforeReorders,'strictly older page leaves existing nodes in place');
 const cached=run('currentChat');node('chat-scroll').scrollTop=650;await run("state.session='user_3';load()");assert.equal(node('chat-title').textContent,'会话三');const beforeReturn=requests.length;await run("state.session='user_2';load()");assert.equal(run('currentChat'),cached);assert.equal(requests.length,beforeReturn);assert.equal(node('chat-scroll').scrollTop,650);
 await run('loadChatEarlier()');assert.equal(run('currentChat.items.length'),120);assert.equal(run('currentChat.done'),true);assert.equal(node('chat-more').hidden,true);const end=requests.length;await run('loadChatEarlier()');assert.equal(requests.length,end);
 context.overlap=[{...message(1),id:'tie-one'},{...message(1),id:'tie-two'},{...message(2),id:'unknown',timestamp:0}];
 run('appendChat(currentChat,overlap,120)');
 assert.deepEqual(Array.from(run('currentChat.items.slice(0,3).map(item=>item.id)')),['1','tie-one','tie-two']);
 assert.equal(run('currentChat.items.at(-1).id'),'unknown');
 context.msg={...message(10),status:1,attachments:[{url:'https://example.test/a.mp3',mime:'audio/mpeg'}]};let html=run('renderChatMessage(msg)');assert(html.includes('chat-own'));assert(html.includes('已撤回'));assert(html.includes('<audio controls'));assert(html.includes('data-raw-toggle'));
 context.msg={...message(11),session:'group_4',sender_name:'群成员 <unsafe>'};html=run('renderChatMessage(msg)');assert(html.includes('chat-sender'));assert(html.includes('群成员 &lt;unsafe&gt;'));assert(!html.includes('chat-own'));
 await run("state.q='nothing';load()");assert.equal(run('chatCaches.size'),2);assert.equal(run('currentChat'),null);assert.equal(node('chat-view').hidden,true);assert(node('items').innerHTML.includes('没有符合条件'));const search=new URL(requests.at(-1),'http://local');assert.equal(search.searchParams.get('session'),'');assert.equal(search.searchParams.get('include_groups'),'1');
 await run("state.q='';load()");assert.equal(run('currentChat'),cached);run('currentChat.done=false;currentChat.next=50');delay=true;const pending=run('loadChatEarlier()');await flush();await run("state.session='user_3';load()");release();await pending;assert.equal(node('chat-title').textContent,'会话三');assert.equal(run('currentChat.items.length'),1);
 delay=true;const switching=run("state.session='user_2';chatCaches.clear();load()");await flush();uid='99';await run("clearAccount('99')");release();await switching;assert.equal(run('chatCaches.size'),0);assert.equal(node('chat-view').hidden,true);assert.equal(node('order').hidden,false);
 const ui=fs.readFileSync(path.join(repo,'ui/app.slint'),'utf8'),integration=fs.readFileSync(path.join(repo,'src/messages_ui.rs'),'utf8'),backend=fs.readFileSync(path.join(repo,'src/messages.rs'),'utf8');
 assert(ui.includes('text: "处理附件"'));assert(ui.includes('message-attachments-pending > 0'));assert(integration.includes('Action::Attachments(uid)'));assert(/if collecting\s*&& !retry_ng\s*&& !preserve_task\s*&& shared.cancel/.test(integration));const branch=integration.slice(integration.indexOf('Action::Attachments(_) =>'),integration.indexOf('let preserve_task'));assert(branch.includes('messages::process_configured_attachments'));assert(!branch.includes('clear_task'));assert(!branch.includes('message_get'));const helper=backend.slice(backend.indexOf('pub fn process_attachments'),backend.indexOf('fn attachment_ext'));assert(!helper.includes('checkpoint_store'));assert(!helper.includes('clear_task'));assert(helper.includes('save_group'));
 console.log('PASS: read-only chat direction, recent-first loading/chronological display, anchored history prepend, retry/end, cached session DOM/scroll, independent sorting, fan clubs, empty search, stale/account isolation and standalone attachment wiring');
})().catch(error=>{console.error(error);process.exitCode=1});
