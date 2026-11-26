'use strict';
(() => {
 const button=document.getElementById('account-toggle'),menu=document.getElementById('account-menu');
 const avatar=document.getElementById('account-avatar'),fallback=document.getElementById('account-fallback');
 document.getElementById('account-default-avatar').onerror=function(){this.hidden=true;};
 let lastUid,lastScope,avatarUrl='',request=0;
 const close=()=>{menu.hidden=true;button.setAttribute('aria-expanded','false');};
 function showAvatar(url,original,scope){
  let safe='';
  try{
   const parsed=new URL(url,location.href);
   if(parsed.origin===location.origin&&parsed.pathname.startsWith('/api/archive-resources/'))safe=parsed.href;
   else if(window.archiveOffline)safe=window.archiveImages?.publicURL(url)||'';
   else if(parsed.protocol==='https:')safe=parsed.href;
  }catch{}
  const remote=window.archiveImages?.publicURL(original)||'';
  const key=JSON.stringify([safe,remote,scope]);
  if(key===avatarUrl)return;
  window.archiveImages?.reset(avatar);
  window.archiveImages?.stopIndicator?.(avatar);
  avatarUrl=key;avatar.hidden=true;fallback.hidden=false;avatar.removeAttribute('src');
  const photo=avatar.parentElement;
  photo?.removeAttribute('data-image-state');
  avatar.onload=()=>{if(avatarUrl===key){window.archiveImages?.stopIndicator?.(avatar);photo?.setAttribute('data-image-state','loaded');avatar.hidden=false;fallback.hidden=true;}};
  avatar.onerror=()=>{
   if(avatarUrl!==key||window.archiveImages?.retry(avatar,remote))return;
   window.archiveImages?.stopIndicator?.(avatar);
   photo?.setAttribute('data-image-state','fallback');
   avatar.hidden=true;fallback.hidden=false;
  };
  if(safe||remote){
   window.archiveImages?.startIndicator?.(avatar);
   avatar.src=safe||remote;
  }
 }
 async function refresh(){
  const token=++request;
  try{
   const response=await fetch('/api/account',{cache:'no-store'});if(!response.ok)throw new Error('account unavailable');
   const data=await response.json();if(token!==request)return;
   const offline=data.mode==='offline',logged=!offline&&data.logged_in===true && /^[0-9]+$/.test(data.uid||'') && data.uid!=='0';
   const selected=offline||logged,uid=selected?data.uid:'';
   const scope=[data.mode||'online',data.library_id||'',uid].join(':');
   const scopeChanged=lastScope!==undefined&&lastScope!==scope;
   window.archiveOffline=offline;
   if(lastScope!==undefined&&lastScope!==scope&&typeof location.reload==='function'){location.reload();return;}
   lastScope=scope;
   if(uid!==lastUid||scopeChanged){close();if(lastUid!==undefined)window.dispatchEvent(new CustomEvent('accountchange',{detail:{uid}}));lastUid=uid;}
   const name=selected?(data.name||uid)+(offline?' · 本地':''):'未登录';
   document.getElementById('account-name').textContent=name;
   const menuName=document.getElementById('account-menu-name');
   menuName.textContent=name;menuName.hidden=!selected;
   button.setAttribute('data-mode',offline?'offline':'online');
   document.getElementById('account-uid').textContent=selected?'UID '+uid+(offline?' · 本地资料':''):'请在桌面应用右上角登录';
   button.title=selected?name+' · UID '+uid:'请在桌面应用右上角登录';
   showAvatar(selected?data.avatar_url:'',offline?data.avatar_fallback_url:'',scope);
  }catch{ /* Keep the last verified account through transient network errors. */ }
 }
 button.onclick=()=>{menu.hidden=!menu.hidden;button.setAttribute('aria-expanded',String(!menu.hidden));if(!menu.hidden)menu.focus();};
 button.onkeydown=e=>{if(e.key==='ArrowDown'){e.preventDefault();menu.hidden=false;button.setAttribute('aria-expanded','true');menu.focus();}};
 menu.onkeydown=e=>{if(e.key==='Escape'){e.preventDefault();close();button.focus();}else if(e.key==='Tab')close();};
 document.addEventListener('click',e=>{if(!button.contains(e.target)&&!menu.contains(e.target))close();});
 window.addEventListener('focus',refresh);
 document.addEventListener('visibilitychange',()=>{if(!document.hidden)refresh();});
 window.setInterval(()=>{if(!document.hidden)refresh();},15000);
 window.refreshAccount=refresh;refresh();
})();
