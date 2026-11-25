const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const ui=fs.readFileSync(path.join(__dirname,'../ui/app.slint'),'utf8');
function body(marker){let i=ui.indexOf(marker);assert(i>=0,marker);i=ui.indexOf('{',i);const start=++i;let depth=1,quoted=false,escaped=false;for(;depth;i++){const c=ui[i];assert(c!==undefined);if(quoted){if(escaped)escaped=false;else if(c==='\\')escaped=true;else if(c==='"')quoted=false;}else if(c==='"')quoted=true;else if(c==='{')depth++;else if(c==='}')depth--;}return ui.slice(start,i-1);}
for(const name of ['settings','qr','alert','groups','verify']){
 const modal=body(name+'-modal := PopupWindow');assert(modal.includes('close-policy: no-auto-close'));assert(modal.includes('width: root.width; height: root.height'));assert(modal.includes('scroll-event(event) => {return accept;}')||modal.includes('scroll-event(event) => { return accept; }'));assert(modal.includes('if event.text == Key.Tab { return reject; }'));
}
assert(ui.includes('private property <int> modal-kind: root.verify-visible ? 5 : root.alert-visible ? 4'));
const date=body('component MessageDateField');
assert(!date.includes('TextInput'));
assert(date.includes('clicked => { root.open-calendar(); }'));
assert(date.includes('close-policy: close-on-click-outside'));
assert(date.includes('if root.calendar-accepted() { dropdown.close(); }'));
assert(date.includes('root.cleared(); dropdown.close();'));
assert(date.includes('text: "取消"; width: 72px; small: true; clicked => { dropdown.close(); }'));
assert(date.includes('cell.day == root.calendar-date.day'));
assert(date.includes('parent.picked ? Theme.accent-dark'));
assert(date.includes('border-width: cell.today ? 1px : 0px'));
const yearBox=body('component HubYearBox');
assert(!yearBox.includes('TextInput'));
assert(yearBox.includes('list := ListView'));
assert(yearBox.includes('for row in 8030'));
assert(yearBox.includes('root.edited(1970 + row)'));
const headerHeight=Number(date.match(/width: 320px; height: (\d+)px; spacing: 6px/)[1]);
const previousWidth=Number(date.match(/triangle: -1; width: (\d+)px/)[1]);
const nextWidth=Number(date.match(/triangle: 1; width: (\d+)px/)[1]);
const yearWidth=Number(date.match(/HubYearBox \{[^\n]*width: (\d+)px/)[1]);
const monthWidth=Number(date.match(/months := HubMonthBox \{ width: (\d+)px/)[1]);
for(const scale of [1,1.25,1.5]){
 const pixels=value=>Math.round(value*scale);
 assert.equal(pixels(previousWidth),pixels(headerHeight));
 assert.equal(pixels(nextWidth),pixels(headerHeight));
 assert.equal(pixels(previousWidth+yearWidth+monthWidth+nextWidth+3*6),pixels(320));
}
const html=fs.readFileSync(path.join(__dirname,'../web/index.html'),'utf8');
assert.match(html, /id="workspace-toggle"[^]*?class="workspace-chevron"[^]*?d="M1 1 L5 5 L9 1"/);
assert.match(html, /id="account-toggle"[^]*?class="chevron"[^]*?d="M0 0h10L5 6Z"/);
assert(body('component Toggle inherits Rectangle').includes('y: (parent.height - self.height) / 2'));
assert(!ui.includes('text: "读取本地"'));assert(ui.includes('actions-bottom-padding: 6px'));
for(const name of ['comments','messages'])assert(body(name+'-actions := HubPanel').includes(name+'-task-controls := TaskControls'));
console.log('PASS: native modal trees, exclusive top popup, scroll shielding, read-only calendar launch and virtualized year selection, local-read removal, compact action rows');
