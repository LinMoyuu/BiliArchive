// Preview fallbacks never write to the archive or fetch account data.
(() => {
 const attempted=new WeakMap();
 const finished=new WeakMap();
 const indicators=new WeakMap();
 const queued=new Set();
 const active=new Map();
 const holds=new Map();
 const rootedObservers=new WeakMap();
 const observed=new WeakMap();
 const selector='img[data-archive-src]';
 const maxActive=4;
 let framePending=false;

 function eligible(img){
  return img.isConnected&&img.getAttribute('data-archive-src')&&!img.closest('[hidden]');
 }
 function schedule(){
  if(framePending)return;
  framePending=true;
  const run=()=>{framePending=false;pump();};
  if(window.requestAnimationFrame)window.requestAnimationFrame(run);
  else setTimeout(run,0);
 }
 function visit(node,handler){
  if(node?.matches?.(selector))handler(node);
  node?.querySelectorAll?.(selector).forEach(handler);
 }
 function track(img){
  const source=img.getAttribute('data-archive-src');
  if(!source||active.has(img)||finished.get(img)===source)return;
  if(typeof IntersectionObserver==='undefined'){queue(img);schedule();return;}
  const root=img.closest('[data-archive-scroll-root]');
  let observer=root?rootedObservers.get(root):defaultObserver;
  if(!observer&&root){observer=createObserver(root);rootedObservers.set(root,observer);}
  if(observed.get(img)===observer)return;
  unobserve(img);
  observed.set(img,observer);
  observer.observe(img);
 }
 function unobserve(img){
  observed.get(img)?.unobserve(img);
  observed.delete(img);
 }
 function clearIndicator(img){
  const indicator=indicators.get(img);
  if(!indicator)return;
  clearTimeout(indicator.timer);
  indicator.frame.removeAttribute('data-image-loading');
  indicators.delete(img);
 }
 function stopIndicator(img, reason='image'){
  const indicator=indicators.get(img);
  if(!indicator)return;
  indicator.reasons.delete(reason);
  if(!indicator.reasons.size)clearIndicator(img);
 }
 function startIndicator(img, reason='image'){
  const existing=indicators.get(img);
  if(existing){existing.reasons.add(reason);return;}
  const frame=img.closest?.('.archive-image-slot, .account-photo');
  if(!frame)return;
  const timer=setTimeout(()=>{
   if(img.isConnected)frame.setAttribute('data-image-loading','');
  },120);
  indicators.set(img,{frame,timer,reasons:new Set([reason])});
 }
 function queue(img){
  if(!eligible(img))return;
  queued.add(img);
  startIndicator(img);
 }
 function finish(img, state){
  const job=active.get(img);
  if(!job)return;
  clearTimeout(job.timer);
  stopIndicator(img);
  active.delete(img);
  finished.set(img,job.source);
  img.setAttribute('data-image-state',state);
  schedule();
 }
 function cancel(img, preserveProfile=false){
  if(preserveProfile===true)stopIndicator(img);
  else clearIndicator(img);
  const job=active.get(img);
  if(job){
   clearTimeout(job.timer);
   active.delete(img);
   attempted.delete(img);
   img.removeAttribute('src');
   img.removeAttribute('data-image-state');
  }
  unobserve(img);
  queued.delete(img);
 }
 function arm(img,job){
  clearTimeout(job.timer);
  job.timer=setTimeout(()=>{
   if(active.get(img)!==job)return;
   if(eligible(img)&&retry(img,img.getAttribute('data-archive-original')))return;
   finish(img,'fallback');
   img.removeAttribute('src');
   if(img.hasAttribute('data-archive-avatar')||img.hasAttribute('data-reply-avatar'))img.src='/assets/noface.jpg';
   else placeholder(img);
  },8000);
 }
 function pump(){
  if(document.hidden)return;
  for(const img of queued){
   if(active.size>=maxActive)break;
   if([...holds.keys()].some(root=>root.contains(img)))continue;
   queued.delete(img);
   if(!eligible(img)){stopIndicator(img);continue;}
   unobserve(img);
   const source=img.getAttribute('data-archive-src');
   const job={source};
   active.set(img,job);
   img.setAttribute('data-image-state','loading');
   img.decoding='async';
   img.fetchPriority='low';
   img.referrerPolicy='no-referrer';
   // IntersectionObserver already decides when the image is needed.
   img.loading='eager';
   startIndicator(img);
   arm(img,job);
   img.src=publicURL(source)||source;
   if(img.complete&&img.naturalWidth>0)finish(img,'loaded');
  }
 }
 function createObserver(root=null){
  const observer=new IntersectionObserver(entries=>{
   for(const entry of entries){
    if(observed.get(entry.target)!==observer)continue;
    if(entry.isIntersecting)queue(entry.target);
    else if(queued.delete(entry.target))stopIndicator(entry.target);
   }
   schedule();
  },{root,rootMargin:'120px'});
  return observer;
 }
 const defaultObserver=typeof IntersectionObserver==='undefined'?null:createObserver();
 function resume(root){visit(root,track);schedule();}
 function pause(root){visit(root,cancel);}
 function hold(root){
  clearTimeout(holds.get(root));
  // Give the local URL map a short head start, never delaying the text or indefinitely holding images.
  holds.set(root,setTimeout(()=>release(root),1200));
 }
 function release(root){
  clearTimeout(holds.get(root));
  holds.delete(root);
  schedule();
 }
 function setSource(img,source,pendingOnly=false){
  if(!img.hasAttribute('data-archive-src')){
   if(!pendingOnly)img.src=source;
   return;
  }
  const previous=img.getAttribute('data-archive-src');
  if(previous===source||pendingOnly&&(active.has(img)||finished.has(img)))return;
  if(queued.has(img)){
   // Keep the waiting image in the same queue position with the same indicator.
   img.setAttribute('data-archive-src',source);
   return;
  }
  // Transfer the frame's indicator from profile lookup to the new image.
  const preserveProfile=indicators.get(img)?.reasons.has('profile');
  cancel(img,preserveProfile);
  if(preserveProfile)startIndicator(img);
  finished.delete(img);
  attempted.delete(img);
  img.removeAttribute('src');
  img.removeAttribute('data-image-state');
  img.setAttribute('data-archive-src',source);
  track(img);
 }
 if(typeof MutationObserver!=='undefined'){
  new MutationObserver(records=>{
   for(const record of records){
    for(const node of record.removedNodes)if(!node.isConnected)visit(node,cancel);
    for(const node of record.addedNodes)if(node.isConnected)visit(node,track);
   }
   schedule();
  }).observe(document.body,{subtree:true,childList:true});
 }
 document.addEventListener('visibilitychange',schedule);
 document.addEventListener('load',event=>{
  const img=event.target;
  if(active.has(img)&&img.complete&&img.currentSrc===img.src)finish(img,'loaded');
 },true);

 function publicURL(value){
  if(typeof value!=='string'||!value.trim())return '';
  const source=value.trim();
  try{
   const url=new URL(source.startsWith('//')?'https:'+source:source.replace(/^http:\/\//i,'https://'));
   if(url.protocol!=='https:'||url.username||url.password||url.port)return '';
   return url.hostname==='hdslb.com'||url.hostname.endsWith('.hdslb.com')?url.href:'';
  }catch{return '';}
 }
 function retry(img,source){
  const remote=publicURL(source);
  if(!remote||img.src===remote||attempted.get(img)===remote)return false;
  attempted.set(img,remote);
  img.referrerPolicy='no-referrer';
  const job=active.get(img);
  if(job)arm(img,job);
  img.src=remote;
  return true;
 }
 function placeholder(img){
  clearIndicator(img);
  const fallback=document.createElement('span');
  fallback.className='archive-image-placeholder';
  const frame=img.closest?.('.archive-image-slot');
  if(frame){
   frame.setAttribute('data-lightbox-source',img.getAttribute('data-archive-src')||'');
   frame.setAttribute('data-lightbox-original',img.getAttribute('data-archive-original')||'');
  }
  if(img.hasAttribute('data-archive-placeholder'))fallback.setAttribute?.('data-image-slot','');
  fallback.textContent='图片未能加载';
  img.replaceWith(fallback);
 }
 window.archiveImages={publicURL,retry,placeholder,resume,pause,hold,release,setSource,startIndicator,stopIndicator,reset:img=>attempted.delete(img)};
 document.addEventListener('error',event=>{
  const img=event.target;
  if(img?.tagName!=='IMG'||!img.hasAttribute('data-archive-original'))return;
  if(img.hasAttribute('data-archive-src')&&(!img.getAttribute('src')||!eligible(img)))return;
  if(retry(img,img.getAttribute('data-archive-original'))){
   // Leave the normal avatar/thumbnail error handlers for the final failure.
   event.stopImmediatePropagation();
   return;
  }
  finish(img,'fallback');
  if(img.hasAttribute('data-archive-avatar')){
   if(img.getAttribute('src')!=='/assets/noface.jpg')img.src='/assets/noface.jpg';
   else img.hidden=true;
  }else if(img.hasAttribute('data-archive-placeholder')){
   placeholder(img);
  }
 },true);
 resume(document);
})();
