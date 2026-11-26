const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict'),path=require('node:path');
const script=fs.readFileSync(process.argv[2]||path.join(__dirname,'../web/workspaces.js'),'utf8');
function environment(start='/comments'){
 let doc;const nodes=new Map();function node(id){if(!nodes.has(id))nodes.set(id,{id,listeners:{},addEventListener(type,fn){this.listeners[type]=fn},scrollTo(options){this.scrollOptions=options;this.scrollTop=options.top},attributes:{},hidden:false,value:'',setAttribute(k,v){this.attributes[k]=v},focus(){doc.activeElement=this},contains(el){return el===this||this.id==='workspace-menu'&&['workspace-comments','workspace-messages'].includes(el?.id)},isConnected:true,scrollHeight:1000,clientHeight:500,scrollWidth:500,clientWidth:500,scrollTop:0,scrollLeft:0,querySelectorAll(){return []}});return nodes.get(id)}
 let url=new URL(start,'http://127.0.0.1:8686');const listeners={},contexts={},made={comments:0,messages:0};let release;
 const history={pushState(s,t,value){url=new URL(value,url)},replaceState(s,t,value){url=new URL(value,url)}};
 const location={get href(){return url.href},get pathname(){return url.pathname}};
 const window={addEventListener(type,f){listeners[type]=f},createCommentsWorkspace(ctx){made.comments++;contexts.comments=ctx;return {ready:new Promise(r=>release=r),activate(){},deactivate(){}}},createMessagesWorkspace(ctx){made.messages++;contexts.messages=ctx;return {ready:Promise.resolve(),activate(){return Promise.resolve()},deactivate(){},refresh(){},accountChanged(uid){contexts.changedAccount=uid}}}};
 doc={getElementById:node,title:'',activeElement:null,addEventListener(type,f){listeners['document:'+type]=f}};
 const ctx=vm.createContext({window,document:doc,location,history,URL,Promise,requestAnimationFrame:f=>f()});vm.runInContext(script,ctx);
 return{nodes,node,contexts,made,window,doc,listeners,back(value){url=new URL(value,url);listeners.popstate()},url:()=>url.href,release:()=>release?.()};
}
(async()=>{
 let e=environment('/comments?q=comment#video-1');assert.deepEqual(e.made,{comments:1,messages:0});assert.equal(e.node('comment-workspace').hidden,false);
 e.node('comment-workspace').scrollTop=120;e.node('workspace-messages').onclick();assert.equal(e.made.messages,1);assert(e.url().endsWith('/messages'));assert.equal(e.node('message-workspace').hidden,false);
 e.contexts.comments.replaceHash('#video-2');assert(e.url().endsWith('/messages'),'hidden comment load must not replace the active message route');
 e.node('workspace-comments').onclick();e.release();await new Promise(r=>setImmediate(r));assert.equal(e.made.comments,1);assert(e.url().endsWith('/comments?q=comment#video-2'));assert.equal(e.node('comment-workspace').scrollTop,120);
 e.back('/messages');await new Promise(r=>setImmediate(r));assert.equal(e.node('workspace-messages').attributes['aria-checked'],'true');assert.equal(e.doc.title,'消息归档 · BiliArchive');assert.equal(e.node('comment-header').hidden,true);
 e=environment('/messages');assert.deepEqual(e.made,{comments:0,messages:1});assert.equal(e.node('message-workspace').hidden,false);
 e.listeners.accountchange({detail:{uid:'99'}});assert.equal(e.contexts.changedAccount,'99');
 const event=key=>({key,preventDefault(){}});
 e.node('workspace-toggle').onkeydown(event('ArrowDown'));assert.equal(e.node('workspace-menu').hidden,false);assert.equal(e.doc.activeElement,e.node('workspace-messages'));
 e.node('workspace-menu').onkeydown(event('ArrowUp'));assert.equal(e.doc.activeElement,e.node('workspace-comments'));
 e.node('workspace-menu').onkeydown(event('Escape'));assert.equal(e.node('workspace-menu').hidden,true);assert.equal(e.doc.activeElement,e.node('workspace-toggle'));
 e.node('workspace-toggle').onclick();e.listeners['document:click']({target:e.node('workspace-comments')});assert.equal(e.node('workspace-menu').hidden,false,'inside click remains open');
 e.listeners['document:click']({target:e.node('outside')});assert.equal(e.node('workspace-menu').hidden,true);
 // The fixed button follows the active content scroller, not a sidebar or JSON panel.
 e=environment('/messages');const messagesRoot=e.node('message-workspace');
 messagesRoot.scrollTop=300;messagesRoot.listeners.scroll();assert.equal(e.node('back-to-top').hidden,true);
 messagesRoot.scrollTop=301;messagesRoot.listeners.scroll();assert.equal(e.node('back-to-top').hidden,false);
 e.node('back-to-top').onclick();assert.equal(messagesRoot.scrollOptions.behavior,'smooth');assert.equal(messagesRoot.scrollTop,0);
 e.window.matchMedia=()=>({matches:true});messagesRoot.scrollTop=600;e.node('back-to-top').onclick();assert.equal(messagesRoot.scrollOptions.behavior,'instant');
 e.node('workspace-comments').onclick();assert(e.url().endsWith('/comments'));const comments=e.node('comments');comments.scrollTop=500;e.node('comment-workspace').scrollTop=90;comments.listeners.scroll();assert.equal(e.node('back-to-top').hidden,false);e.node('back-to-top').onclick();assert.equal(comments.scrollTop,0);assert.equal(e.node('comment-workspace').scrollTop,90);
 e=environment('/messages');e.node('message-workspace').classList={contains:name=>name==='chat-active'};e.node('m-chat-scroll').scrollTop=500;e.node('message-workspace').scrollTop=80;e.node('m-chat-scroll').listeners.scroll();assert.equal(e.node('back-to-top').hidden,false);e.node('back-to-top').onclick();assert.equal(e.node('m-chat-scroll').scrollTop,0);assert.equal(e.node('message-workspace').scrollTop,80);
 e=environment('/messages');e.node('message-workspace').classList={contains:name=>name==='private-active'};e.node('m-conversation-pane').scrollTop=500;e.node('m-conversation-pane').listeners.scroll();assert.equal(e.node('back-to-top').hidden,false);e.node('back-to-top').onclick();assert.equal(e.node('m-conversation-pane').scrollTop,0);
 const html=fs.readFileSync(path.join(__dirname,'../web/index.html'),'utf8');assert(!html.includes('workspace-select'));assert(html.includes('aria-haspopup="menu"'));assert(html.includes('消息归档'));
 const ui=fs.readFileSync(path.join(__dirname,'../ui/app.slint'),'utf8');assert(!/if root.workspace == [01] : Rectangle/.test(ui),'desktop workspaces must stay instantiated');assert(ui.includes('visible: root.workspace == 0'));assert(ui.includes('visible: root.workspace == 1'));
 const messages=fs.readFileSync(path.join(__dirname,'../web/messages.js'),'utf8');assert(messages.includes('if(data.account_uid!==state.uid)return summary(undefined,{eager:true,identity:data});'),'switching back must only reload message data after an account change');
 console.log('PASS: lazy workspaces, direct message route, menu navigation, history, independent state/scroll and hidden-request route isolation');
})().catch(e=>{console.error(e);process.exitCode=1});
