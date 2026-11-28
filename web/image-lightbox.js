'use strict';
(() => {
 const dialog=document.getElementById('image-lightbox');
 if(!dialog)return;
 const viewport=dialog.querySelector('.lightbox-viewport');
 const status=dialog.querySelector('.lightbox-status');
 const closeButton=dialog.querySelector('[data-lightbox-close]');
 const originalLink=dialog.querySelector('[data-image-original]');
 const frameSelector='.archive-image-slot, .account-photo';
 const minScale=1,maxScale=8,closeDuration=180;
 let opener,image,pending,timeout,closeTimer,revision=0,scale=minScale,drag,pressTarget;

 function imageURL(value){
  if(!value)return '';
  try{
   const url=new URL(value.startsWith('//')?'https:'+value:value,location.href);
   if(!['https:','http:'].includes(url.protocol)||url.username||url.password)return '';
   return window.archiveImages?.publicURL(url.href)||url.href;
  }catch{return '';}
 }

 function imageFrame(event){
  const target=event.target;
  const frame=target.closest?.(frameSelector);
  if(frame)return frame;
  // Keyboard activation of a cover, attachment, or avatar link.
  if(event.detail===0&&target.matches?.('a, .account-photo'))return target.querySelector(frameSelector);
  return null;
 }

 function sizeImage(){
  if(!image)return;
  viewport.classList.toggle('zoomed',scale>minScale);
  const fit=Math.min(1,viewport.clientWidth/image.naturalWidth,viewport.clientHeight/image.naturalHeight);
  image.width=Math.max(1,Math.round(image.naturalWidth*fit*scale));
  image.height=Math.max(1,Math.round(image.naturalHeight*fit*scale));
 }

 function zoom(nextScale,point){
  if(!image||dialog.hasAttribute('data-closing'))return;
  const clamped=Math.max(minScale,Math.min(maxScale,nextScale));
  if(clamped===scale)return;
  const rect=viewport.getBoundingClientRect();
  const x=point?point.clientX-rect.left:viewport.clientWidth/2;
  const y=point?point.clientY-rect.top:viewport.clientHeight/2;
  const fraction=(position,size)=>Math.max(0,Math.min(1,position/size));
  const horizontal=fraction(viewport.scrollLeft+x-image.offsetLeft,image.width);
  const vertical=fraction(viewport.scrollTop+y-image.offsetTop,image.height);
  scale=clamped;
  sizeImage();
  viewport.scrollLeft=image.offsetLeft+horizontal*image.width-x;
  viewport.scrollTop=image.offsetTop+vertical*image.height-y;
 }

 function stopPending(abort){
  clearTimeout(timeout);
  if(!pending)return;
  pending.onload=pending.onerror=null;
  if(abort)pending.removeAttribute('src');
  pending=null;
 }

 function showStatus(message,loading=false){
  status.textContent=message;
  status.hidden=!message;
  status.toggleAttribute('data-lightbox-loading',loading);
  viewport.setAttribute('aria-busy',String(loading));
 }

 function loadImage(sources, alt, token){
  if(!dialog.open||token!==revision)return;
  const source=sources.shift();
  if(!source){showStatus('图片未能加载');return;}
  const candidate=new Image();
  candidate.alt=alt;
  candidate.draggable=false;
  candidate.referrerPolicy='no-referrer';
  candidate.decoding='async';
  pending=candidate;
  function failed(){
   if(candidate!==pending)return;
   stopPending(true);
   loadImage(sources,alt,token);
  }
  timeout=setTimeout(failed,8000);
  candidate.onerror=failed;
  candidate.onload=()=>{
   if(candidate!==pending||!dialog.open||token!==revision)return;
   if(!candidate.naturalWidth||!candidate.naturalHeight){failed();return;}
   stopPending(false);
   image=candidate;
   viewport.append(image);
   showStatus('');
   sizeImage();
  };
  candidate.src=source;
 }

 function open(frame, target){
  const thumb=[...frame.querySelectorAll('img')].find(img=>!img.closest('[hidden]'));
  const source=imageURL(thumb?.dataset.archiveSrc||frame.dataset.lightboxSource);
  const original=imageURL(thumb?.dataset.archiveOriginal||thumb?.dataset.lightboxOriginal||frame.dataset.lightboxOriginal);
  const displayed=imageURL(thumb?.currentSrc||thumb?.getAttribute('src'));
  const local=[source,displayed].filter(url=>url&&new URL(url).origin===location.origin);
  const sources=[...new Set([...local,original,source,displayed].filter(Boolean))];
  if(!sources.length)return false;
  opener=frame.closest('a,button')||target.closest?.('[tabindex]')||frame;
  if(!opener.matches('a,button,[tabindex]'))opener.tabIndex=-1;
  scale=minScale;
  originalLink.href=original||sources[0];
  originalLink.hidden=false;
  showStatus('正在加载图片…',true);
  dialog.showModal();
  closeButton.focus();
  loadImage(sources,thumb?.alt||'原图',++revision);
  return true;
 }

 document.addEventListener('click',event=>{
  if(dialog.open||opener||event.defaultPrevented||event.button!==0||event.ctrlKey||event.metaKey||event.shiftKey||event.altKey)return;
  const frame=imageFrame(event);
  if(!frame||!open(frame,event.target))return;
  event.preventDefault();
  event.stopPropagation();
 },true);

 function close(){
  if(!dialog.open||dialog.hasAttribute('data-closing'))return;
  ++revision;
  stopPending(true);
  if(window.matchMedia?.('(prefers-reduced-motion: reduce)').matches){dialog.close();return;}
  dialog.setAttribute('data-closing','');
  closeTimer=setTimeout(()=>dialog.close(),closeDuration+50);
 }

 closeButton.onclick=close;
 window.addEventListener('resize',sizeImage);
 window.addEventListener('accountchange',()=>{if(dialog.open)dialog.close();});
 dialog.addEventListener('cancel',event=>{event.preventDefault();close();});
 dialog.addEventListener('animationend',event=>{
  if(event.target===dialog&&event.animationName==='lightbox-fade-out'&&dialog.hasAttribute('data-closing'))dialog.close();
 });
 dialog.addEventListener('keydown',event=>{
  const factor=event.key==='+'||event.key==='='?1.25:event.key==='-'?0.8:0;
  if(!event.ctrlKey&&!event.metaKey&&!event.altKey&&(factor||event.key==='0')){
   event.preventDefault();
   zoom(factor?scale*factor:minScale);
  }
 });
 const isBackdrop=target=>target===dialog||target===viewport||target.classList?.contains('lightbox-stage')||target.classList?.contains('lightbox-panel');
 dialog.addEventListener('pointerdown',event=>{pressTarget=event.button===0?event.target:null;});
 dialog.addEventListener('click',event=>{
  if(pressTarget&&isBackdrop(pressTarget)&&isBackdrop(event.target)&&!drag?.moved)close();
  if(event.target===image&&!drag?.moved)zoom(scale===minScale?2:minScale,event);
  pressTarget=null;
  drag=null;
 });
 dialog.addEventListener('close',()=>{
  ++revision;
  clearTimeout(closeTimer);
  stopPending(true);
  dialog.removeAttribute('data-closing');
  viewport.replaceChildren();
  image=null;drag=null;scale=minScale;pressTarget=null;
  viewport.classList.remove('dragging','zoomed');
  showStatus('');
  originalLink.hidden=true;
  originalLink.removeAttribute('href');
  if(opener?.isConnected)opener.focus({preventScroll:true});
  opener=null;
 });

 viewport.addEventListener('wheel',event=>{
  event.preventDefault();
  if(event.deltaY===0)return;
  const unit=event.deltaMode===1?16:event.deltaMode===2?viewport.clientHeight:1;
  const delta=Math.max(-800,Math.min(800,event.deltaY*unit));
  zoom(scale*Math.exp(-delta*0.002),event);
 },{passive:false});
 viewport.addEventListener('pointerdown',event=>{
  drag=null;
  if(event.button!==0||event.pointerType==='touch'||!image||scale<=minScale)return;
  drag={id:event.pointerId,x:event.clientX,y:event.clientY,left:viewport.scrollLeft,top:viewport.scrollTop,moved:false};
 });
 viewport.addEventListener('pointermove',event=>{
  if(drag?.id!==event.pointerId||!(event.buttons&1))return;
  const x=event.clientX-drag.x,y=event.clientY-drag.y;
  if(!drag.moved){
   if(Math.hypot(x,y)<5)return;
   drag.moved=true;
   viewport.setPointerCapture(event.pointerId);
   viewport.classList.add('dragging');
  }
  viewport.scrollLeft=drag.left-x;
  viewport.scrollTop=drag.top-y;
 });
 viewport.addEventListener('lostpointercapture',()=>viewport.classList.remove('dragging'));
 viewport.addEventListener('pointercancel',()=>{
  drag=null;
  pressTarget=null;
  viewport.classList.remove('dragging');
 });
})();
