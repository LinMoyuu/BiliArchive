const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict');
const root=require('node:path').join(__dirname,'..');
const script=fs.readFileSync(root+'/web/messages.js','utf8').replace(/\r\n/g,'\n').match(/BEGIN WORKSPACE LOGIC\n([\s\S]*?)\n\/\/ END WORKSPACE LOGIC/)[1];
const nodes=new Map();function node(id){if(!nodes.has(id))nodes.set(id,{innerHTML:'',scrollTop:0,scrollHeight:1000,clientHeight:500,classList:{add(){},remove(){}},getBoundingClientRect(){return{top:0,bottom:500}},replaceChildren(...children){this.children=children;this.innerHTML=children.map(n=>n.innerHTML).join('')},insertAdjacentHTML(_,html){this.innerHTML+=html},textContent:'',value:'',querySelectorAll(){return []},querySelector(selector){return node(id+selector)}});return nodes.get(id)}
const buttons=['replies','mentions','likes','private'].map(type=>({dataset:{type},attributes:{},setAttribute(k,v){this.attributes[k]=v}}));
const fixture={id:'18446744073709551614',type:'private',session:'user_2',sender_uid:'12',sender_name:'<script>alert(1)</script>',text:'hello <img src=x onerror=alert(1)>',timestamp:1700000000,status:1,raw:{msg_key:'18446744073709551614'},attachments:[{id:'a'.repeat(32),url:'https://example.org/a.png',path:'attachments/a.png',error:''}]};
const requests=[];let account='12';let fail=false;
const ctx=vm.createContext({console,URL,URLSearchParams,location:{href:'http://127.0.0.1:9000/messages'},setTimeout,clearTimeout,document:{createElement(){return node('element-'+Math.random())},getElementById:node,querySelectorAll(selector){return selector==='#types button'?buttons:[]}},fetch:async url=>{
requests.push(url);if(fail)return{ok:false,status:500};
if(url.includes('/api/messages/likes?'))return{ok:true,json:async()=>({account_uid:account,notice:'通知报告 100 个赞 / 已归档 51 位用户 · 接口已遍历；明细数与通知计数不一致',total:51,items:[{uid:'18446744073709551614',nickname:'<script>like user</script>',like_time:100,raw:{}}]})};
return{ok:true,json:async()=>url.includes('/summary')?{account_uid:account,counts:{private:1,groups:2,likes:0,replies:0,mentions:0},sessions:[{key:'user_2',name:'someone',timestamp:100,session_type:1,talker_id:'2'},{key:'group_3',name:'fan group',timestamp:101,session_type:2,talker_id:'3'}],attachments_saved:1,attachments_pending:0}:{account_uid:account,total:1,items:[fixture]}};
}});
vm.runInContext(script,ctx);
(async()=>{
 await vm.runInContext('ready',ctx);
 await buttons.find(b=>b.dataset.type==='likes').onclick();
 assert.equal(node('order').value,'new');
 for(const text of ['时间倒序','时间正序','点赞最多','点赞最少'])assert(node('order').innerHTML.includes(text));
 const last=()=>new URL(requests.filter(u=>u.includes('/api/messages/items?')).at(-1),'http://local');
 for(const order of ['likes_desc','likes_asc','old','new']){
  vm.runInContext("state.q='keep query';state.offset=150",ctx);
  node('order').value=order;await node('order').onchange();
  assert.equal(last().searchParams.get('order'),order);
  assert.equal(last().searchParams.get('offset'),'0');
  assert.equal(last().searchParams.get('q'),'keep query');
  assert.equal(vm.runInContext('state.order',ctx),'new');
 }
 node('order').value='likes_desc';await node('order').onchange();
 await buttons.find(b=>b.dataset.type==='replies').onclick();
 assert(!node('order').innerHTML.includes('点赞最多'));
 node('order').value='old';await node('order').onchange();
 assert.equal(last().searchParams.get('order'),'old');
 await buttons.find(b=>b.dataset.type==='likes').onclick();
 assert.equal(node('order').value,'likes_desc');
 assert.equal(vm.runInContext('listOrder()',ctx),'likes_desc');
 assert.equal(vm.runInContext('state.order',ctx),'old');

 // Every appended page keeps the chosen quantity order; switching invalidates a delayed page.
 const originalFetch=ctx.fetch;
 let release,delay=false;
 ctx.fetch=async url=>{
  if(!url.includes('/api/messages/items?'))return originalFetch(url);
  requests.push(url);
  const u=new URL(url,'http://local'),offset=Number(u.searchParams.get('offset'));
  const rows=Array.from({length:Math.min(50,101-offset)},(_,i)=>({id:String(offset+i+1),type:'likes',like_count:101-offset-i,timestamp:101-offset-i,text:'page',raw:{},attachments:[]}));
  const response={ok:true,json:async()=>({account_uid:account,total:101,items:rows})};
  if(delay){delay=false;return new Promise(resolve=>{release=()=>resolve(response)});}
  return response;
 };
 node('order').value='likes_desc';await node('order').onchange();
 await vm.runInContext('loadMore()',ctx);
 assert.equal(last().searchParams.get('offset'),'50');
 assert.equal(vm.runInContext('listOrder()',ctx),'likes_desc');
 assert.equal(vm.runInContext('state.offset',ctx),100);
 delay=true;const oldPage=vm.runInContext('loadMore()',ctx);
 await new Promise(setImmediate);assert.equal(typeof release,'function');
 node('order').value='likes_asc';await node('order').onchange();
 release();await oldPage;
 assert.equal(vm.runInContext('state.offset',ctx),50);
 assert.equal(vm.runInContext('likeOrder',ctx),'likes_asc');
 // A count-ranked notice list must never pass quantity ranking into its user detail query.
 const controls=new Map(),box={innerHTML:'',insertAdjacentHTML(_,html){this.innerHTML+=html},querySelectorAll(){return[]}};
 ctx.detail={dataset:{likeId:'7'},isConnected:true,querySelector(selector){if(selector==='[data-like-content]')return box;if(!controls.has(selector))controls.set(selector,{});return controls.get(selector)}};
 await vm.runInContext('loadLikeUsers(detail)',ctx);
 assert.equal(new URL(requests.at(-1),'http://local').searchParams.get('order'),'old');
 account='99';await vm.runInContext('summary()',ctx);
 assert.equal(node('order').value,'new');
 assert.equal(last().searchParams.get('order'),'new');
 // Desktop statistics share the comment card and log components; preview-only state is gone.
 const ui=fs.readFileSync(root+'/ui/app.slint','utf8');
 const integration=fs.readFileSync(root+'/src/messages_ui.rs','utf8');
 const main=fs.readFileSync(root+'/src/main.rs','utf8');
 assert.equal((ui.match(/StatsPreview \{/g)||[]).length,2);
 assert.equal((ui.match(/StatCard \{ label:/g)||[]).length,10);
 for(const type of ['replies','mentions','likes','private','groups']){
  assert(ui.includes('message-count-'+type+': "0"'));
  assert(integration.includes('set_message_count_'+type));
 }
 for(const obsolete of ['message-preview','message-query','message-sessions','message-like-cards','message-ascending'])assert(!ui.includes(obsolete));
 assert(!main.includes('message_view: Mutex'));assert(!main.includes('messages_ui::View'));assert(!integration.includes('pub struct View'));
 assert(integration.includes('state.doc.counts()'));assert(integration.includes('state.doc.account_uid == uid.to_string()'));
 assert(integration.includes('snapshot_is_current(&account, uid, &snapshot_root)'));assert(!integration.includes('w.get_account_uid()'));
 const api=fs.readFileSync(root+'/src/web.rs','utf8');
 assert(api.indexOf('state.doc.query_ordered(')<api.indexOf('let total = list.len();',api.indexOf('state.doc.query_ordered(')));
 console.log('PASS: like quantity options, server request order, independent preferences, search retention, offset reset, account reset and shared native statistics');
})().catch(e=>{console.error(e);process.exitCode=1});
