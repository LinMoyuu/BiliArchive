window.createMessagesWorkspace=function(ctx){
const realDocument=window.document;
const mapped=selector=>selector.replace(/#([\w-]+)/g,(_,id)=>'#m-'+id);
const document={createElement:tag=>realDocument.createElement(tag),getElementById(id){return ctx.root.querySelector('#'+'m-'+id)||ctx.head.querySelector('#'+id);},querySelectorAll(selector){return [...ctx.root.querySelectorAll(mapped(selector)),...ctx.head.querySelectorAll(mapped(selector))];},addEventListener(type,handler){realDocument.addEventListener(type,event=>{if(ctx.active())handler(event);});},get activeElement(){return realDocument.activeElement;}};
const location={get href(){return ctx.url().href;},get search(){return ctx.url().search;},get hash(){return ctx.url().hash;}};
const history={replaceState(state,title,url){ctx.replaceHash(url);}};
// BEGIN WORKSPACE LOGIC

'use strict';
const $=id=>document.getElementById(id);
const loading=(node,mode,label,valid)=>globalThis.archiveLoading?.begin(node,mode,label,valid);
const state={type:'replies',session:'',q:'',order:'new',offset:0,limit:50,total:0,uid:'',request:0};
const sessionNames=new Map();let privateSelection='',privateSessionTop=0;
const groupingModes={replies:'flat',mentions:'flat',my_comments:'flat'};
const groupSorts={replies:'time',mentions:'time'};
let likeOrder='new';
const defaultMyCommentOrder='published_desc';
let myCommentOrder=defaultMyCommentOrder;
const myCommentOrders={
 published_desc:'发布时间倒序',published_asc:'发布时间正序',replies_desc:'回复最多',replies_asc:'回复最少',
 likes_desc:'点赞最多',likes_asc:'点赞最少',new:'最近互动倒序',old:'最近互动正序'
};
function listOrder(){return state.type==='my_comments'?myCommentOrder:state.type==='likes'?likeOrder:state.order;}
// Page requests share a bounded queue; only automatic work pauses while hidden.
let workspaceVisible=true,pageRequests=0;const pageQueue=[],activePages=new Set(),autoTargets=new Map(),pageObservers=new Map();
let stream=newStream();
function newStream(){return{request:-1,next:0,loaded:0,totalItems:0,hasTotal:false,seen:new Set(),busy:false,done:false,error:''}}
function workspaceActive(){return workspaceVisible&&(typeof ctx==='undefined'||ctx.active())}
function pumpPages(){
 for(const task of activePages)if(!task.valid())task.controller?.abort();
 for(let i=pageQueue.length-1;i>=0;i--){if(!pageQueue[i].valid()){pageQueue.splice(i,1)[0].resolve(null);}}
 while(pageRequests<2){
  let index=pageQueue.findIndex(task=>!task.automatic);
  if(index<0&&workspaceActive()&&pageQueue.length)index=0;
  if(index<0)break;
  const task=pageQueue.splice(index,1)[0];++pageRequests;
  task.controller=typeof AbortController==='undefined'?null:new AbortController();
  activePages.add(task);
  Promise.resolve().then(()=>task.valid()?get(task.url,task.controller?.signal):null)
   .then(task.resolve,error=>{if(error.name==='AbortError')task.resolve(null);else task.reject(error);})
   .finally(()=>{activePages.delete(task);--pageRequests;pumpPages();});
 }
}
function pageRequest(url,valid,automatic=false){return new Promise((resolve,reject)=>{pageQueue.push({url,valid,automatic,resolve,reject});pumpPages();})}
function unwatch(target){
 if(!target)return;
 autoTargets.get(target)?.observer?.unobserve(target);
 autoTargets.delete(target);
}
function disconnectPageObservers(){
 for(const observer of pageObservers.values())observer.disconnect();
 pageObservers.clear();
}
function watch(target,action){
 if(!target)return;
 unwatch(target);
 const registration={action,observer:null};
 autoTargets.set(target,registration);
 if(typeof IntersectionObserver==='undefined'||!workspaceActive())return;
 const pane=$('conversation-pane');
 const root=typeof ctx==='undefined'?null:state.type==='private'&&!likeView&&pane?.contains(target)?pane:ctx.root;
 let observer=pageObservers.get(root);
 if(!observer){
  observer=new IntersectionObserver(entries=>{
   if(!workspaceActive())return;
   for(const entry of entries){
    const current=autoTargets.get(entry.target);
    if(entry.isIntersecting&&current?.observer===observer)current.action();
   }
  },{root,rootMargin:'0px 0px 240px 0px'});
  pageObservers.set(root,observer);
 }
 registration.observer=observer;
 observer.observe(target);
}
function resetPages(){disconnectPageObservers();autoTargets.clear();stream=newStream();pumpPages();}
function resumePages(){pumpPages();pumpSubjects();for(const[target,{action}]of [...autoTargets]){if(target.isConnected===false)unwatch(target);else watch(target,action);}}
function appendHTML(root,html){if(html)root.insertAdjacentHTML('beforeend',html);}

let groupEpoch=0;const groupCache=new Map();
function grouping(){return groupingModes[state.type]||'flat'}
function ranking(){return grouping()==='flat'?'time':groupSorts[state.type]||'time'}
function invalidateGroups(){++groupEpoch;groupCache.clear();resetPages();}
function renderHeaderStats(d){
 const counts=d.counts||{},groups=Math.max(0,Number(counts.groups)||0);const number=n=>Math.max(0,Number(n)||0).toLocaleString();
 $('header-stats').innerHTML=`<div class="stat"><b>${number(counts.replies)}</b>条回复</div><div class="stat"><b>${number(counts.mentions)}</b>条@</div><div class="stat"><b>${number(counts.likes)}</b>条赞通知</div><div class="stat"${groups>0?' title="包含粉丝团消息"':''}><b>${number((Number(counts.private)||0)+(Number(counts.groups)||0))}</b>条私信${groups>0?` <small>其中粉丝团 ${number(groups)}</small>`:''}</div>`;
}
function timeLabel(value){return value?new Date(value*1000).toLocaleString():'时间未知'}
const DEFAULT_REPLY_AVATAR='/assets/noface.jpg';
function imageURL(value){if(typeof value!=='string'||!value.trim())return '';const raw=value.trim();return safeURL(raw.startsWith('//')?'https:'+raw:raw)}
function replyThumbnail(i){
 const url=imageURL(i.raw?.item?.image);if(!url)return null;
 const cached=(i.attachments||[]).find(a=>imageURL(a.url)===url&&a.path&&/^[a-f0-9]{32}$/.test(a.id));
 return {url,src:cached?'/api/messages/attachments/'+cached.id:url};
}
function replyDescription(type){return {video:'对我的视频发表了评论',album:'对我的动态发表了评论',dynamic:'对我的动态发表了评论',article:'对我的专栏发表了评论',reply:'回复了我的评论'}[type]||'回复了我'}
function replyTime(value){
 const date=new Date(Number(value)*1000);if(!value||!Number.isFinite(date.getTime()))return '时间未知';
 const pad=n=>String(n).padStart(2,'0');return `${date.getFullYear()}年${date.getMonth()+1}月${date.getDate()}日 ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}
function replyIcon(type){return type==='link'?'<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M9 2h5v5M14 2L7 9M6 3H3v10h10v-3"/></svg>':'<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M5 3L1 8l4 5M11 3l4 5-4 5M9 2L7 14"/></svg>'}
// Native notification routes are converted for this browser view only.
// Match src/message_subjects.rs; only the base-58 remainder becomes a Number.
const MESSAGE_BV_ALPHABET='FcwAPNKTMug3GV5Lj7EJnHpWsx4tb8haYeviqBz6rkCy12mUSDQX9RdoZf';
function routeID(value){
 if(typeof value!=='string'||!/^\d+$/.test(value))return '';
 return value.replace(/^0+/,'');
}
function routeBV(aid){
 const id=routeID(aid);if(!id)return '';
 const n=BigInt(id),max=1n<<51n;if(n>=max)return '';
 let encoded=(max|n)^23442827791579n;const chars=Array.from('BV1000000000');
 for(let at=11;at>=3;--at){chars[at]=MESSAGE_BV_ALPHABET[Number(encoded%58n)];encoded/=58n;}
 [chars[3],chars[9]]=[chars[9],chars[3]];[chars[4],chars[7]]=[chars[7],chars[4]];
 return chars.join('');
}
function routeVideoID(value){
 const aid=routeID(value.replace(/^av/,''));if(aid)return routeBV(aid)||'av'+aid;
 if(value.length!==12||!value.startsWith('BV1'))return '';
 const chars=Array.from(value);[chars[3],chars[9]]=[chars[9],chars[3]];[chars[4],chars[7]]=[chars[7],chars[4]];
 let decoded=0n;
 for(const c of chars.slice(3)){const at=MESSAGE_BV_ALPHABET.indexOf(c);if(at<0)return '';decoded=decoded*58n+BigInt(at);}
 const id=((decoded&((1n<<51n)-1n))^23442827791579n).toString();
 return routeBV(id)===value?value:'';
}
function routeComment(params){
 const read=name=>{const values=params.getAll(name);return values.length===1?routeID(values[0]):'';};
 return read('comment_secondary_id')||read('comment_root_id');
}
function publicMessageRoute(value){
 if(typeof value!=='string'||!value.trim())return null;
 try{
  const raw=value.trim(),url=new URL(raw.startsWith('//')?'https:'+raw:raw);
  if(!['http:','https:'].includes(url.protocol)||url.username||url.password||url.port||!['www.bilibili.com','m.bilibili.com','bilibili.com','t.bilibili.com','b23.tv'].includes(url.hostname))return null;
  url.protocol='https:';
  return {url:url.href,comment:/^#reply[1-9]\d*$/.test(url.hash)};
 }catch{return null;}
}
function nativeMessageRoute(value){
 if(typeof value!=='string'||!value.trim())return null;
 try{
  const url=new URL(value.trim());
  if(['https:','http:'].includes(url.protocol))return publicMessageRoute(value);
  if(url.protocol!=='bilibili:'||url.username||url.password||url.port)return null;
  const parts=url.pathname.replace(/^\/+|\/+$/g,'').split('/').map(decodeURIComponent),host=url.hostname.toLowerCase();
  if(host==='video'&&parts.length===1){
   const video=routeVideoID(parts[0]);if(!video)return null;
   const comment=routeComment(url.searchParams);
   return {url:`https://www.bilibili.com/video/${video}/${comment?'#reply'+comment:''}`,comment:!!comment};
  }
  let id='';
  if(['dynamic','opus','article'].includes(host)&&parts.length===1)id=routeID(host==='article'?parts[0].replace(/^cv/,''):parts[0]);
  else if(host==='following'&&parts[0]==='detail'){
   if(parts.length===2)id=routeID(parts[1]);
   else if(parts.length===1&&url.searchParams.getAll('dynamic_id').length===1)id=routeID(url.searchParams.get('dynamic_id'));
  }
  if(!id)return null;
  return {url:host==='article'?`https://www.bilibili.com/read/cv${id}`:`https://www.bilibili.com/opus/${id}`,comment:false};
 }catch{return null;}
}
function notificationDestination(item){
 const raw=item.raw?.item||{};
 const destination=nativeMessageRoute(raw.native_uri)||publicMessageRoute(item.url)||publicMessageRoute(raw.uri);
 if(destination&&item.type==='replies'&&item.reply_rpid){
  const url=focusedComment(destination,routeID(item.reply_rpid));
  return {url,comment:url.includes('#reply'),label:url.includes('#reply')?'查看评论':'查看主题'};
 }
 return destination?{...destination,label:destination.comment?'查看评论':'查看主题'}:null;
}
function notificationLink(item){
 const destination=notificationDestination(item);
 return destination?link(destination.url,replyIcon('link')+destination.label):'';
}

// Subject presentation is separate from notification text and archive data.
let subjectEpoch=0,subjectRequests=0;const subjectData=new Map(),subjectBusy=new Map(),subjectQueue=[],subjectAttempts=new Set();
function resetSubjects(){
 ++subjectEpoch;
 for(const task of subjectBusy.values())task.controller?.abort();
 subjectData.clear();subjectBusy.clear();subjectAttempts.clear();subjectQueue.length=0;
}
function rememberSubject(subject, enriched=false){
 if(!subject?.key)return null;
 const old=subjectData.get(subject.key)||{};
 for(const [key,value]of Object.entries(subject)){
  if(value===''||value==null)continue;
  if(key==='cover_checked'){old[key]=old[key]||value;continue;}
  if(enriched||!old[key])old[key]=value;
 }
 subjectData.set(subject.key,old);return old;
}
function subjectMissing(s){const has=value=>typeof value==='string'&&!!value.trim(),cover=s.cover_checked===false&&!s.cover;return s.type==='video'?!(has(s.title)&&has(s.bvid))||cover:s.type==='dynamic'?!has(s.text)||cover:s.type==='article'?!has(s.title):false;}
function subjectReason(label,reason,classes=''){
 return reason?`<span class="subject-reason ${classes}" tabindex="0" role="note" aria-label="${esc(label+'：'+reason)}" data-reason="${esc(reason)}">${esc(label)}</span>`:`<span class="${classes}">${esc(label)}</span>`;
}
function subjectContent(s,mode,source=''){
 if(mode==='cover'||mode==='like-cover'){const cover=imageURL(s.cover);return cover?`<a class="group-cover-link" href="${esc(cover)}" target="_blank" rel="noopener noreferrer" referrerpolicy="no-referrer"><span class="archive-image-slot"><img data-group-cover data-archive-src="${esc(resourceImages.get(cover)||(s.cover_src?.startsWith('/api/messages/attachments/')?s.cover_src:cover))}" data-original="${esc(cover)}" data-archive-original="${esc(cover)}" alt="主题封面" loading="lazy" referrerpolicy="no-referrer"></span></a>`:mode==='like-cover'?'':'<span class="group-cover-placeholder" aria-label="暂无封面"></span>';}
 const video=s.type==='video',dynamic=s.type==='dynamic';
 const ident=video?(s.bvid||(s.id?'av'+s.id:'')):s.id?(dynamic?'动态 ':'专栏 ')+s.id:'';
 const value=String((video?s.title:s.text||s.title)||'').trim();
 const busy=!!s.key&&subjectBusy.get(s.key)?.started,missing=subjectMissing(s),needsDisplay=!value||dynamic&&!String(s.text||'').trim()||s.cover_checked===false&&!s.cover,actionable=!!s.key&&missing&&needsDisplay;
 const field=!value?(dynamic?'正文':'主题'):dynamic&&!String(s.text||'').trim()?'正文':s.cover_checked===false&&!s.cover?'封面':video&&!s.bvid?'BV号':'信息';
 const reason=busy?'正在读取主题信息，请稍候':actionable?String(s.error||'尚未获取完整主题信息，可手动重试'):'';
 const limited=!busy&&actionable&&/风控|受限/.test(s.error||'');
 const text=value?(value.length>80&&mode!=='heading'?value.slice(0,80)+'…':value):(busy?'正在获取'+field+'…':field+'未获取');
 // The subject label always precedes its single preferred identifier.
 const label=value?(mode==='body'?`<span class="subject-title">${esc(text)}</span>`:link(s.url,esc(text))||esc(text)):subjectReason(text,limited?'':reason,'subject-missing');
 const identifier=ident?`<span class="subject-ident">${link(s.url,esc(ident))||esc(ident)}</span>`:'';
 const status=limited?subjectReason('暂受限',reason,'subject-status'):value&&actionable?subjectReason(busy?'正在获取'+field+'…':field+'未获取',reason,'subject-status'):'';
 const retry=actionable&&!busy?`<button data-subject-retry aria-label="重试获取${esc(field)}">重试</button>`:'';
 const actions=status||retry?`<span class="subject-retry-row">${status}${retry}</span>`:'';
 const line=`<span class="subject-line">${label}${identifier}${commentSource(source)}${actions}</span>`;
 if(mode!=='body')return line;
 if(!value||dynamic&&!String(s.text||'').trim())return `<div class="reply-subject subject-empty">${line}</div>`;
 return `<details class="reply-subject"><summary aria-label="${dynamic?'动态主题':'所属主题'}">${line}</summary><div class="subject-text">${esc(value)}</div>${link(s.url,'查看主题')}</details>`;
}

function commentSource(source){return source?`<span class="my-comment-source">${esc(source)}</span>`:'';}
function subjectHTML(i,mode,source=''){
 const s=rememberSubject(i.subject);
 if(s&&mode==='heading'&&s.type!=='video')return '';
 if(s){const tag=mode==='body'?'div':'span';return `<${tag} class="subject-slot" data-subject-key="${esc(s.key)}" data-subject-mode="${mode}"${source?` data-comment-source="${esc(source)}"`:''}>${subjectContent(s,mode,source)}</${tag}>`;}
 const raw=i.raw?.item||{};
 const url=i.url||raw.uri||'',videoPath=String(url).match(/\/video\/(BV[a-zA-Z0-9]{10}|av[0-9]+)(?:[/?#]|$)/)?.[1]||'';
 if(mode==='heading'&&raw.type==='video')return subjectContent({type:'video',title:raw.title||'',url,bvid:raw.bvid||(videoPath.startsWith('BV')?videoPath:''),id:/^[0-9]+$/.test(String(raw.subject_id||''))?String(raw.subject_id):videoPath.startsWith('av')?videoPath.slice(2):'',cover_checked:true},mode);
 if(mode==='body'&&['dynamic','album'].includes(raw.type))return subjectContent({type:'dynamic',text:raw.title||raw.desc||'',url,id:raw.dynamic_id||'',cover_checked:true},mode);
 return '';
}
function bindSubjectReasons(root){
 root.querySelectorAll?.('[data-reason]').forEach(element=>{
  const position=()=>{const rect=element.getBoundingClientRect(),browser=typeof window==='undefined'?globalThis:window,width=browser.innerWidth||800,height=browser.innerHeight||600,tipWidth=Math.min(280,width-24);
   element.style.setProperty('--subject-tip-x',Math.max(12,Math.min(rect.left,width-tipWidth-12))+'px');
   element.style.setProperty('--subject-tip-y',(rect.bottom+132<=height?rect.bottom+6:Math.max(8,rect.top-126))+'px');
  };
  element.onmouseenter=position;element.onfocus=position;
 });
}
function subjectRoots(){
 return [$('items'), ...(likeView?[$('like-detail')]:[])];
}
function refreshSubjectNodes(key){
 const scroller=messageScrollRoot();
 const bounds=scroller?.getBoundingClientRect?.(),top=scroller?.scrollTop;
 const root=likeView?$('like-detail'):$('items');
 const anchor=bounds&&top>0?[...root.querySelectorAll('article, details.message-group, .like-subject')]
   .find(node=>node.getBoundingClientRect?.()?.bottom>bounds.top):null;
 const anchorTop=anchor?.getBoundingClientRect().top;
 for(const root of subjectRoots())root.querySelectorAll('[data-subject-key]').forEach(node=>{
  if(key&&node.dataset.subjectKey!==key)return;
  const subject=subjectData.get(node.dataset.subjectKey);if(!subject)return;
  const html=subjectContent(subject,node.dataset.subjectMode,node.dataset.commentSource);
  if(node._subjectHTML===html)return;
  const opened=node.querySelector('details')?.open;
  node.innerHTML=html;
  const detail=node.querySelector('details');if(detail&&opened)detail.open=true;
  node._subjectHTML=html;
  bindGroupCovers(node);bindSubjectReasons(node);
  const retry=node.querySelector('[data-subject-retry]');if(retry){retry.disabled=subjectBusy.has(subject.key);retry.onclick=e=>{e.preventDefault();e.stopPropagation();queueSubject(subject.key,true);};}
 });
 if(anchor&&anchor.isConnected!==false&&scroller.scrollTop===top){
  const shift=anchor.getBoundingClientRect().top-anchorTop;
  if(shift)scroller.scrollTop=top+shift;
 }
}
function bindGroupCovers(root){
 root.querySelectorAll?.('[data-group-cover]').forEach(img=>{img.onerror=()=>{if((typeof window==='undefined'||!window.archiveOffline)&&!img.dataset.originalTried&&img.getAttribute('src')!==img.dataset.original){img.dataset.originalTried='true';img.src=img.dataset.original;}else{const link=img.closest?.('.group-cover-link')||img.parentElement;const placeholder=document.createElement?document.createElement('span'):null;if(placeholder){placeholder.className='group-cover-placeholder';placeholder.setAttribute('aria-label','封面加载失败');link.replaceWith(placeholder);}else link.hidden=true;}};});
 root.querySelectorAll?.('.group-cover-link').forEach(link=>{link.onclick=e=>e.stopPropagation();});
}
function queueSubject(key,retry=false){
 const subject=subjectData.get(key);if(!subject||subjectBusy.has(key)||!subjectMissing(subject)||!retry&&(subject.error||subjectAttempts.has(key)))return;
 subjectAttempts.add(key);
 const task={key,retry,epoch:subjectEpoch,uid:state.uid};subjectBusy.set(key,task);subjectQueue.push(task);pumpSubjects();
}
function pumpSubjects(){
 while(subjectRequests<2&&subjectQueue.length&&workspaceActive()){
  const task=subjectQueue.shift();if(task.epoch!==subjectEpoch||task.uid!==state.uid)continue;
  ++subjectRequests;task.started=true;refreshSubjectNodes(task.key);
  const params=new URLSearchParams({account_uid:task.uid,keys:task.key,retry:task.retry?'1':'0'});
  task.controller=typeof AbortController==='undefined'?null:new AbortController();
  get('/api/messages/subjects?'+params,task.controller?.signal).then(data=>{
   if(task.epoch!==subjectEpoch||task.uid!==state.uid||data.account_uid!==task.uid)return;
   for(const s of data.subjects||[]){if(s.key===task.key){const merged=rememberSubject(s,true);merged.error=s.error||'';}}
  }).catch(()=>{if(task.epoch===subjectEpoch){const s=subjectData.get(task.key);if(s)s.error='主题内容暂不可用，可稍后重试';}}).finally(()=>{
   --subjectRequests;if(subjectBusy.get(task.key)===task)subjectBusy.delete(task.key);
   if(task.epoch===subjectEpoch)refreshSubjectNodes(task.key);pumpSubjects();
  });
 }
}
function hydrateSubjects(){
 refreshSubjectNodes();
 for(const root of subjectRoots())root.querySelectorAll('[data-subject-key]').forEach(node=>queueSubject(node.dataset.subjectKey));
}

let rawSequence=0;
function rawAction(i){
 const id='message-raw-'+(++rawSequence);
 return `<button class="raw-toggle" data-raw-toggle aria-expanded="false" aria-controls="${id}">${replyIcon('raw')}原始数据</button><pre class="raw-panel" id="${id}" hidden>${esc(JSON.stringify(i.raw,null,2))}</pre>`;
}
function bindRawActions(root){
 root.querySelectorAll('[data-raw-toggle]').forEach(button=>{button.onclick=event=>{event?.stopPropagation();const panel=button.parentElement.querySelector('#'+button.getAttribute('aria-controls'));if(!panel)return;panel.hidden=!panel.hidden;button.setAttribute('aria-expanded',String(!panel.hidden));};});
}
const likeCards=new Map();let likeView=null;
// Like notices identify the liked reply via native routes, not the notification ID.
function likeNativeDestination(value){
 let destination=nativeMessageRoute(value),comment='';
 if(typeof value!=='string'||!value.trim())return {destination,comment};
 try{
  const url=new URL(value.trim());
  if(url.protocol!=='bilibili:'&& !publicMessageRoute(value))return {destination:null,comment:''};
  if(url.username||url.password||url.port)return {destination:null,comment:''};
  if(url.protocol==='bilibili:'&&!destination&&url.hostname.toLowerCase()!=='comment')return {destination:null,comment:''};
  const read=name=>{const values=url.searchParams.getAll(name);return values.length===1?routeID(values[0]):'';};
  comment=read('comment_secondary_id')||read('anchor')||read('comment_root_id');
  if(url.protocol==='bilibili:'&&url.hostname.toLowerCase()==='comment'){
   const parts=url.pathname.replace(/^\/+|\/+$/g,'').split('/').map(decodeURIComponent);
   if(parts.length!==4||parts[0]!=='detail'||!parts.slice(1).every(v=>routeID(v)))return {destination:null,comment:''};
   comment=comment||routeID(parts[3]);
   const entries=url.searchParams.getAll('enterUri');
   if(entries.length===1&&entries[0].trim()){
    try{
     const entry=new URL(entries[0]);
     // Normalize the known app-only opus/detail form without accepting recursive comment routes.
     if(entry.protocol==='bilibili:'&&entry.hostname.toLowerCase()==='opus'&&/^\/detail\/[0-9]+\/?$/.test(entry.pathname))entry.pathname=entry.pathname.replace('/detail/','/');
     destination=nativeMessageRoute(entry.href);
    }catch{destination=null;}
   }
  }
 }catch{return {destination:null,comment:''};}
 return {destination,comment};
}
function likeDestination(i){
 const item=i.raw?.item||{};
 if(item.type!=='reply')return i.url||item.uri||'';
 const native=likeNativeDestination(item.native_uri);
 const destination=native.destination||publicMessageRoute(i.url)||publicMessageRoute(item.uri);
 if(!destination)return '';
 if(i.my_comment_rpid)return focusedComment(destination,routeID(i.my_comment_rpid));
 // Existing valid anchors outrank the fallback item ID. Unsafe numeric IDs cannot be repaired.
 const saved=/^#reply([1-9]\d*)$/.exec(new URL(destination.url).hash)?.[1]||'';
 const itemID=typeof item.item_id==='number'?(Number.isSafeInteger(item.item_id)&&item.item_id>0?String(item.item_id):''):routeID(item.item_id);
 const comment=native.comment||saved||itemID;
 return focusedComment(destination,comment);
}
function focusedComment(destination,comment){
 if(!comment)return destination.url;
 const url=new URL(destination.url),dynamic=/^\/(?:opus|dynamic)\/[1-9]\d*\/?$/.test(url.pathname)||url.hostname==='t.bilibili.com'&&/^\/[1-9]\d*\/?$/.test(url.pathname);
 const supported=dynamic||/^\/video\/(?:BV[0-9A-Za-z]+|av[1-9]\d*)\/?$/.test(url.pathname)||/^\/read\/cv[1-9]\d*\/?$/.test(url.pathname);
 if(!supported)return destination.url;
 if(dynamic)url.searchParams.set('comment_on','1');
 url.hash='reply'+comment;
 return url.href;
}
function likeTarget(i){
 const item=i.raw?.item||{};
 return {type:item.type||'',text:[i.my_comment?.text,item.title,item.target_reply_content,item.root_reply_content,item.source_content,i.text].find(s=>typeof s==='string'&&s.trim())||'主题内容暂不可用',url:likeDestination(i),thumb:replyThumbnail(i)};
}
function likeActors(i){
 const raw=i.raw||{},list=Array.isArray(raw.users)?raw.users:raw.user?[raw.user]:[];
 const known=new Set();return list.filter(u=>u&&typeof u==='object').filter(u=>{const key=String(u.mid||u.uid||u.nickname||u.uname||'');if(!key||known.has(key))return false;known.add(key);return true;}).slice(0,2);
}
function actorLink(u,photo=false){
 const uid=String(u.mid||u.uid||''),valid=/^[0-9]+$/.test(uid)&&!/^0+$/.test(uid),name=String(u.nickname||u.uname||(valid?'用户 '+uid:'匿名用户'));
 const content=photo?avatarPhoto(uid,name,[u.avatar,u.face]):esc(name);
 return valid?link('https://space.bilibili.com/'+uid,content):content;
}
function renderLikeNotice(i){
 likeCards.set(String(i.id),i);
 const actors=likeActors(i),target=likeTarget(i),count=String(i.like_count||0);
 const people=actors.length?actors.map(u=>actorLink(u)).join('、'):'点赞用户';
 const avatars=actors.length?actors.map(u=>actorLink(u,true)).join(''):avatarPhoto('','默认用户');
 const noun={video:'视频',reply:'评论',dynamic:'动态',album:'动态',article:'专栏'}[target.type]||'内容';
 const right=target.thumb?`<a class="reply-thumbnail" href="${esc(target.thumb.url)}" target="_blank" rel="noopener noreferrer" referrerpolicy="no-referrer"><span class="archive-image-slot"><img data-reply-thumbnail data-archive-original="${esc(target.thumb.url)}" data-archive-src="${esc(target.thumb.src)}" alt="被赞内容缩略图" loading="lazy" referrerpolicy="no-referrer"></span></a>`:`<div class="like-topic-excerpt">${esc(target.text)}</div>`;
 return `<article class="reply-notice notice-likes like-notice" data-like-card="${esc(i.id)}" role="button" tabindex="0" aria-label="查看点赞详情"><div class="like-avatars${actors.length<=1?' like-avatar-single':''}">${avatars}</div><div class="reply-content"><div class="reply-heading"><strong>${people}</strong><span>${actors.length?'等':''}总计 ${esc(count)} 人赞了我的${esc(noun)}</span></div><div class="reply-footer"><time>${esc(replyTime(i.timestamp))}</time><button class="like-open" data-like-open="${esc(i.id)}">点赞详情</button>${notificationLink(i)}${rawAction(i)}</div></div><div class="like-topic">${i.subject&&!target.thumb?subjectHTML(i,'like-cover'):''}${right}</div></article>`;
}
function messageScrollRoot(){return typeof ctx==='undefined'?$('workspace-scroll'):ctx.root;}
function closeLikeView(restore=true){
 if(!likeView)return;
 const old=likeView;likeView=null;
 const detail=$('like-detail').querySelector('[data-like-id]');if(detail){unwatch(detail.querySelector('[data-like-sentinel]'));detail.dataset.request=String(Number(detail.dataset.request||0)+1);}pumpPages();
 $('like-detail').hidden=true;$('list-pane').hidden=false;
 if(restore){messageScrollRoot().scrollTop=old.listTop;pagination();old.origin?.focus?.({preventScroll:true});}
}
function openLikeView(id,origin){
 if(likeView?.id===String(id))return;
 const item=likeCards.get(String(id));if(!item)return;
 const target=likeTarget(item),root=messageScrollRoot(),listTop=likeView?.listTop??root.scrollTop;
 if(likeView)closeLikeView(false);
 likeView={id:String(id),uid:state.uid,listTop,origin};
 $('list-pane').hidden=true;$('like-detail').hidden=false;unwatch($('sentinel'));pumpPages();
 const remaining=(item.attachments||[]).filter(a=>!target.thumb||imageURL(a.url)!==target.thumb.url);
 const thumb=target.thumb?`<a class="reply-thumbnail" href="${esc(target.thumb.url)}" target="_blank" rel="noopener noreferrer" referrerpolicy="no-referrer"><span class="archive-image-slot"><img data-reply-thumbnail data-archive-original="${esc(target.thumb.url)}" data-archive-src="${esc(target.thumb.src)}" alt="被赞内容封面" referrerpolicy="no-referrer"></span></a>`:'';
 $('like-detail').innerHTML=`<div class="like-breadcrumb"><button data-like-back>收到的赞</button><span aria-hidden="true">›</span><span>点赞详情</span></div><div class="like-subject"><div><div class="like-subject-title">${link(target.url,esc(target.text))||esc(target.text)}</div>${item.subject&&subjectMissing(item.subject)?subjectHTML(item,'topic'):''}<div class="muted like-subject-notice" data-like-notice role="status"></div></div>${thumb}</div>${remaining.length?`<div class="attachments">${attachments(remaining)}</div>`:''}<section class="like-detail-users" data-like-id="${esc(item.id)}" data-order="${state.order}"><div class="like-detail-tools"><label class="search"><input data-like-query aria-label="搜索点赞用户" placeholder="搜索昵称或 UID" value=""></label><select data-like-order aria-label="点赞时间排序"><option value="new"${state.order==='new'?' selected':''}>时间倒序</option><option value="old"${state.order==='old'?' selected':''}>时间正序</option></select></div><div data-like-content></div><div class="like-detail-progress" data-like-pager><span class="muted" data-like-progress role="status"></span><span data-like-error role="alert"></span><button data-like-more>加载更多</button></div><div data-like-sentinel class="load-sentinel" aria-hidden="true"></div></section>`;
 $('like-detail').querySelector('[data-like-back]').onclick=()=>closeLikeView();
 const detail=$('like-detail').querySelector('[data-like-id]'),input=detail.querySelector('[data-like-query]');
 let timer;input.oninput=()=>{clearTimeout(timer);timer=setTimeout(()=>{if(detail.isConnected!==false&&likeView?.id===String(item.id)&&likeView.uid===state.uid)loadLikeUsers(detail)},200)};
 detail.querySelector('[data-like-order]').onchange=event=>{detail.dataset.order=event.target.value;loadLikeUsers(detail)};
 root.scrollTop=0;bindReplyImages($('like-detail'));hydrateSubjects();return loadLikeUsers(detail);
}
function bindLikeCards(root){
 root.querySelectorAll('[data-like-card]').forEach(card=>{
  const image=card.querySelector('[data-reply-thumbnail]'),item=likeCards.get(card.dataset.likeCard);
  if(image&&item){const target=likeTarget(item);image.onerror=()=>{if((typeof window==='undefined'||!window.archiveOffline)&&!image.dataset.originalTried&&image.getAttribute('src')!==target.thumb.url){image.dataset.originalTried='true';image.src=target.thumb.url;}else{const topic=card.querySelector('.like-topic');topic.textContent=target.text;topic.classList.add('like-topic-excerpt');}};}
  if(card.dataset.cardBound)return;card.dataset.cardBound='true';
  card.onclick=event=>{if(event.target.closest('a,button,summary,details,pre,input,select'))return;openLikeView(card.dataset.likeCard,card);};
  card.onkeydown=event=>{if(event.target!==card||!['Enter',' '].includes(event.key))return;event.preventDefault();openLikeView(card.dataset.likeCard,card);};
  card.querySelector('[data-like-open]').onclick=event=>{event.stopPropagation();return openLikeView(card.dataset.likeCard,card);};
 });
 bindRawActions(root);
}

function renderReply(i){
 const raw=i.raw||{},item=raw.item||{},user=raw.user||raw.users?.[0]||{};
 const uid=/^[0-9]+$/.test(i.sender_uid||'')&&(i.sender_uid||'').replace(/^0+/,'')?i.sender_uid:'';
 const name=[i.sender_name,user.nickname,user.uname,uid?'用户 '+uid:'未知用户'].find(s=>typeof s==='string'&&s.trim());
 const avatarHTML=avatarPhoto(uid,name,[user.avatar,user.face]);
 const avatarLink=uid?link('https://space.bilibili.com/'+uid,avatarHTML):avatarHTML;
 const userLink=uid?link('https://space.bilibili.com/'+uid,esc(name)):esc(name);
 const body=typeof item.source_content==='string'&&item.source_content.trim()?item.source_content:String(i.text||'');
 const pictures=item.type==='reply'?myCommentImages(i.my_comment):'';
 const excerpt=item.type==='reply'?(pictures||i.my_comment?.text?.trim()?i.my_comment?.text:i.comment_excerpt||item.target_reply_content):'';
 const quoteText=typeof excerpt==='string'&&excerpt.trim()!==body.trim()?esc(excerpt):'';
 const quote=quoteText||pictures?`<blockquote class="reply-quote">${quoteText}${pictures}</blockquote>`:'';
 const thumb=replyThumbnail(i),remaining=(i.attachments||[]).filter(a=>!thumb||imageURL(a.url)!==thumb.url);
 const thumbnail=thumb?`<a class="reply-thumbnail" href="${esc(thumb.url)}" target="_blank" rel="noopener noreferrer" referrerpolicy="no-referrer"><span class="archive-image-slot"><img data-reply-thumbnail data-archive-original="${esc(thumb.url)}" data-archive-src="${esc(thumb.src)}" alt="通知关联内容缩略图" loading="lazy" referrerpolicy="no-referrer"></span></a>`:'';
 return `<article class="reply-notice${thumb?' has-thumbnail':''}"><div class="reply-avatar">${avatarLink}</div><div class="reply-content"><div class="reply-heading"><strong>${userLink}</strong><span>${esc(replyDescription(item.type))}</span>${subjectHTML(i,'heading')}</div><div class="reply-body">${esc(body)}</div>${quote}${i.subject?.type==='video'?'':subjectHTML(i,'body')}${remaining.length?`<div class="attachments">${attachments(remaining)}</div>`:''}<div class="reply-footer"><time>${esc(replyTime(i.timestamp))}</time>${notificationLink(i)}${rawAction(i)}</div></div>${thumbnail}</article>`;
}
function bindReplyImages(root){
 hydrateProfileAvatars(root);
 root.querySelectorAll('[data-reply-avatar]').forEach(img=>{img.onerror=()=>{if(img.getAttribute('src')!==DEFAULT_REPLY_AVATAR&&!img.dataset.defaultTried){img.dataset.defaultTried='true';img.src=DEFAULT_REPLY_AVATAR;}else{img.hidden=true;}};});
 root.querySelectorAll('[data-reply-thumbnail]').forEach(img=>{img.onerror=()=>{
  if(globalThis.archiveImages)globalThis.archiveImages.placeholder(img);
  else if(img.style)img.style.visibility='hidden';
 };});
}

function noticeUser(i){
 const raw=i.raw||{},user=raw.user||raw.users?.[0]||{};
 const uid=/^[0-9]+$/.test(i.sender_uid||'')&&(i.sender_uid||'').replace(/^0+/,'')?i.sender_uid:'';
 const name=[i.sender_name,user.nickname,user.uname,uid?'用户 '+uid:'未知用户'].find(s=>typeof s==='string'&&s.trim());
 const photo=avatarPhoto(uid,name,[user.avatar,user.face,raw.sender_face,raw.face,i.sender_avatar_url],{lookup:i.type==='private'});
 return {uid,name,photo:uid?link('https://space.bilibili.com/'+uid,photo):photo,label:uid?link('https://space.bilibili.com/'+uid,esc(name)):esc(name)};
}
// Avatar metadata is separate from archived messages. Only visible archived senders are queried.
const resourceImages=new Map(),profiles=new Map(),profileAttempts=new Set(),profileQueue=new Map(),profileRequests=new Map(),profileObservers=new Map();
let profileEpoch=0,profileActive=0,profileRetry=false;
function disconnectProfileObservers(){
 for(const observer of profileObservers.values())observer.disconnect();
 profileObservers.clear();
}
function stopProfileIndicators(task){
 for(const img of task?.images||[])globalThis.archiveImages?.stopIndicator(img,'profile');
 task?.images.clear();
}
function pauseProfileIndicators(){
 for(const task of profileQueue.values())stopProfileIndicators(task);
 for(const task of profileRequests.values())stopProfileIndicators(task);
}
function avatarURL(value){
 const url=imageURL(value);return url&&url!==safeURL(DEFAULT_REPLY_AVATAR)&&!/\/member\/noface\.jpg(?:[?#]|$)/i.test(url)?url:'';
}
function rememberAvatar(uid,value){
 const key=routeID(String(uid||'')),avatar=avatarURL(value);if(!key||!avatar)return;
 profiles.set(key,{...profiles.get(key),uid:key,avatar_url:avatar});
 stopProfileIndicators(profileQueue.get(key));profileQueue.delete(key);
}
function profileAvatar(uid,fallback=''){
 const values=Array.isArray(fallback)?fallback:[fallback];
 return values.map(avatarURL).find(Boolean)||avatarURL(profiles.get(String(uid))?.avatar_url)||DEFAULT_REPLY_AVATAR;
}
function avatarPhoto(uid,name,candidates=[],options={}){
 const key=routeID(String(uid||'')),values=Array.isArray(candidates)?candidates:[candidates],direct=values.map(avatarURL).find(Boolean);
 if(direct)rememberAvatar(key,direct);
 const avatar=profileAvatar(key,values),lookup=options.lookup&&key&&key!==state.uid;
 return `<span class="archive-image-slot archive-avatar-slot${options.className==='group-avatar'?' archive-group-avatar-slot':''}"><img${options.className?` class="${esc(options.className)}"`:''} data-reply-avatar data-archive-original="${esc(avatar)}"${key?` data-profile-uid="${esc(key)}"`:''}${direct?' data-avatar-direct="true"':''}${lookup&&avatar===DEFAULT_REPLY_AVATAR?' data-profile-missing="true"':''} data-archive-src="${esc(resourceImages.get(avatar)||avatar)}" alt="${esc(name)}的头像" loading="lazy" referrerpolicy="no-referrer"></span>`;
}
function profilePhoto(uid,name,fallback=''){return avatarPhoto(uid,name,fallback,{lookup:true});}
function profilePriority(uid){return state.session==='user_'+uid||!!currentChat?.items.some(i=>i.sender_uid===uid)?1:0;}
function applyProfileImages(root){
 root.querySelectorAll('[data-profile-uid]').forEach(img=>{
  const photo=avatarURL(profiles.get(img.dataset.profileUid)?.avatar_url);if(!photo||img.dataset.avatarDirect)return;
  const source=resourceImages.get(photo)||photo;
  if((img.getAttribute('data-archive-src')||img.getAttribute('src'))!==source){
   img.setAttribute('data-archive-original',photo);
   if(typeof window!=='undefined'&&window.archiveImages)window.archiveImages.setSource(img,source);
   else img.src=source;
   img.hidden=false;delete img.dataset.defaultTried;
  }
  delete img.dataset.profileMissing;for(const observer of profileObservers.values())observer.unobserve(img);
 });
}
function queueProfile(uid,img=null){
 if(typeof window!=='undefined'&&window.archiveOffline)return;
 if(!routeID(uid)||uid===state.uid||profiles.has(uid))return;
 let task=profileRequests.get(uid)||profileQueue.get(uid);
 if(!task){
  if(profileAttempts.has(uid))return;
  task={uid,account:state.uid,epoch:profileEpoch,retry:profileRetry,images:new Set()};
  profileQueue.set(uid,task);
 }
 if(img){
  task.images.add(img);
  globalThis.archiveImages?.startIndicator(img,'profile');
 }
 pumpProfiles();
}
function pumpProfiles(){
 if(!workspaceActive()||state.type!=='private'||(typeof window!=='undefined'&&window.archiveOffline))return;
 while(profileActive<2&&profileQueue.size){
  const [key,task]=[...profileQueue.entries()].sort((a,b)=>profilePriority(b[0])-profilePriority(a[0]))[0];profileQueue.delete(key);
  if(task.epoch!==profileEpoch||task.account!==state.uid||profileAttempts.has(key))continue;
  profileAttempts.add(key);profileRequests.set(key,task);++profileActive;
  const current=()=>task.epoch===profileEpoch&&task.account===state.uid;
  const params=new URLSearchParams({account_uid:task.account,uid:key,...(task.retry?{retry:'1'}:{})});
  get('/api/messages/profile?'+params).then(data=>{
   if(!current()||data.account_uid!==state.uid||String(data.profile?.uid)!==key)return;
   const avatar=avatarURL(data.profile.avatar_url);if(!avatar)return;
   profiles.set(key,{...data.profile,avatar_url:avatar});
   if(state.type==='private'){
    applyProfileImages($('sessions'));applyProfileImages($('items'));
    if(currentChat)applyProfileImages(currentChat.element);
   }
  }).catch(()=>{}).finally(()=>{
   stopProfileIndicators(task);
   if(profileRequests.get(key)===task)profileRequests.delete(key);
   --profileActive;pumpProfiles();
  });
 }
}
function hydrateProfileAvatars(root){
 if(state.type!=='private')return;
 applyProfileImages(root);
 const images=root.querySelectorAll('[data-profile-missing]');if(!images.length)return;
 if(typeof IntersectionObserver==='undefined'){
  if(workspaceActive())images.forEach(img=>queueProfile(img.dataset.profileUid,img));return;
 }
 if(!workspaceActive())return;
 const scrollRoot=root===$('sessions')?$('session-pane'):
  root===currentChat?.element?$('chat-scroll'):$('conversation-pane');
 let observer=profileObservers.get(scrollRoot);
 if(!observer){
  observer=new IntersectionObserver(entries=>{
   if(!workspaceActive()||profileObservers.get(scrollRoot)!==observer)return;
   for(const entry of entries)if(entry.isIntersecting&&entry.target.isConnected!==false){
    observer.unobserve(entry.target);
    queueProfile(entry.target.dataset.profileUid,entry.target);
   }
  },{root:scrollRoot,rootMargin:'120px'});
  profileObservers.set(scrollRoot,observer);
 }
 images.forEach(img=>observer.observe(img));
}
function resetProfiles(keepSaved=false){
 ++profileEpoch;disconnectProfileObservers();pauseProfileIndicators();profileQueue.clear();profileRequests.clear();profileAttempts.clear();if(!keepSaved){profiles.clear();resourceImages.clear();}profileRetry=keepSaved;
}
function resumeProfiles(){
 hydrateProfileAvatars($('sessions'));hydrateProfileAvatars($('items'));if(currentChat)hydrateProfileAvatars(currentChat.element);pumpProfiles();
}

const sessionInfo=new Map(),chatCaches=new Map();let currentChat=null,chatEpoch=0,chatScope='',chatObserver;
function privateSearch(){return state.type==='private'&&state.q.trim().length>0;}
function privateChat(){return state.type==='private'&&!!state.session&&!privateSearch();}
function chatScroll(){return $('chat-scroll');}
function rememberChat(){if(currentChat)currentChat.top=chatScroll().scrollTop;}
function hideChat(){
 rememberChat();if(currentChat){currentChat.pending=null;currentChat.busy=false;}++chatEpoch;currentChat=null;chatObserver?.disconnect();
 if($('chat-view').hidden===false)$('chat-messages').replaceChildren?.();
 $('chat-view').hidden=true;messageScrollRoot().classList?.remove('chat-active');
 if(typeof ctx!=='undefined')ctx.scrollChanged?.();
}
function resetChats(){hideChat();chatCaches.clear();chatScope='';$('chat-messages').replaceChildren?.();}
function renderChatMessage(i){
 const own=i.sender_uid===state.uid,session=sessionInfo.get(i.session)||{},user=noticeUser(i);
 const accountPhoto=typeof realDocument==='undefined'?null:realDocument.getElementById('account-avatar');
 const name=own?'我':i.sender_name||((session.session_type===1&&session.name)||user.name);
 const photo=avatarPhoto(i.sender_uid,name,[i.raw?.user?.avatar,i.raw?.user?.face,i.raw?.sender_face,i.raw?.face,i.sender_avatar_url,own?accountPhoto?.getAttribute('src'):session.session_type===1?session.avatar_url:''],{lookup:true});
 const status=i.status?({1:'已撤回',2:'被系统撤回',50:'图片已失效'}[i.status]||'消息状态 '+i.status):'';
 return `<article class="chat-message${own?' chat-own':''}" data-chat-key="${esc(i.type+':'+i.id)}"><time data-chat-time>${esc(replyTime(i.timestamp))}</time><div class="chat-row"><div class="chat-avatar">${user.uid?link('https://space.bilibili.com/'+user.uid,photo):photo}</div><div class="chat-content">${!own&&session.session_type===2?`<div class="chat-sender">${esc(name)}</div>`:''}<div class="chat-bubble"><div class="chat-text">${esc(i.text||'')}</div>${i.attachments?.length?`<div class="attachments">${attachments(i.attachments)}</div>`:''}</div><div class="reply-footer">${status?`<span class="notice-state">${esc(status)}</span>`:''}${notificationLink(i)}${rawAction(i)}</div></div></div></article>`;
}
function chatHistory(){
 const cache=currentChat;if(!cache)return;
 $('chat-error').textContent=cache.error;
 $('chat-progress').textContent=cache.initialized&&!cache.items.length?'没有符合条件的归档消息':`已加载 ${cache.items.length}${cache.hasTotal?' / '+cache.total:''} 条${cache.done?' · 已到最早记录':''}`;
 $('chat-more').hidden=cache.done&&!cache.error;$('chat-more').disabled=cache.busy;
 $('chat-more').textContent=cache.busy?'读取中…':cache.error?'重试加载':'加载更早消息';
 watchChat();
}
function watchChat(){
 chatObserver?.disconnect();const cache=currentChat;
 if(!cache||cache.busy||cache.done||cache.error||!workspaceActive()||typeof IntersectionObserver==='undefined')return;
 if(!chatObserver)chatObserver=new IntersectionObserver(entries=>{if(workspaceActive()&&entries.some(e=>e.isIntersecting))loadChatEarlier(true)},{root:chatScroll(),rootMargin:'240px 0px 0px 0px'});
 chatObserver.observe($('chat-sentinel'));
}
function chatAnchor(cache){
 const viewport=chatScroll().getBoundingClientRect(),rows=cache.element.children;
 let low=0,high=rows.length;
 while(low<high){
  const mid=Math.floor((low+high)/2);
  if(rows[mid].getBoundingClientRect().bottom>viewport.top)high=mid;
  else low=mid+1;
 }
 const node=rows[low];
 return node?{node,offset:node.getBoundingClientRect().top-viewport.top}:null;
}
function chatTimeDivider(node,item,previous){
 const gap=previous?Number(item.timestamp)-Number(previous.timestamp):Infinity;
 node.querySelector('[data-chat-time]').hidden=!!previous&&gap>=0&&gap<300;
}
function appendChat(cache,rows,offset){
 const first=!cache.initialized,scroll=chatScroll(),anchor=first?null:chatAnchor(cache),height=scroll.scrollHeight,top=scroll.scrollTop;
 const fresh=[];
 for(let n=0;n<rows.length;n++){
  const row=rows[n],key=row.type+':'+row.id;
  if(cache.seen.has(key))continue;
  cache.seen.add(key);cache.sequence.set(key,offset+n);fresh.push(row);
 }
 const compare=(a,b)=>{
  const x=Number(a.timestamp)||0,y=Number(b.timestamp)||0;
  return (x===0)!==(y===0)?(x===0?1:-1):x-y||cache.sequence.get(a.type+':'+a.id)-cache.sequence.get(b.type+':'+b.id);
 };
 const firstOld=cache.items[0];
 const canPrepend=fresh.length>0&&Number(firstOld?.timestamp)>0&&
  Number(cache.items.at(-1)?.timestamp)>0&&fresh.every(item=>Number(item.timestamp)>0&&Number(item.timestamp)<Number(firstOld.timestamp));
 if(canPrepend){
  fresh.sort(compare);
  const oldHead=cache.element.children[0];
  cache.element.insertAdjacentHTML('afterbegin',fresh.map(renderChatMessage).join(''));
  for(let n=0;n<fresh.length;n++){
   const node=cache.element.children[n];
   chatTimeDivider(node,fresh[n],fresh[n-1]);
   bindReplyImages(node);bindRawActions(node);
  }
  chatTimeDivider(oldHead,firstOld,fresh.at(-1));
  cache.items=fresh.concat(cache.items);
 }else if(fresh.length){
  cache.element.insertAdjacentHTML('beforeend',fresh.map(renderChatMessage).join(''));
  cache.items=cache.items.concat(fresh).sort(compare);
  const nodes=new Map([...cache.element.children].map(node=>[node.dataset.chatKey,node]));
  for(let n=0;n<cache.items.length;n++){
   const item=cache.items[n],node=nodes.get(item.type+':'+item.id);
   if(!node)continue;
   cache.element.appendChild(node);
   chatTimeDivider(node,item,cache.items[n-1]);
  }
  bindReplyImages(cache.element);bindRawActions(cache.element);
 }
 cache.initialized=true;
 if(first)scroll.scrollTop=scroll.scrollHeight;
 else if(anchor)scroll.scrollTop+=anchor.node.getBoundingClientRect().top-scroll.getBoundingClientRect().top-anchor.offset;
 else scroll.scrollTop=top+scroll.scrollHeight-height;
 cache.top=scroll.scrollTop;
}
async function loadChatEarlier(automatic=false){
 const cache=currentChat,epoch=chatEpoch,uid=state.uid;
 if(!cache||cache.busy||(cache.done&&!cache.refresh)||automatic&&(cache.error||!workspaceActive()))return;
 const current=()=>currentChat===cache&&epoch===chatEpoch&&uid===state.uid&&privateChat();
 const refreshing=!!cache.refresh,offset=refreshing?0:cache.next;
 const ticket={};cache.pending=ticket;cache.busy=true;cache.error='';chatHistory();
 const finish=loading(cache.initialized?$('chat-progress'):$('chat-messages'),cache.initialized?'inline':'skeleton','正在读取私信',current);
 try{
  const params=new URLSearchParams({type:requestType(),include_groups:'0',session:state.session,q:state.q,order:'new',offset:String(offset),limit:'50'});
  const data=await pageRequest('/api/messages/items?'+params,current,automatic);if(!data||!current())return;
  if(data.account_uid!==uid){await summary();return;}
  const rows=data.items||[],scroll=chatScroll(),top=scroll.scrollTop;
  const nearBottom=scroll.scrollHeight-top-scroll.clientHeight<=24;
  if(refreshing){
   cache.element.replaceChildren();
   cache.items=[];cache.seen.clear();cache.sequence.clear();cache.initialized=false;cache.refresh=false;
  }
  cache.total=data.total;cache.hasTotal=true;
  appendChat(cache,rows,offset);
  if(refreshing&&!nearBottom)scroll.scrollTop=top;
  cache.top=scroll.scrollTop;
  cache.next=offset+rows.length;cache.done=!rows.length||cache.next>=cache.total;state.offset=cache.next;state.total=cache.total;
 }catch(error){if(current())cache.error=error.message;}
 finally{finish?.();if(cache.pending===ticket){cache.pending=null;cache.busy=false;if(current())chatHistory();}}
}
function showChat(refresh=false){
 const scope=JSON.stringify([state.uid,'']);if(scope!==chatScope){chatCaches.clear();chatScope=scope;}
 rememberChat();++chatEpoch;chatObserver?.disconnect();
 let cache=chatCaches.get(state.session);
 if(!cache){
  if(chatCaches.size>=8)chatCaches.delete(chatCaches.keys().next().value);
  const element=document.createElement('div');element.className='conversation-messages';cache={element,items:[],seen:new Set(),sequence:new Map(),next:0,total:0,hasTotal:false,busy:false,done:false,error:'',initialized:false,top:0};chatCaches.set(state.session,cache);}
 chatCaches.delete(state.session);chatCaches.set(state.session,cache);
 const alreadyVisible=currentChat===cache;
 if(refresh){cache.refresh=true;cache.pending=null;cache.busy=false;}
 currentChat=cache;state.offset=cache.next;state.total=cache.total;const session=sessionInfo.get(state.session)||{};
 if(session.session_type===1&&!avatarURL(session.avatar_url))queueProfile(session.talker_id);
 $('chat-title').textContent=session.name||session.talker_id||state.session;
 $('chat-view').hidden=false;messageScrollRoot().classList?.add('chat-active');
 if(!alreadyVisible)$('chat-messages').replaceChildren(cache.element);
 chatScroll().scrollTop=cache.top;
 chatScroll().onscroll=()=>{if(currentChat===cache)cache.top=chatScroll().scrollTop;};
 $('chat-more').onclick=()=>loadChatEarlier();chatHistory();
 if(typeof ctx!=='undefined')ctx.scrollChanged?.();
 hydrateProfileAvatars(cache.element);
 return cache.initialized&&!cache.refresh?Promise.resolve():loadChatEarlier();
}
function sessionPreviewTime(value){const d=new Date(Number(value)*1000);if(!value||!Number.isFinite(d.getTime()))return '';const today=new Date();return d.toDateString()===today.toDateString()?d.toLocaleTimeString([],{hour:'2-digit',minute:'2-digit'}):d.toLocaleDateString();}

function renderNotice(i){
 const user=noticeUser(i),raw=i.raw||{},item=raw.item||{},category=categoryOf(i),privateMessage=['private','groups'].includes(category);
 const own=privateMessage&&i.sender_uid===state.uid;
 const avatar=user.photo,actor=user.label;
 const description=i.type==='mentions'?'@了我':category==='groups'?'发送了粉丝团消息':own?'我发送的私信':'发来的私信';
 const content=privateMessage?String(i.text||''):typeof item.source_content==='string'&&item.source_content.trim()?item.source_content:String(i.text||'');
 const quoted=!privateMessage?[item.target_reply_content,item.root_reply_content].find(s=>typeof s==='string'&&s.trim()&&s.trim()!==content.trim()):'';
 const quote=quoted?`<blockquote class="reply-quote">${esc(quoted)}</blockquote>`:'';
 const thumb=privateMessage?null:replyThumbnail(i),remaining=(i.attachments||[]).filter(a=>!thumb||imageURL(a.url)!==thumb.url);
 const thumbnail=thumb?`<a class="reply-thumbnail" href="${esc(thumb.url)}" target="_blank" rel="noopener noreferrer" referrerpolicy="no-referrer"><span class="archive-image-slot"><img data-reply-thumbnail data-archive-original="${esc(thumb.url)}" data-archive-src="${esc(thumb.src)}" alt="通知关联内容缩略图" loading="lazy" referrerpolicy="no-referrer"></span></a>`:'';
 const session=privateMessage?`<span class="notice-session">${esc(sessionNames.get(i.session)||i.session||names[category])}</span>`:'';
 const stateText=privateMessage&&i.status?`<span class="notice-state">${({1:'已撤回',2:'被系统撤回',50:'图片已失效'}[i.status]||'消息状态 '+esc(i.status))}</span>`:'';
 return `<article class="reply-notice notice-${esc(category)}${own?' mine':''}${thumb?' has-thumbnail':''}"><div class="reply-avatar">${avatar}</div><div class="reply-content"><div class="reply-heading"><strong>${actor}</strong><span>${esc(description)}</span>${session}${subjectHTML(i,'heading')}</div><div class="reply-body">${esc(content)}</div>${quote}${i.subject?.type==='video'?'':subjectHTML(i,'body')}${remaining.length?`<div class="attachments">${attachments(remaining)}</div>`:''}<div class="reply-footer"><time>${esc(replyTime(i.timestamp))}</time>${stateText}${notificationLink(i)}${rawAction(i)}</div></div>${thumbnail}</article>`;
}
function myCommentImages(detail){
 const pictures=(detail?.images||[]).map(imageURL).filter(Boolean).map(url=>({url}));
 return pictures.length?`<div class="attachments">${attachments(pictures,true)}</div>`:'';
}
function renderMyComment(i){
 const raw=i.raw?.item||{},detail=i.my_comment;
 const location=likeNativeDestination(raw.native_uri).destination||publicMessageRoute(i.url)||publicMessageRoute(raw.uri);
 const target=location?focusedComment(location,routeID(i.id)):'';
 const source=(i.sources||[]).map(type=>type==='replies'?'回复':'点赞').join('、');
 const pictures=myCommentImages(detail);
 const replies=i.reply_total!=null?`<span>回复 ${esc(i.reply_total)}</span>`:`<span>已归档回复 ${esc(i.reply_count||0)}</span>`;
 const likes=i.like_count!=null?`<span>${i.like_count_from_notification?'通知点赞':'被赞'} ${esc(i.like_count)}</span>`:'';
 const opened=i.reply_count?`<details data-my-comment="${esc(i.id)}"><summary>已归档的回复（${esc(i.reply_count)}）</summary><div data-my-comment-replies></div><small data-my-comment-error role="status"></small><button type="button" data-my-comment-more>加载回复</button></details>`:'';
 const text=i.text||(pictures?'':'评论正文未归档');
 return `<article class="reply-notice my-comment"><div class="reply-content"><div class="reply-heading">${subjectHTML(i,'my-comment',source)||commentSource(source)}${detail?'':'<span>仅有通知摘要</span>'}</div><div class="reply-body">${esc(text)}</div>${pictures}<div class="reply-footer">${detail?.timestamp?`<time>发布于 ${esc(replyTime(detail.timestamp))}</time>`:''}<time>最近互动 ${esc(replyTime(i.timestamp))}</time>${replies}${likes}${link(target,target.includes('#reply')?'查看评论':'查看主题')}</div>${opened}</div></article>`;
}
function renderItem(i){return i.type==='replies'?renderReply(i):i.type==='likes'?renderLikeNotice(i):i.type==='my_comments'?renderMyComment(i):renderNotice(i)}
function bindMyComments(root){
 for(const detail of root.querySelectorAll('[data-my-comment]')){
  if(detail.ontoggle)continue;
  const box=detail.querySelector('[data-my-comment-replies]'),button=detail.querySelector('[data-my-comment-more]'),error=detail.querySelector('[data-my-comment-error]');
  const uid=state.uid,epoch=state.request;
  let offset=0,busy=false,done=false;
  const current=()=>state.uid===uid&&state.request===epoch&&detail.isConnected!==false;
  const load=async()=>{
   if(!current()||busy||done)return;
   busy=true;button.disabled=true;error.textContent='';
   try{
    const params=new URLSearchParams({type:'replies',my_comment_rpid:detail.dataset.myComment,order:'old',offset:String(offset),limit:'50'});
    const data=await pageRequest('/api/messages/items?'+params,current,false);
    if(!data||!current()||data.account_uid!==uid)return;
    const items=data.items||[];
    appendHTML(box,items.map(renderReply).join(''));bindReplyImages(box);bindRawActions(box);
    offset+=items.length;done=!items.length||offset>=data.total;
    button.hidden=done;button.textContent='加载更多回复';
   }catch(e){if(current()){error.textContent=e.message;button.textContent='重试加载';}}
   finally{busy=false;if(current())button.disabled=false;}
  };
  detail.ontoggle=()=>{if(detail.open&&!offset)load();};
  button.onclick=load;
 }
}

function pagination(){
 const unit=grouping()==='flat'?'条':'项';
 $('page').textContent=`${privateSearch()?'全部会话搜索 · ':''}已加载 ${stream.loaded}${stream.hasTotal?' / '+state.total:''} ${unit}${grouping()!=='flat'&&stream.hasTotal?' · '+stream.totalItems+(state.type==='my_comments'?' 条评论':' 条消息'):''}${stream.done?' · 已全部加载':''}`;
 $('more').hidden=stream.done&&!stream.error;$('more').disabled=stream.busy||stream.request!==state.request;
 $('more').textContent=stream.busy?'读取中…':stream.error?'重试加载':'加载更多';
 if(stream.done||stream.error||likeView)unwatch($('sentinel'));else if(!stream.busy&&stream.request===state.request)watch($('sentinel'),()=>loadMore(true));
}
function groupBody(detail,cache){
 if(cache.content)return;
 const box=detail.querySelector('[data-group-body]');
 box.innerHTML='<div data-group-items></div><div class="group-more"><small data-group-progress></small><span data-group-error role="alert"></span><button data-group-more>加载更多</button></div><div data-group-sentinel class="load-sentinel" aria-hidden="true"></div>';
 cache.content=box.querySelector('[data-group-items]');cache.progress=box.querySelector('[data-group-progress]');cache.errorBox=box.querySelector('[data-group-error]');cache.more=box.querySelector('[data-group-more]');cache.sentinel=box.querySelector('[data-group-sentinel]');
 cache.more.onclick=()=>loadMessageGroup(detail);
}
function renderGroupBody(detail,cache){
 groupBody(detail,cache);
 cache.progress.textContent=`已加载 ${cache.items.length} / ${cache.total} 条${cache.done?' · 已全部加载':''}`;
 cache.errorBox.textContent=cache.error;cache.more.hidden=cache.done&&!cache.error;cache.more.disabled=cache.busy;
 cache.more.textContent=cache.busy?'读取中…':cache.error?'重试加载':'加载更多';
 if(detail.open&&!cache.busy&&!cache.done&&!cache.error)watch(cache.sentinel,()=>loadMessageGroup(detail,true));else unwatch(cache.sentinel);
}
async function loadMessageGroup(detail,automatic=false){
 const key=detail.dataset.groupKey,cache=groupCache.get(key);if(!cache||cache.busy||cache.done||automatic&&(cache.error||!detail.open||!workspaceActive()))return;
 const epoch=groupEpoch,uid=state.uid,type=state.type,mode=grouping(),offset=cache.next;
 const current=()=>epoch===groupEpoch&&uid===state.uid&&type===state.type&&mode===grouping()&&detail.isConnected!==false&&groupCache.get(key)===cache;
 cache.busy=true;cache.error='';renderGroupBody(detail,cache);
 const finish=loading(cache.loaded?cache.progress:cache.content,cache.loaded?'inline':'skeleton','正在读取分组消息',()=>current()&&detail.open);
 try{
  const params=new URLSearchParams({type,group_by:mode,group_key:key,q:state.q,order:detail.dataset.order||listOrder(),offset:String(offset),limit:'50'});
  const d=await pageRequest('/api/messages/items?'+params,()=>current()&&detail.open,automatic);if(!d||!current())return;
  if(d.account_uid!==uid){await summary();return;}
  const rows=d.items||[],fresh=[];
  for(const row of rows){const id=row.type+':'+row.id;if(!cache.seen.has(id)){cache.seen.add(id);fresh.push(row);cache.items.push(row);}}
  appendHTML(cache.content,fresh.map(renderItem).join(''));bindReplyImages(cache.content);bindRawActions(cache.content);hydrateSubjects();
  if(type==='my_comments')bindMyComments(cache.content);
  cache.total=d.total;cache.next=offset+rows.length;cache.done=!rows.length||cache.next>=d.total;cache.loaded=true;
 }catch(e){if(current())cache.error=e.message;}
 finally{finish?.();cache.busy=false;if(current())renderGroupBody(detail,cache);}
}
function renderGroups(groups,totalItems,append=false){
 if(!append)$('items').innerHTML='';
 appendHTML($('items'),groups.map(g=>{
  if(g.standalone)return renderItem(g.item);
  groupCache.set(g.key,{items:[],seen:new Set(),next:0,total:g.count,busy:false,loaded:false,done:false,error:''});
  const user=grouping()==='user';
  const avatar=user?avatarPhoto(g.key.slice(5),g.label,[g.avatar_url],{className:'group-avatar'}):'';
  const stats=user?`<small class="group-stats">${esc(g.identifier)} · ${esc(g.count)} ${state.type==='replies'?'次回复':'次 @'} · ${ranking()==='time'&&state.order==='old'?'最早':'最新'} ${esc(timeLabel(ranking()==='time'&&state.order==='old'?g.first_time:g.last_time))}</small>`:'';
  const cover=user?'':`<span class="group-cover">${g.subject?subjectHTML({subject:g.subject},'cover'):'<span class="group-cover-placeholder" aria-label="暂无封面"></span>'}</span>`;
  return `<details class="message-group" data-group-key="${esc(g.key)}"><summary>${avatar}<span class="group-heading"><b>${g.subject?subjectHTML({subject:g.subject},'group'):esc(g.label)}</b>${stats}</span>${cover}</summary><div data-group-body></div></details>`;
 }).join(''));
 $('items').querySelectorAll('[data-group-key]').forEach(detail=>{if(detail.dataset.groupBound)return;detail.dataset.groupBound='true';detail.ontoggle=()=>{const cache=groupCache.get(detail.dataset.groupKey);if(!cache)return;if(!detail.open){unwatch(cache.sentinel);pumpPages();}else if(!cache.loaded)return loadMessageGroup(detail);else renderGroupBody(detail,cache);};});
 bindReplyImages($('items'));bindRawActions($('items'));hydrateSubjects();
 if(state.type==='my_comments')bindMyComments($('items'));
}

const names={private:'私信',groups:'粉丝团',likes:'收到的赞',replies:'回复我的',mentions:'@我的'};
const esc=s=>String(s??'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
function categoryOf(i){return i.category||(i.type==='private'&&String(i.session||'').startsWith('group_')?'groups':i.type)}
function visibleSessions(sessions){return state.type==='private'?sessions.filter(s=>s.session_type===1||s.session_type===2).sort((a,b)=>Number(b.session_type===2)-Number(a.session_type===2)||(b.last_time??b.timestamp)-(a.last_time??a.timestamp)):[]}
function requestType(){return state.type==='private'&&state.session.startsWith('group_')&&!privateSearch()?'groups':state.type}
function sessionButtons(sessions){
 const list=visibleSessions(sessions),groups=list.filter(s=>s.session_type===2),users=list.filter(s=>s.session_type===1);
 const button=s=>`<button data-session="${esc(s.key)}" class="session-row">${s.session_type===1?profilePhoto(s.talker_id,s.name||s.talker_id,s.avatar_url):avatarPhoto('',s.name||s.talker_id,[s.avatar_url])}<span class="session-copy"><b title="${esc(s.name||s.talker_id)}">${s.session_type===2?'[粉丝团] ':''}${esc(s.name||s.talker_id)}</b><span title="${esc(s.last_message||'暂无归档消息')}">${esc(s.last_message||'暂无归档消息')}</span></span><time>${esc(sessionPreviewTime(s.last_time??s.timestamp))}</time></button>`;
 return '<button data-session="">所有私信</button>'+(groups.length?'<div class="session-heading">粉丝团</div>'+groups.map(button).join(''):'')+(users.length?'<div class="session-heading">普通私信</div>'+users.map(button).join(''):'');
}
let summaryRequest=0,expectedUid=null,latestSummary=null,renderedSessions=null;
const firstPages=new Map();
function rememberFirstPage(url,data){
 firstPages.delete(url);
 firstPages.set(url,{data,expires:Date.now()+30000});
 if(firstPages.size>8)firstPages.delete(firstPages.keys().next().value);
}
function clearAccount(uid){latestSummary=null;renderedSessions=null;firstPages.clear();likeOrder='new';myCommentOrder=defaultMyCommentOrder;privateSelection='';privateSessionTop=0;resetProfiles();resetChats();sessionInfo.clear();closeLikeView(false);likeCards.clear();$('like-detail').innerHTML='';sessionNames.clear();resetSubjects();expectedUid=String(uid||'');++summaryRequest;++state.request;invalidateGroups();groupingModes.replies=groupingModes.mentions=groupingModes.my_comments='flat';groupSorts.replies=groupSorts.mentions='time';$('header-stats').innerHTML='';state.uid='';state.session='';state.offset=0;state.total=0;$('items').innerHTML='';$('sessions').innerHTML='';renderAttachmentStats();$('error').textContent='';$('page').textContent='';$('more').disabled=true;return summary();}
function safeURL(value){try{const u=new URL(value,location.href);return ['http:','https:'].includes(u.protocol)?u.href:''}catch{return ''}}
function link(url,title){const href=safeURL(url);return href?`<a href="${esc(href)}" target="_blank" rel="noopener noreferrer" referrerpolicy="no-referrer">${title}</a>`:''}
async function get(url,signal){const response=await fetch(url,{signal,cache:'no-store'});if(!response.ok)throw new Error(`读取失败（${response.status}）`);return response.json()}
function renderAttachmentStats(){
 const status=$('status');
 const counts=latestSummary?.attachments_by_type?.[state.type];
 const saved=Number(counts?.saved)||0,pending=Number(counts?.pending)||0;
 status.hidden=!(saved||pending);
 status.title=status.hidden?'':'在桌面应用的消息归档处理附件';
 status.textContent=status.hidden?'':'附件 '+saved+' 已保存 / '+pending+' 待处理';
}
function controls(){
 renderAttachmentStats();
 $('my-comment-note').hidden=state.type!=='my_comments';
 $('session-pane').hidden=state.type!=='private';messageScrollRoot().classList?.[state.type==='private'?'add':'remove']('private-active');
document.querySelectorAll('#types button').forEach(b=>b.setAttribute('aria-pressed',String(b.dataset.type===state.type)));$('grouping').hidden=!Object.hasOwn(groupingModes,state.type);
 document.querySelectorAll('#grouping button').forEach(b=>{
  b.hidden=state.type==='my_comments'&&b.dataset.group==='user';
  b.setAttribute('aria-pressed',String(b.dataset.group===grouping()));
 });
 if(state.type==='private')$('sessions').querySelectorAll('button').forEach(b=>b.setAttribute('aria-pressed',String(b.dataset.session===(privateSearch()?'':state.session))));
 const noun=state.type==='replies'?'回复次数':'@次数';
 $('order').innerHTML=state.type==='my_comments'
  ?Object.entries(myCommentOrders).map(([value,label])=>`<option value="${value}">${label}</option>`).join('')
  :'<option value="new">时间倒序</option><option value="old">时间正序</option>'+(state.type==='likes'?'<option value="likes_desc">点赞最多</option><option value="likes_asc">点赞最少</option>':grouping()==='flat'?'':`<option value="count_desc">${noun}最多</option><option value="count_asc">${noun}最少</option>`);
 $('order').value=ranking()==='time'?listOrder():ranking();$('order').hidden=privateChat();if(typeof ctx!=='undefined')ctx.scrollChanged?.();
}
function attachments(list,knownImages=false){return (list||[]).map(a=>{
 const local=a.path && /^[a-f0-9]{32}$/.test(a.id)?`/api/messages/attachments/${a.id}`:'';
 const url=local||safeURL(a.url);if(!url)return '';
 const image=knownImages || (a.mime||'').startsWith('image/') || /\.(png|jpe?g|gif|webp|avif|bmp)(?:[?#]|$)/i.test(a.path||a.url);
 const audio=(a.mime||'').startsWith('audio/') || /\.(mp3|m4a|aac|wav|ogg|amr)(?:[?#]|$)/i.test(a.path||a.url);
 const content=image?`<span class="archive-image-slot"><img data-archive-src="${esc(url)}" data-archive-original="${esc(a.url)}" data-archive-placeholder loading="lazy" referrerpolicy="no-referrer" alt="消息图片"></span>`:esc(local?'下载附件':'打开原始附件');
 return `<div>${audio?`<audio controls preload="none" src="${esc(url)}"></audio>`:link(url,content)}${image&&local?`<div>${link(a.url,'原始图片链接')}</div>`:''}${a.error?`<div class="muted">${esc(a.error)}</div>`:''}</div>`;
 }).join('')}
function likeUserHTML(u){
 const uid=/^[0-9]+$/.test(u.uid||'') && !/^0+$/.test(u.uid||'')?u.uid:'';
 const label=u.nickname||(uid?'用户 '+uid:'匿名用户'),name=esc(label);
 const photo=avatarPhoto(uid,label,[u.avatar]);
 return `<div class="like-user"><div class="reply-avatar">${uid?link('https://space.bilibili.com/'+uid,photo):photo}</div><div class="like-user-content"><div><strong>${uid?link('https://space.bilibili.com/'+uid,name):name}</strong><span>赞了我</span></div><time>${esc(replyTime(u.like_time))}</time></div>${uid?link('https://space.bilibili.com/'+uid,'查看主页'):''}</div>`;
}
function likePageCurrent(detail,cache){return detail.likeStream===cache&&detail.dataset.request===cache.token&&detail.isConnected!==false&&cache.uid===state.uid;}
function likePageProgress(detail,cache){
 if(!likePageCurrent(detail,cache))return;
 const more=detail.querySelector('[data-like-more]'),sentinel=detail.querySelector('[data-like-sentinel]');
 detail.querySelector('[data-like-pager]').hidden=(cache.busy&&cache.next===0)||(cache.done&&!cache.error);
 detail.querySelector('[data-like-progress]').hidden=!cache.busy||cache.next===0;
 detail.querySelector('[data-like-error]').textContent=cache.error;
 more.hidden=cache.done&&!cache.error;more.disabled=cache.busy;more.textContent=cache.busy?'读取中…':cache.error?'重试加载':'加载更多';
 if(cache.done||cache.error)unwatch(sentinel);else if(!cache.busy)watch(sentinel,()=>loadMoreLikeUsers(detail,true));
}
function loadLikeUsers(detail){
 const old=detail.likeStream;if(old)unwatch(detail.querySelector('[data-like-sentinel]'));
 const token=String(Number(detail.dataset.request||0)+1);detail.dataset.request=token;
 const cache={token,uid:state.uid,q:detail.querySelector('[data-like-query]')?.value||'',order:detail.dataset.order||state.order,next:0,loaded:0,total:0,hasTotal:false,seen:new Set(),busy:false,done:false,error:''};detail.likeStream=cache;
 detail.querySelector('[data-like-content]').innerHTML='';
 detail.querySelector('[data-like-more]').onclick=()=>loadMoreLikeUsers(detail);
 return loadMoreLikeUsers(detail);
}
async function loadMoreLikeUsers(detail,automatic=false){
 const cache=detail.likeStream;if(!cache||!likePageCurrent(detail,cache)||cache.busy||cache.done||automatic&&(cache.error||!workspaceActive()))return;
 cache.busy=true;cache.error='';likePageProgress(detail,cache);
 const current=()=>likePageCurrent(detail,cache);
 const finish=loading(detail.querySelector(cache.next?'[data-like-progress]':'[data-like-content]'),cache.next?'inline':'skeleton','正在读取点赞明细',current);
 try{
  const params=new URLSearchParams({card_id:detail.dataset.likeId,q:cache.q,order:cache.order,offset:String(cache.next),limit:'50'});
  const data=await pageRequest('/api/messages/likes?'+params,current,automatic);if(!data||!current())return;
  if(data.account_uid!==cache.uid){await summary();return;}
  const notice=$('like-detail').querySelector('[data-like-notice]');if(notice)notice.textContent=`${data.notice||''}${data.notice?' · ':''}当前搜索 ${data.total} 位`;
  cache.hasTotal=true;cache.total=data.total;const rows=data.items||[],fresh=[];
  for(const user of rows){const key=user.key||(user.uid&&!/^0+$/.test(user.uid)?'uid:'+user.uid:JSON.stringify([user.nickname,user.like_time,user.raw]));if(!cache.seen.has(key)){cache.seen.add(key);fresh.push(user)}}
  const box=detail.querySelector('[data-like-content]');appendHTML(box,fresh.map(likeUserHTML).join(''));bindReplyImages(box);
  cache.loaded+=fresh.length;cache.next+=rows.length;cache.done=!rows.length||cache.next>=cache.total;detail.dataset.loaded='true';
  if(cache.done&&!cache.loaded)box.innerHTML='<p class="muted">暂无符合条件的用户明细，可在桌面继续采集补全。</p>';
 }catch(error){if(current())cache.error=error.message;}
 finally{finish?.();cache.busy=false;if(current())likePageProgress(detail,cache);}
}

function bindLikeDetails(){
 hydrateSubjects();bindReplyImages($('items'));bindLikeCards($('items'));
}
function render(items,append=false){if(!append){$('items').innerHTML='';likeCards.clear();}appendHTML($('items'),items.map(renderItem).join(''));bindLikeDetails();if(state.type==='my_comments')bindMyComments($('items'));}
async function loadMore(automatic=false,reuseFirstPage=false){
 const batch=stream,request=state.request,uid=state.uid,mode=grouping();
 const current=()=>stream===batch&&batch.request===state.request&&request===state.request&&uid===state.uid;
 if(!current()||batch.busy||batch.done||automatic&&(batch.error||!workspaceActive()||likeView))return;
 batch.busy=true;batch.error='';$('error').textContent='';$('page-error').textContent='';pagination();
 const first=batch.next===0;
 const finish=loading(first?$('items'):$('page'),first?($('items').children?.length?'overlay':'skeleton'):'inline',first?'正在读取消息':'正在加载下一页',current);
 try{
  const params=new URLSearchParams({type:requestType(),include_groups:state.type==='private'&&(!state.session||privateSearch())?'1':'0',session:privateSearch()?'':state.session,q:state.q,order:listOrder(),offset:String(batch.next),limit:String(state.limit)});
  if(mode!=='flat'){params.set('group_by',mode);params.set('sort',ranking());}
  const url=(mode==='flat'?'/api/messages/items?':'/api/messages/groups?')+params;
  const saved=first&&reuseFirstPage&&state.type!=='my_comments'?firstPages.get(url):null;
  const cached=saved&&saved.expires>Date.now()?saved.data:null;
  const d=cached??await pageRequest(url,()=>current()&&(!automatic||!likeView),automatic);
  if(!d||!current())return;
  if(d.account_uid!==state.uid){await summary();return;}
  if(first&&!cached)rememberFirstPage(url,d);
  state.total=d.total;batch.totalItems=d.total_items??d.total;batch.hasTotal=true;
  const rows=mode==='flat'?(d.items||[]):(d.groups||[]),fresh=[];
  for(const row of rows){const key=mode==='flat'?'item:'+row.type+':'+row.id:row.standalone?'item:'+row.item.type+':'+row.item.id:'group:'+row.key;if(!batch.seen.has(key)){batch.seen.add(key);fresh.push(row);}}
  const scroll=state.type==='private'?$('conversation-pane'):messageScrollRoot();
  const top=first?scroll?.scrollTop:undefined;
  if(first)$('items').textContent='';
  if(mode==='flat')render(fresh,true);else renderGroups(fresh,d.total_items,true);
  if(first&&top>0)scroll.scrollTop=top;
  batch.loaded+=fresh.length;batch.next+=rows.length;state.offset=batch.next;batch.done=!rows.length||batch.next>=d.total;
  if(batch.done&&!batch.loaded)appendHTML($('items'),'<p class="muted">没有符合条件的本地消息。</p>');
 }catch(e){if(current()){batch.error=e.message;$(first?'error':'page-error').textContent=e.message;}}
 finally{finish?.();batch.busy=false;if(current())pagination();}
}
function load(reuseFirstPage=false, retain=false){if(!retain||!privateChat())hideChat();closeLikeView(false);likeCards.clear();++state.request;invalidateGroups();state.offset=0;state.total=0;stream.request=state.request;controls();if(!retain)$('items').innerHTML='';$('error').textContent='';showSessions(reuseFirstPage);
 if(privateChat())return showChat(retain);
 return loadMore(false,reuseFirstPage);
}

function adoptMessageAccount(uid){
 if(state.uid===uid)return false;
 likeOrder='new';myCommentOrder=defaultMyCommentOrder;groupingModes.my_comments='flat';privateSelection='';privateSessionTop=0;
 latestSummary=null;renderedSessions=null;firstPages.clear();
 resetProfiles();resetChats();sessionInfo.clear();sessionNames.clear();
 closeLikeView(false);likeCards.clear();$('like-detail').innerHTML='';
 resetSubjects();++state.request;invalidateGroups();
 $('header-stats').innerHTML='';$('sessions').innerHTML='';$('status').textContent='';
 state.uid=uid;state.session='';state.offset=0;$('items').innerHTML='';renderAttachmentStats();
 return true;
}
async function summary(data,{eager=false,identity=null,retain=false}={}){
 const token=++summaryRequest;
 const initial=latestSummary?null:loading($('items'),'skeleton','正在读取消息',()=>token===summaryRequest);
 const sessionsLoading=loading($('sessions'),latestSummary?'overlay':'skeleton','正在读取会话',()=>token===summaryRequest);
 const current=uid=>token===summaryRequest&&(expectedUid===null||uid===expectedUid);
 const imageRoot=typeof ctx!=='undefined'&&!latestSummary?ctx.root:null;
 if(imageRoot)window.archiveImages?.hold(imageRoot);
 try{
 let firstPage=null;
 if(eager){
  const account=identity??await get('/api/messages/summary?identity=1');
  if(!current(account.account_uid))return;
  adoptMessageAccount(account.account_uid);
  initial?.();
  firstPage=load(true);
  // Give the page request a chance to start before issuing the heavier summary.
  await Promise.resolve();
  if(!current(account.account_uid))return;
 }
 const d=data&&Object.prototype.hasOwnProperty.call(data,'account_uid')?data:await get('/api/messages/summary');
 if(!current(d.account_uid))return;
 const switched=adoptMessageAccount(d.account_uid);
 resourceImages.clear();for(const[source,target]of Object.entries(d.resource_images||{})){if(target.startsWith('/api/archive-resources/'))resourceImages.set(source,target);}
 sessionNames.clear();sessionInfo.clear();for(const session of d.sessions||[]){sessionNames.set(session.key,session.name||session.talker_id||session.key);sessionInfo.set(session.key,session);if(session.session_type===1)rememberAvatar(session.talker_id,session.avatar_url);}
 if(state.type==='private'&&state.session&&!sessionInfo.has(state.session)){state.session='';privateSelection='';}
 renderHeaderStats(d);
 latestSummary=d;renderedSessions=null;
 initial?.();sessionsLoading?.();renderAttachmentStats();
 if(!eager||switched){firstPages.clear();await load(false,retain&&!switched);return;}
 if(state.type==='private')privateSessionTop=$('session-pane').scrollTop;
 showSessions(false);
 for(const root of [$('items'),$('like-detail'),currentChat?.element]){
  root?.querySelectorAll('img[data-archive-src]').forEach(img=>{
   const local=resourceImages.get(img.getAttribute('data-archive-original'));
   if(local&&typeof window!=='undefined')window.archiveImages?.setSource(img,local,true);
  });
 }
 if(imageRoot)window.archiveImages?.release(imageRoot);
 await firstPage;
 }catch(e){if(token===summaryRequest)$('error').textContent=e.message}
 finally{initial?.();sessionsLoading?.();if(imageRoot&&token===summaryRequest)window.archiveImages?.release(imageRoot);}
}

function showSessions(restoreScroll){
 const sessions=$('sessions');sessions.hidden=state.type!=='private';
 if(sessions.hidden||!latestSummary)return;
 const rebuilt=renderedSessions!==latestSummary;
 if(rebuilt){
  renderedSessions=latestSummary;
  disconnectProfileObservers();sessions.innerHTML=sessionButtons(latestSummary.sessions);
  bindReplyImages(sessions);
  sessions.querySelectorAll('button').forEach(button=>{
   button.setAttribute('aria-pressed',String(button.dataset.session===(privateSearch()?'':state.session)));
   button.onclick=()=>{
    privateSessionTop=$('session-pane').scrollTop;
    state.q='';$('q').value='';clearTimeout(timer);timer=undefined;
    state.session=button.dataset.session;privateSelection=state.session;
    return load();
   };
  });
 }
 if(rebuilt||restoreScroll)$('session-pane').scrollTop=privateSessionTop;
 hydrateProfileAvatars(sessions);
 globalThis.archiveImages?.resume(sessions);
}

function switchCategory(type){
 const returningToRoot=type===state.type;
 if(state.type==='private'){
  privateSelection=returningToRoot?'':state.session;
  privateSessionTop=returningToRoot?0:$('session-pane').scrollTop;
  disconnectProfileObservers();pauseProfileIndicators();profileQueue.clear();
 }
 if(timer!==undefined){clearTimeout(timer);timer=undefined;state.q=$('q').value;}
 if(returningToRoot){
  state.q='';$('q').value='';
  messageScrollRoot().scrollTop=0;
  if(type==='private')$('conversation-pane').scrollTop=0;
 }
 state.type=type;state.session=type==='private'?privateSelection:'';
 if(latestSummary||stream.request===state.request)return load(true);
 hideChat();controls();
 // An existing initial or account load will render the current category.
 return ready.then(()=>stream.request===state.request?undefined:summary(undefined,{eager:true}));
}
document.querySelectorAll('#types button').forEach(button=>button.onclick=()=>switchCategory(button.dataset.type));
document.querySelectorAll('#grouping button').forEach(b=>b.onclick=()=>{if(b.hidden||grouping()===b.dataset.group)return;
 if(timer!==undefined){clearTimeout(timer);timer=undefined;state.q=$('q').value;}
 groupingModes[state.type]=b.dataset.group;return load();});
let timer;$('q').oninput=()=>{clearTimeout(timer);timer=setTimeout(()=>{timer=undefined;state.q=$('q').value;state.offset=0;load(false,true)},200)};
$('order').onchange=()=>{
 const value=$('order').value;
 if(state.type==='my_comments'){
  if(!Object.hasOwn(myCommentOrders,value)){controls();return;}
  myCommentOrder=value;
  if(timer!==undefined){clearTimeout(timer);timer=undefined;state.q=$('q').value;}
 }
 else if(state.type==='likes'){if(!['new','old','likes_desc','likes_asc'].includes(value)){controls();return;}likeOrder=value;}
 else if(['new','old'].includes(value)){state.order=value;if(grouping()!=='flat')groupSorts[state.type]='time';}
 else if(grouping()!=='flat'&&['count_desc','count_asc'].includes(value))groupSorts[state.type]=value;
 else{controls();return;}
 state.offset=0;return load();
};
$('more').onclick=()=>loadMore();
function refresh(){resetSubjects();if(state.type==='private')privateSessionTop=$('session-pane').scrollTop;resetProfiles(true);if(!privateChat())resetChats();closeLikeView(false);++state.request;invalidateGroups();if(typeof window!=='undefined')window.refreshAccount?.();return summary(undefined,{retain:true});}
$('refresh').onclick=refresh;const ready=summary(undefined,{eager:true});

// END WORKSPACE LOGIC
return {ready,activate(){workspaceVisible=true;resumePages();watchChat();resumeProfiles();return ready.then(async()=>{
 // Check identity without rebuilding a loaded workspace. Only an account change reloads its data.
 const data=await get('/api/messages/summary?identity=1');if(data.account_uid!==state.uid)return summary(undefined,{eager:true,identity:data});
}).catch(e=>{$('error').textContent=e.message});},deactivate(){rememberChat();workspaceVisible=false;disconnectPageObservers();chatObserver?.disconnect();disconnectProfileObservers();pauseProfileIndicators();},accountChanged:clearAccount,refresh};
};
