const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict'),path=require('node:path');
const repo=path.join(__dirname,'..');
const source=fs.readFileSync(path.join(repo,'web/messages.js'),'utf8').replace(/\r\n/g,'\n').match(/BEGIN WORKSPACE LOGIC\n([\s\S]*?)\n\/\/ END WORKSPACE LOGIC/)[1];
const n={value:'',querySelectorAll(){return[]}};
const ctx=vm.createContext({URL,URLSearchParams,setTimeout,clearTimeout,location:{href:'http://localhost/messages'},document:{getElementById(){return n},querySelectorAll(){return[]}},fetch:async()=>new Promise(()=>{})});vm.runInContext(source,ctx);const run=s=>vm.runInContext(s,ctx);
run("state.uid='12';sessionNames.set('user_3','普通会话');sessionNames.set('group_4','粉丝团 <unsafe>')");
const base={id:'1',type:'mentions',sender_uid:'3',sender_name:'测试 <user>',text:'legacy combined',timestamp:1700000000,status:0,raw:{user:{avatar:'javascript:alert(1)'},item:{source_content:'正文 <script>',target_reply_content:'原评论',image:'https://example.test/cover.png'}},attachments:[]};
const render=i=>{ctx.fixture=i;return run('renderItem(fixture)')};
let html=render(base);assert(html.includes('notice-mentions'));assert(html.includes('@了我'));assert(html.includes('正文 &lt;script&gt;'));assert(html.includes('原评论'));assert(html.includes('reply-thumbnail'));assert(html.includes('src="/assets/noface.jpg"'));assert(html.includes('原始数据'));assert(!/(?:src|href)="javascript:/.test(html));assert(!html.includes('<script>'));
html=render({...base,type:'likes',like_count:100,like_users_count:51,like_detail:{status:'finished',error:''},raw:{users:[{mid:'3',nickname:'first'},{mid:'4',nickname:'second'}],item:{source_content:'被赞的内容'}}});assert(html.includes('notice-likes'));assert(html.includes('first'));assert(html.includes('second'));assert(html.includes('等总计 100 人赞了我的内容'));assert(html.includes('class="like-avatars"'));assert(html.includes('data-like-card="1"'));assert(html.includes('href="https://space.bilibili.com/4"'));
html=render({...base,type:'likes',like_count:1,like_users_count:0,like_detail:{status:'partial',error:'补采失败'},raw:{user:{nickname:'one'},item:{source_content:'body'}}});assert(!html.includes('notice-multiple'));assert(html.includes('one'));assert(html.includes('总计 1 人赞了我的内容'));
const attachment={id:'a'.repeat(32),path:'attachments/a.mp3',url:'https://example.test/a.mp3',mime:'audio/mpeg'};
html=render({...base,type:'private',session:'user_3',sender_uid:'12',text:'我发出的正文',status:1,raw:{msg_status:1},attachments:[attachment]});assert(html.includes('notice-private mine'));assert(html.includes('我发送的私信'));assert(html.includes('普通会话'));assert(html.includes('已撤回'));assert(html.includes('<audio controls'));assert(!html.includes('reply-thumbnail'));
html=render({...base,type:'private',session:'user_3',raw:{},status:50});assert(!html.includes('notice-private mine'));assert(html.includes('发来的私信'));assert(html.includes('图片已失效'));
html=render({...base,type:'private',session:'group_4',raw:{},status:2});assert(html.includes('notice-groups'));assert(html.includes('粉丝团 &lt;unsafe&gt;'));assert(html.includes('发送了粉丝团消息'));assert(html.includes('被系统撤回'));
for(const type of ['replies','mentions','likes','private','groups']){html=render({...base,type});assert(html.includes('reply-notice'));assert(html.includes('reply-avatar'));assert(html.includes('reply-content'));assert(html.includes('reply-footer'));assert(html.includes('replyTime')===false);assert(html.includes('noopener noreferrer'));assert(html.includes('referrerpolicy="no-referrer"'));}
const main=fs.readFileSync(path.join(repo,'src/main.rs'),'utf8');const ensure=main.slice(main.indexOf('fn ensure_web('),main.indexOf('/// 停止服务'));
assert(ensure.includes('get_workspace()'));assert.equal((ensure.match(/webbrowser::open\(&preview_url\(&url, workspace\)\)/g)||[]).length,2);assert(!ensure.includes('stats.videos'));assert(!main.includes('require_data'));const footer=main.slice(main.indexOf('ui.on_preview_status_clicked'),main.indexOf('// ---- 设置里的 启动 / 停止'));assert(footer.includes('ensure_web(&shared,&weak,&web_url,port,true)'));assert(!footer.includes('invoke_settings_open'));
// Check every Rust module, including the separate message-workspace callback.
let previewCalls=0;
for(const file of fs.readdirSync(path.join(repo,'src')).filter(name=>name.endsWith('.rs'))){
 const rust=fs.readFileSync(path.join(repo,'src',file),'utf8');
 for(const call of rust.matchAll(/\bensure_web\s*\(([^()]*)\)\s*;/g)){
  const args=call[1].split(',').filter(value=>value.trim());
  assert.equal(args.length,5,file+': ensure_web must use the current five-argument signature');++previewCalls;
 }
}
assert(previewCalls>=5);
const messageUI=fs.readFileSync(path.join(repo,'src/messages_ui.rs'),'utf8');
assert(messageUI.includes('ensure_web(&shared, &weak, &web, port, true)'));
assert(!messageUI.includes('webbrowser::open('),'message preview must share the workspace-aware opener');
const css=fs.readFileSync(path.join(repo,'web/workspaces.css'),'utf8'),slint=fs.readFileSync(path.join(repo,'ui/app.slint'),'utf8');
assert(css.includes('--control-height:32px'));assert(css.includes('--search-height:34px'));assert(css.includes('--chip-height:26px'));assert(css.includes('*::-webkit-scrollbar'));assert(css.includes('.search:focus-within'));assert(css.includes('.search .clr'));assert(slint.includes('height: small ? 26px : 32px'));assert(slint.includes('component HubScrollBar'));assert(slint.includes('component StatsPreview'));assert(slint.includes('component StatCard'));assert(!slint.includes('message-preview'));assert(!slint.includes('component HubSearch'));assert(slint.includes('for row[i] in root.videos'));assert(!slint.includes('component HubListView'));assert(!slint.includes('SpinBox {'));assert(!slint.includes('ComboBox {'));
console.log('PASS: unified category cards, multiple/single likes, detail completeness, own/incoming private messages, fan club session, withdrawal/status, audio, escaping, preview workspace routing and shared control hierarchy');
