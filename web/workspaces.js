'use strict';
(() => {
 const toggle=document.getElementById('workspace-toggle'),menu=document.getElementById('workspace-menu');
 const choices={comments:document.getElementById('workspace-comments'),messages:document.getElementById('workspace-messages')};
 const roots={comments:document.getElementById('comment-workspace'),messages:document.getElementById('message-workspace')};
 const heads={comments:document.getElementById('comment-header'),messages:document.getElementById('message-header')};
 const instances={},scrolls={},routes={comments:new URL('/comments',location.href),messages:new URL('/messages',location.href)};
 let active=null,epoch=0;
 const which=()=>location.pathname.replace(/\/$/,'')==='/messages'?'messages':'comments';
 const saveScroll=key=>{scrolls[key]=[roots[key],...roots[key].querySelectorAll('*')].filter(e=>e.scrollHeight>e.clientHeight||e.scrollWidth>e.clientWidth).map(e=>[e,e.scrollTop,e.scrollLeft]);};
 const topButton=document.getElementById('back-to-top');
 function scrollTarget(){return active==='messages'?(roots.messages.classList?.contains('chat-active')?document.getElementById('m-chat-scroll'):roots.messages.classList?.contains('private-active')?document.getElementById('m-conversation-pane'):roots.messages):document.getElementById('comments');}
 function updateTopButton(){if(topButton)topButton.hidden=(scrollTarget()?.scrollTop||0)<=300;}
 for(const target of [roots.messages,document.getElementById('comments'),document.getElementById('m-chat-scroll'),document.getElementById('m-conversation-pane')])target?.addEventListener?.('scroll',updateTopButton,{passive:true});
 if(topButton)topButton.onclick=()=>{const target=scrollTarget();if(!target)return;const behavior=window.matchMedia?.('(prefers-reduced-motion: reduce)')?.matches?'instant':'smooth';if(target.scrollTo)target.scrollTo({top:0,behavior});else target.scrollTop=0;updateTopButton();};
 function context(key){return {scrollChanged:updateTopButton,root:roots[key],head:heads[key],active:()=>active===key,url:()=>routes[key],replaceHash(hash){const next=new URL(routes[key]);next.hash=hash;routes[key]=next;if(active===key)history.replaceState({workspace:key},'',next.pathname+next.search+next.hash);}};}
 function activate(key,push=false,fromHistory=false){
  if(!roots[key])return;
  if(active){window.archiveImages?.pause(roots[active]);saveScroll(active);instances[active]?.deactivate?.();if(!fromHistory)routes[active]=new URL(location.href);}
  active=key;const token=++epoch;
  if(push){const next=routes[key];history.pushState({workspace:key},'',next.pathname+next.search+next.hash);}
  else routes[key]=new URL(location.href);
  for(const name of Object.keys(roots)){roots[name].hidden=name!==key;heads[name].hidden=name!==key;}
  window.archiveImages?.resume(roots[key]);
  const name=key==='comments'?'稿件评论':'消息归档';document.title=name+' · BiliArchive';
  document.getElementById('product-name').textContent=name;
  document.getElementById('product-icon').textContent=key==='comments'?'评':'信';
  document.getElementById('product-subtitle').textContent=key==='comments'?'他人在我的稿件下发表的评论':'私信与互动通知 · 收集与归档';
  Object.entries(choices).forEach(([value,item])=>item.setAttribute('aria-checked',String(value===key)));
  closeMenu();updateTopButton();
  let pending;
  if(!instances[key]){instances[key]=(key==='comments'?window.createCommentsWorkspace:window.createMessagesWorkspace)(context(key));pending=instances[key].ready;}
  else pending=instances[key].activate?.(fromHistory);
  Promise.resolve(pending).catch(()=>{}).then(()=>requestAnimationFrame(()=>{if(token===epoch)for(const [el,top,left] of scrolls[key]||[]){if(el.isConnected!==false){el.scrollTop=top;el.scrollLeft=left;}}updateTopButton();}));
 }
 function closeMenu(){menu.hidden=true;toggle.setAttribute('aria-expanded','false');}
 function openMenu(){menu.hidden=false;toggle.setAttribute('aria-expanded','true');choices[active||'comments'].focus();}
 toggle.onclick=()=>menu.hidden?openMenu():closeMenu();
 Object.entries(choices).forEach(([key,item])=>item.onclick=()=>{if(key!==active)activate(key,true);closeMenu();toggle.focus();});
 toggle.onkeydown=e=>{if(e.key==='ArrowDown'||e.key==='ArrowUp'){e.preventDefault();openMenu();}};
 menu.onkeydown=e=>{const list=Object.values(choices);const index=list.indexOf(document.activeElement);if(e.key==='Escape'){e.preventDefault();closeMenu();toggle.focus();}else if(e.key==='ArrowDown'||e.key==='ArrowUp'){e.preventDefault();list[(index+(e.key==='ArrowDown'?1:-1)+list.length)%list.length].focus();}else if(e.key==='Tab'){closeMenu();}};
 document.addEventListener('click',e=>{if(!toggle.contains(e.target)&&!menu.contains(e.target))closeMenu();});
 window.addEventListener('accountchange',e=>{
  instances.comments?.accountChanged?.();
  instances.messages?.accountChanged?.(e.detail.uid);
 });
 window.addEventListener('popstate',()=>activate(which(),false,true));
 window.addEventListener('hashchange',()=>{if(active==='comments'){routes.comments=new URL(location.href);instances.comments?.activate?.(true);}});
 routes[which()]=new URL(location.href);activate(which());
})();
