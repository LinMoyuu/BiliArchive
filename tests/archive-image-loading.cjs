const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const root = path.join(__dirname, '..');
const read = file => fs.readFileSync(path.join(root, file), 'utf8');

const frames = [], timers = new Map(), listeners = {};
const visible = { hidden: false }, other = { hidden: true };
let onIntersection;
const rootedIntersections = new Map();
let timerId = 0;
function image(parent, name) {
 const attrs = new Map([
  ['data-archive-src', '/api/archive-resources/' + name],
  ['data-archive-original', 'https://i0.hdslb.com/' + name + '.jpg'],
  ['data-archive-placeholder', ''],
 ]);
 const slotAttrs=new Map();
 const slot={
  setAttribute(key,value){slotAttrs.set(key,value);},
  removeAttribute(key){slotAttrs.delete(key);},
  hasAttribute(key){return slotAttrs.has(key);},
 };
 return {
  slot, tagName: 'IMG', isConnected: true, complete: true, currentSrc: '',
  closest: selector => selector==='[data-archive-scroll-root]' ? parent.scrollRoot || null :
   selector==='[hidden]' ? (parent.hidden ? parent : null) : slot,
  matches: () => true,
  getAttribute: key => attrs.get(key),
  setAttribute: (key, value) => attrs.set(key, value),
  hasAttribute: key => attrs.has(key),
  removeAttribute: key => attrs.delete(key),
  replaceWith(node) { this.placeholder = node; },
  set src(value) { attrs.set('src', value); this.currentSrc = value; },
  get src() { return attrs.get('src'); },
 };
}
const first = Array.from({length:6}, (_, i) => image(visible, String(i)));
const incoming = image(other, 'new');
const document = {
 hidden: false,
 querySelectorAll: () => [],
 addEventListener(type, callback) { listeners[type] = callback; },
 createElement() { return {}; },
};
const window = {requestAnimationFrame: callback => frames.push(callback)};
vm.runInNewContext(read('web/archive-images.js'), {
 window, document, URL,
 IntersectionObserver: class {
  constructor(callback, {root}) { if(root)rootedIntersections.set(root,callback);else onIntersection=callback; }
  observe() {}
  unobserve() {}
 },
 setTimeout(callback,delay) { timers.set(++timerId, {callback,delay}); return timerId; },
 clearTimeout(id) { timers.delete(id); },
});
const flush = () => { while(frames.length) frames.shift()(); };
const intersect = (...images) => onIntersection(images.map(target => ({target, isIntersecting:true})));

window.archiveImages.resume({querySelectorAll: () => first});
flush();
assert(first.every(img => !img.src && !img.hasAttribute('data-image-state')), 'offscreen images have static placeholders');
intersect(...first, incoming);
flush();
assert.equal(first.filter(img => img.src).length, 4, 'slow visible images leave room for data requests');
assert(!incoming.src, 'images in the hidden workspace are not loaded');
assert.equal(first[0].decoding, 'async');
assert.equal(first[0].getAttribute('data-image-state'),'loading');
assert.equal(first[0].fetchPriority, 'low');

listeners.load({target:first[0]});
assert.equal(first[0].getAttribute('data-image-state'),'loaded');
for(const timer of [...timers.values()].filter(timer=>timer.delay===120))timer.callback();
assert(!first[0].slot.hasAttribute('data-image-loading'),'cache hit never flashes a spinner');
assert(first[1].slot.hasAttribute('data-image-loading'),'slow request animates on its frame');
flush();
assert(first[4].src, 'the next image starts after a slot opens');
let interrupted = false;
listeners.error({target:first[1], stopImmediatePropagation() { interrupted = true; }});
assert(interrupted && first[1].src === 'https://i0.hdslb.com/1.jpg', 'local miss retries the CDN in the same slot');
assert(first[1].slot.hasAttribute('data-image-loading'),'fallback retains its active spinner');
flush();
assert(!first[5].src);
const latestTimeout = [...timers.values()].filter(timer=>timer.delay===8000).at(-1);
latestTimeout.callback();
flush();
assert(first[1].placeholder && first[5].src, 'a stalled fallback is replaced and releases its slot');
assert.equal(first[1].getAttribute('data-image-state'),'fallback');
assert(!first[1].slot.hasAttribute('data-image-loading'));

window.archiveImages.pause({querySelectorAll: () => first});
visible.hidden = true;
other.hidden = false;
window.archiveImages.resume({querySelectorAll: () => [incoming]});
intersect(incoming);
flush();
assert(incoming.src);
assert(first.slice(2).every(img => !img.src && !img.hasAttribute('data-image-state') && !img.slot.hasAttribute('data-image-loading')), 'switching cancels requests and animations');
assert(first[0].src, 'already completed images survive switching');
listeners.load({target:incoming});
const undecided = image(other,'pending-map');
const messageRoot = {contains: element => element === undecided};
window.archiveImages.hold(messageRoot);
intersect(undecided);
flush();
assert(!undecided.src, 'the image waits briefly for the local map without blocking the list');
window.archiveImages.setSource(undecided,'/api/archive-resources/local',true);
window.archiveImages.release(messageRoot);
intersect(undecided);
flush();
assert.equal(undecided.src,'/api/archive-resources/local');

const sidebar = {};
const sessionAvatar = image({hidden:false,scrollRoot:sidebar},'session-avatar');
window.archiveImages.resume({querySelectorAll: () => [sessionAvatar]});
intersect(sessionAvatar);
flush();
assert(!sessionAvatar.src,'document visibility must not load avatars outside the sidebar viewport');
rootedIntersections.get(sidebar)([{target:sessionAvatar,isIntersecting:true}]);
flush();
assert(sessionAvatar.src,'scrolling the sidebar starts avatar requests without clicking');
listeners.load({target:sessionAvatar});

const lookupAvatar = image(other,'profile-avatar');
window.archiveImages.startIndicator(lookupAvatar,'profile');
for(const timer of [...timers.values()].filter(timer=>timer.delay===120))timer.callback();
assert(lookupAvatar.slot.hasAttribute('data-image-loading'));
window.archiveImages.setSource(lookupAvatar,'/api/archive-resources/fetched-avatar');
window.archiveImages.stopIndicator(lookupAvatar,'profile');
assert(lookupAvatar.slot.hasAttribute('data-image-loading'),'profile spinner transfers to the new image request');
intersect(lookupAvatar);
flush();
assert(lookupAvatar.src);
listeners.load({target:lookupAvatar});
assert(!lookupAvatar.slot.hasAttribute('data-image-loading'));
const pausedAvatars = [image(other,'paused-a'), image(other,'paused-b')];
for(const avatar of pausedAvatars)window.archiveImages.startIndicator(avatar,'profile');
for(const timer of [...timers.values()].filter(timer=>timer.delay===120))timer.callback();
assert(pausedAvatars.every(avatar=>avatar.slot.hasAttribute('data-image-loading')));
window.archiveImages.pause({querySelectorAll:()=>pausedAvatars});
assert(pausedAvatars.every(avatar=>!avatar.slot.hasAttribute('data-image-loading')));

for(const file of ['web/comments.js','web/messages.js']){
 const imageTags = [...read(file).matchAll(/<img[^\n>]*data-archive-original[^\n>]*>/g)].map(match => match[0]);
 assert(imageTags.length > 0);
 assert(imageTags.every(tag => tag.includes('data-archive-src=')&&!/\s+src=/.test(tag)), file);
}

const indicatorWindow = {};
const indicator = image(visible,'loader');
const indicatorTimers = new Map();
let nextTimer = 0;
vm.runInNewContext(read('web/archive-loading.js'), {
 window:indicatorWindow,
 setTimeout(callback,delay){assert.equal(delay,150);indicatorTimers.set(++nextTimer,callback);return nextTimer;},
 clearTimeout(id){indicatorTimers.delete(id);},
});
const busy = indicatorWindow.archiveLoading;
const fast = busy.begin(indicator,'skeleton','正在加载');
assert.equal(indicator.getAttribute('aria-busy'),'true');
fast();
assert.equal(indicatorTimers.size,0);
assert(!indicator.hasAttribute('data-loading'));
const abandoned = busy.begin(indicator,'skeleton','旧请求');
const current = busy.begin(indicator,'overlay','正在搜索');
abandoned();
[...indicatorTimers.values()].at(-1)();
assert.equal(indicator.getAttribute('data-loading'),'overlay');
current();
assert(!indicator.hasAttribute('aria-busy') && !indicator.hasAttribute('data-loading'));
busy.begin(indicator,'skeleton','已取消',()=>false);
[...indicatorTimers.values()].at(-1)();
assert(!indicator.hasAttribute('aria-busy'));
assert.match(read('web/workspaces.css'), /prefers-reduced-motion:reduce/);
console.log('PASS: delayed loading states, superseded requests, and image success, fallback, timeout and cancellation');
