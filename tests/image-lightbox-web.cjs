const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const vm=require('node:vm');

const root=path.join(__dirname,'..');
const read=file=>fs.readFileSync(path.join(root,file),'utf8');
class Node {
 constructor(){this.listeners={};this.attributes={};this.children=[];this.dataset={};this.isConnected=true;this.classes=new Set();}
 addEventListener(name,handler){this.listeners[name]=handler;}
 setAttribute(name,value){this.attributes[name]=value;}
 removeAttribute(name){delete this.attributes[name];delete this[name];}
 toggleAttribute(name,on){if(on)this.attributes[name]='';else delete this.attributes[name];}
 getAttribute(name){return this.attributes[name];}
 hasAttribute(name){return Object.hasOwn(this.attributes,name);}
 focus(){focused=this;}
 append(child){this.children.push(child);}
 replaceChildren(){this.children=[];}
}
let focused;
const nodes=new Map();
const get=selector=>{if(!nodes.has(selector))nodes.set(selector,new Node());return nodes.get(selector)};
const dialog=get('image-lightbox'),viewport=get('.lightbox-viewport');
dialog.querySelector=get;
dialog.showModal=()=>{dialog.open=true;};
dialog.close=()=>{dialog.open=false;dialog.listeners.close();};
Object.assign(viewport,{clientWidth:800,clientHeight:600,scrollWidth:800,scrollHeight:600,scrollLeft:0,scrollTop:0,
 classList:{toggle(name,on){if(on)viewport.classes.add(name);else viewport.classes.delete(name);},add(name){viewport.classes.add(name);},remove(name){viewport.classes.delete(name);}},
 getBoundingClientRect(){return {left:0,top:0};},
 setPointerCapture(id){viewport.captured=id;}
});
const document=new Node(),window=new Node(),loads=[];
document.getElementById=get;
class Image extends Node {
 set src(src){this.url=src;loads.push(this);}
 get src(){return this.url;}
 get offsetLeft(){return Math.max(0,(viewport.clientWidth-this.width)/2);}
 get offsetTop(){return Math.max(0,(viewport.clientHeight-this.height)/2);}
}
vm.runInNewContext(read('web/image-lightbox.js'),{window,document,Image,URL,location:new URL('http://localhost/messages'),setTimeout,clearTimeout});

const image=(source,original)=>Object.assign(new Node(),{dataset:{archiveSrc:source,archiveOriginal:original},closest(){return null;}});
const anchor=new Node();
anchor.matches=()=>true;
const frame=thumb=>Object.assign(new Node(),{querySelectorAll:()=>thumb?[thumb]:[],closest:()=>anchor});
const click=(imgFrame,extra={})=>{
 const event={button:0,detail:1,target:{closest:()=>imgFrame},preventDefault(){this.prevented=true;},stopPropagation(){this.stopped=true;},...extra};
 document.listeners.click(event);
 return event;
};
const linkedFrame=frame(image('/api/archive-resources/local','https://i0.hdslb.com/remote.jpg'));
assert.equal(click(linkedFrame,{ctrlKey:true}).prevented,undefined);
assert.equal(loads.length,0);
const event=click(linkedFrame);
assert(dialog.open&&event.stopped&&event.prevented);
assert.equal(loads[0].src,'http://localhost/api/archive-resources/local');
assert.equal(loads[0].referrerPolicy,'no-referrer');
loads[0].onerror();
assert.equal(loads[1].src,'https://i0.hdslb.com/remote.jpg');
assert.equal(get('[data-image-original]').href,loads[1].src);
Object.assign(loads[1],{naturalWidth:1600,naturalHeight:80});
loads[1].onload();
assert.equal(viewport.children[0].width,800);
assert.equal(viewport.children[0].height,40);
assert.equal(get('.lightbox-status').hidden,true);
const picture=viewport.children[0];
const point={clientX:200,clientY:300};
const wheel=(deltaY,deltaMode=0)=>{
 const event={...point,deltaY,deltaMode,preventDefault(){this.prevented=true;}};
 viewport.listeners.wheel(event);
 assert.equal(event.prevented,true);
};
wheel(-50);
assert.equal(picture.width,884);
assert.equal(viewport.scrollLeft,21,'the pixel under the cursor stays fixed');
wheel(50);
assert.equal(picture.width,800);
dialog.listeners.click({target:picture,...point});
assert.equal(picture.width,1600);
assert.equal(viewport.scrollLeft,200);
const press={target:picture,button:0,pointerType:'mouse',pointerId:1,clientX:100,clientY:80};
viewport.listeners.pointerdown(press);
dialog.listeners.pointerdown(press);
viewport.listeners.pointermove({pointerId:1,buttons:1,clientX:98,clientY:80});
assert.equal(viewport.captured,undefined);
viewport.listeners.pointermove({pointerId:1,buttons:1,clientX:30,clientY:80});
assert.equal(viewport.scrollLeft,270);
viewport.listeners.lostpointercapture();
dialog.listeners.click({target:picture,...point});
assert.equal(viewport.classes.has('dragging'),false);
assert.equal(picture.width,1600,'ending a drag should not toggle zoom');
dialog.listeners.click({target:picture,...point});
assert.equal(picture.width,800);
wheel(-100,1);
wheel(-100,1);
assert.equal(picture.width,6400);
wheel(100,1);
wheel(100,1);
assert.equal(picture.width,800);
dialog.listeners.pointerdown({target:viewport,button:0});
dialog.listeners.click({target:viewport});
assert.equal(dialog.open,true,'keep the modal open while closing animates');
assert.equal(dialog.hasAttribute('data-closing'),true);
dialog.listeners.animationend({target:picture,animationName:'lightbox-shrink'});
assert.equal(dialog.open,true);
dialog.listeners.animationend({target:dialog,animationName:'lightbox-fade-out'});
assert.equal(dialog.open,false);
assert.equal(focused,anchor);
assert.equal(viewport.children.length,0);

const badFrame=frame(image('javascript:alert(1)','data:image/png;base64,a'));
assert.equal(click(badFrame).prevented,undefined);
const failedFrame=frame();
failedFrame.dataset.lightboxOriginal='https://i0.hdslb.com/retry.jpg';
click(failedFrame);
loads[2].onerror();
assert.equal(get('.lightbox-status').textContent,'图片未能加载');
window.matchMedia=()=>({matches:true});
const cancel={preventDefault(){this.prevented=true;}};
dialog.listeners.cancel(cancel);
assert.equal(cancel.prevented,true);
assert.equal(dialog.open,false);
assert.equal(dialog.hasAttribute('data-closing'),false);
window.matchMedia=()=>({matches:false});
click(linkedFrame);
const stale=loads[3].onload;
window.listeners.accountchange();
stale();
assert.equal(viewport.children.length,0);
assert.equal(dialog.open,false);

Object.assign(viewport,{clientWidth:360,clientHeight:640,scrollLeft:0,scrollTop:0});
click(linkedFrame);
const portrait=loads.at(-1);
Object.assign(portrait,{naturalWidth:600,naturalHeight:1200});
portrait.onload();
assert.equal(portrait.height,640,'a tall image can use the entire viewport height');
assert.equal(portrait.offsetTop,0);
dialog.listeners.click({target:portrait,clientX:180,clientY:32});
assert.equal(portrait.height,1280);
assert.equal(viewport.scrollTop,32,'zoom near the top edge keeps the same image point under the pointer');
get('[data-lightbox-close]').onclick();
dialog.listeners.animationend({target:dialog,animationName:'lightbox-fade-out'});
assert.equal(dialog.open,false);

const html=read('web/index.html'),css=read('web/workspaces.css');
assert(css.includes('.lightbox-stage{position:absolute;inset:0}'));
assert.match(css,/\.lightbox-viewport\{\s*position:absolute;inset:0;/);
assert.match(css,/\.lightbox-toolbar\{[^}]*z-index:1/);
assert.match(html,/<dialog id="image-lightbox" aria-labelledby="image-lightbox-title">/);
assert.match(html,/data-image-original target="_blank" rel="noopener noreferrer" referrerpolicy="no-referrer"/);
assert.match(html,/<button[^>]*class="account-photo"/);
assert.match(css,/\.archive-image-slot>img\[data-image-state="loaded"\]\{\s*width:auto;height:auto;max-width:min\(200px,100%\)/);
assert.match(html, /data-image-original[^>]*>访问原图<\/a>\s*<button[^>]*data-lightbox-close/);
assert.match(css, /#image-lightbox\{[^}]*border:0;background:transparent/);
assert.match(css, /#image-lightbox\[data-closing\][^{]*\{animation:lightbox-fade-out/);
console.log('PASS: borderless lightbox, fit, click and wheel zoom, drag, animated close, reduced motion, focus and source fallback');
