// Preview fallbacks never write to the archive or fetch account data.
(() => {
 const attempted=new WeakMap();
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
  img.src=remote;
  return true;
 }
 function placeholder(img){
  const fallback=document.createElement('span');
  fallback.className='archive-image-placeholder';
  fallback.textContent='图片未能加载';
  img.replaceWith(fallback);
 }
 window.archiveImages={publicURL,retry,placeholder,reset:img=>attempted.delete(img)};
 document.addEventListener('error',event=>{
  const img=event.target;
  if(img?.tagName!=='IMG'||!img.hasAttribute('data-archive-original'))return;
  if(retry(img,img.getAttribute('data-archive-original'))){
   // Leave the normal avatar/thumbnail error handlers for the final failure.
   event.stopImmediatePropagation();
  }else if(img.hasAttribute('data-archive-avatar')){
   if(img.getAttribute('src')!=='/assets/noface.jpg')img.src='/assets/noface.jpg';
   else img.hidden=true;
  }else if(img.hasAttribute('data-archive-placeholder')){
   placeholder(img);
  }
 },true);
})();
