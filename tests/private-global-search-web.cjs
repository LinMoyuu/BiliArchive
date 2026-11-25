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
 appendChild(row){const at=this.rows.indexOf(row);if(at>=0)this.rows.splice(at,1);this.rows.push(row)}
 replaceChildren(...children){this.mounted=children}
 setAttribute(k,v){this[k]=v}
}
function node(id){if(!nodes.has(id))nodes.set(id,new Element(id));return nodes.get(id)}
const message=n=>({id:String(n),type:'private',session:'user_2',sender_uid:n%2?'2':'12',sender_name:'成员',text:'消息 '+n,timestamp:1700000000+n*60,raw:{},attachments:[]});
const context=vm.createContext({URL,URLSearchParams,setTimeout,clearTimeout,location:{href:'http://localhost/messages'},document:{getElementById:node,createElement(){return new Element()},querySelectorAll(){return[]}},fetch:async url=>{
 requests.push(url);const p=new URL(url,'http://local').searchParams;
 if(url.includes('/summary'))return{ok:true,json:async()=>({account_uid:uid,counts:{private:121,groups:1},attachments_saved:1,attachments_pending:2,sessions:[{key:'user_2',session_type:1,talker_id:'2',name:'会话二',last_message:'最近消息',last_time:1700007200},{key:'user_3',session_type:1,talker_id:'3',name:'会话三'},{key:'group_4',session_type:2,talker_id:'4',name:'粉丝团'}]})};
 const session=p.get('session'),offset=Number(p.get('offset')),search=p.get('q'),empty=uid!=='12'||search==='nothing';
 const matches=[{...message(201),session:'user_3',text:'跨会话 needle'},{...message(202),session:'group_4',text:'粉丝团 needle'}];
 const total=empty?0:search==='needle'?2:session==='user_2'?120:session?1:0;
 const items=empty?[]:search==='needle'?matches:session==='user_2'?Array.from({length:Math.min(50,total-offset)},(_,i)=>message(total-offset-i)):session?[{...message(1),session}]:[];
 const response=()=>fail?{ok:false,status:500}:{ok:true,json:async()=>({account_uid:uid,total,items})};
 if(delay){delay=false;return new Promise(resolve=>release=()=>resolve(response()))}return response();
}});vm.runInContext(source,context);const run=s=>vm.runInContext(s,context),flush=async()=>{for(let n=0;n<4;n++)await new Promise(r=>setImmediate(r))};
(async()=>{
 await run('ready');await run("state.type='private';state.session='user_2';load()");const original=run('currentChat');node('chat-scroll').scrollTop=640;
 await run("state.q='needle';load()");let query=new URL(requests.at(-1),'http://local').searchParams;assert.equal(query.get('session'),'');assert.equal(query.get('include_groups'),'1');assert.equal(query.get('type'),'private');assert.equal(run('state.session'),'user_2');assert.equal(run('currentChat'),null);assert(node('items').innerHTML.includes('跨会话 needle'));assert(node('items').innerHTML.includes('粉丝团 needle'));assert(node('items').innerHTML.includes('会话三'));assert(node('page').textContent.includes('全部会话搜索'));
 await run("state.q='';load()");assert.equal(run('currentChat'),original);assert.equal(node('chat-scroll').scrollTop,640);assert.equal(node('order').hidden,true);await run("state.q='   ';load()");assert.equal(run('currentChat'),original);assert.equal(node('chat-scroll').scrollTop,640);
 await run("state.session='group_4';load()");await run("state.q='needle';load()");query=new URL(requests.at(-1),'http://local').searchParams;assert.equal(query.get('type'),'private');assert.equal(query.get('session'),'');assert.equal(query.get('include_groups'),'1');
 await run("state.q='nothing';load()");assert(node('items').innerHTML.includes('没有符合条件'));assert.equal(run('state.session'),'group_4');
 const source=fs.readFileSync(path.join(repo,'web/messages.js'),'utf8');assert(source.includes("state.q='';$('q').value='';clearTimeout(timer)"));
 console.log('PASS: private search spans other conversations and fan clubs, ignores selected group/user scope, retains source labels, restores selected chat/scroll and handles no results');
})().catch(error=>{console.error(error);process.exitCode=1});
