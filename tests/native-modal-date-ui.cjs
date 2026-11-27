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
const yearBox=body('year-list-panel := Rectangle');
assert(!yearBox.includes('TextInput'));
assert(!yearBox.includes('PopupWindow'));
assert(yearBox.includes('year-list := ListView'));
assert(date.includes('visible: calendar-focus.year-open'));
const limits=body('export global CalendarLimits');
assert(limits.includes('first-year: 2009'));
assert(limits.includes('last-year: 2077'));
assert(limits.includes('year-count: last-year - first-year + 1'));
assert(yearBox.includes('for row in CalendarLimits.year-count'));
assert(yearBox.includes('calendar-focus.select-year(CalendarLimits.first-year + row)'));
assert(yearBox.includes('calendar-focus.select-year(self.highlighted)'));
assert(date.includes('root.month-selected(year, root.calendar-month)'));
assert(date.includes('enabled: calendar-focus.year-open;\r\n                    clicked => { calendar-focus.close-years(); }')||
 date.includes('enabled: calendar-focus.year-open;\n                    clicked => { calendar-focus.close-years(); }'));
assert(yearBox.includes('min(CalendarLimits.last-year, max(CalendarLimits.first-year, value))'));
assert(date.includes('root.calendar-year > CalendarLimits.first-year || root.calendar-month > 1'));
assert(date.includes('root.calendar-year < CalendarLimits.last-year || root.calendar-month < 12'));
assert(ui.includes('text: "留空不限 · 结束日期包含当天"'));
const settings=body('settings-body := VerticalLayout');
assert(body('settings-card := Rectangle').includes('width: 448px'));
assert(settings.includes('control-width: 208px'));
assert(settings.includes('column-gap: 8px'));
const exportDir=body('export-dir-input := TextInput');
assert(exportDir.includes('read-only: true'));
assert(exportDir.includes('text: root.export-dir-text'));
assert(exportDir.includes('cursor-position-changed(position)'));
assert(exportDir.includes('position.x + self.x < 0px'));
assert(exportDir.includes('position.x + self.x > parent.width - 2px'));
assert(exportDir.includes('changed text => { self.text-offset = 0px; }'));
assert.match(settings,/text: "本地归档线程数"[^}]*font-weight: 600/);
assert(body('offline-resources := VerticalLayout').includes('spacing: 4px'));
assert.equal((settings.match(/text: "ms"/g)||[]).length,2);
assert(settings.includes('text: "每翻一页在此区间随机\\n越大越不易触发风控，但更慢"'));
assert(settings.includes('。\\n头像、封面和评论配图默认只保留链接"'));
assert(settings.includes('root.web-running ? "服务运行中" : "服务未运行"'));
assert(settings.includes('if root.web-running && root.web-address != ""'));
assert(body('web-address-hit := TouchArea').includes('root.settings-open-web()'));
assert(body('web-address-focus := FocusScope').includes('root.settings-open-web()'));
for(const action of ['settings-open-web','settings-toggle-web','settings-pick-dir','settings-open-dir']){
 const button=[...settings.matchAll(/Btn \{([^{}]*(?:clicked => \{[^{}]*\})?)\s*\}/g)].find(match=>match[1].includes(`root.${action}()`));
 assert(button,action);
 assert(!button[1].includes('small: true'),action);
}
const headerHeight=Number(date.match(/width: 320px; height: (\d+)px; spacing: 6px/)[1]);
const previousWidth=Number(date.match(/triangle: -1; width: (\d+)px/)[1]);
const nextWidth=Number(date.match(/triangle: 1; width: (\d+)px/)[1]);
const yearWidth=Number(body('year-button := Btn').match(/width: (\d+)px/)[1]);
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
assert(!body('comments-range := HubPanel').includes('root.resume-text'));
assert(!body('range-scroll := HubScrollView').includes('root.has-resume'));
assert(body('comments-task-controls := TaskControls').includes('has-clearable-resume: root.has-resume'));
assert(!body('messages-task-controls := TaskControls').includes('has-clearable-resume'));
console.log('PASS: native modal trees, exclusive top popup, scroll shielding, read-only calendar launch and virtualized year selection, local-read removal, compact action rows');
