const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
const ui=fs.readFileSync(path.join(__dirname,'../ui/app.slint'),'utf8');
function body(marker){let at=ui.indexOf(marker);assert(at>=0,marker);let start=ui.indexOf('{',at),depth=1,i=start+1,quoted=false,escaped=false;for(;depth&&i<ui.length;i++){const c=ui[i];if(quoted){if(escaped)escaped=false;else if(c==='\\')escaped=true;else if(c==='"')quoted=false;}else if(c==='"')quoted=true;else if(c==='{')depth++;else if(c==='}')depth--;}assert.equal(depth,0,marker);return ui.slice(start+1,i-1);}
const surface=body('component WorkspaceSurface');
for(const [name,value]of Object.entries({'workspace-padding-x':16,'workspace-padding-y':14,'workspace-gap':14,'sidebar-width':372,'panel-radius':14,'panel-padding':14,'panel-title-height':20,'actions-height':123,'actions-bottom-padding':6,'actions-stats-gap':8,'stats-height':101,'log-min-height':180}))assert(ui.includes(name+': '+value+'px;'),name);
for(const binding of ['padding-left: Theme.workspace-padding-x','padding-right: Theme.workspace-padding-x','padding-top: Theme.workspace-padding-y','padding-bottom: Theme.workspace-padding-y','spacing: Theme.workspace-gap'])assert(surface.includes(binding));
assert(surface.includes('@children'));assert(surface.includes('background: Theme.bg'));
const panel=body('component HubPanel');for(const property of ['border-radius: Theme.panel-radius','border-width: 1px','drop-shadow-blur: Theme.panel-shadow-blur','drop-shadow-offset-y: Theme.panel-shadow-offset'])assert(panel.includes(property));
assert(ui.includes('component HubLogPanel inherits HubPanel'));assert(ui.includes('component StatsPreview inherits HubPanel'));
assert.match(ui,/component StatCard inherits Rectangle\s*\{[^}]*min-width: 0px;\s*preferred-width: 0px;\s*horizontal-stretch: 1;/);
for(const name of ['comments','messages']){
 const workspace=body(name+'-workspace := WorkspaceSurface');
 assert(workspace.includes('width: parent.width; height: parent.height;'));assert(!workspace.includes('padding: 16px;'));
 assert(workspace.includes('visible: root.workspace == '+(name==='comments'?0:1)));
 const left=body(name+'-left := VerticalLayout');assert(left.includes('width: Theme.sidebar-width'));assert(left.includes('spacing: Theme.workspace-gap'));
 const right=body(name+'-right := VerticalLayout');assert(right.includes('spacing: 0px'));assert(right.includes('height: Theme.actions-stats-gap'));assert(right.includes('height: Theme.workspace-gap'));assert(right.includes('min-width: 0px'));assert(right.includes('horizontal-stretch: 1'));
 const range=body(name+'-range := HubPanel');assert(range.includes('vertical-stretch: 1'));
 const action=body(name+'-actions := HubPanel');assert(action.includes('height: Theme.actions-height'));assert(action.includes(name+'-actions-header := SectionHeader'));assert(body('component SectionHeader').includes('height: Theme.panel-title-height'));assert(action.includes('padding: Theme.panel-padding'));assert(action.includes('padding-bottom: Theme.actions-bottom-padding'));assert(action.includes('spacing: 6px'));assert(action.includes('height: 32px'));
 assert(workspace.includes(name+'-stats := StatsPreview'));assert(workspace.includes(name+'-log := HubLogPanel'));
 assert(!new RegExp('if root\\.workspace[^\\n]*:\\s*'+name+'-workspace').test(ui),'workspaces must remain mounted');
}
const range=body('messages-range := HubPanel');assert(range.includes('padding: Theme.panel-padding'));assert(range.includes('padding: 0px'));assert(range.indexOf('messages-range-header := SectionHeader')<range.indexOf('message-options-scroll := HubScrollView'));
const dates=body('component MessageDateField');assert(dates.includes('PopupWindow'),'calendar stays outside scroll clipping');
const main=fs.readFileSync(path.join(__dirname,'../src/main.rs'),'utf8');assert(main.includes('workspace_panels_share_bounds_across_sizes_dpi_and_task_states'));assert(main.includes('get_comments_panel_bounds'));assert(main.includes('get_messages_panel_bounds'));
console.log('PASS: shared workspace grid, 14px panel corners, equal fixed panel heights, matching column containers, inset scrolling, external date popup and geometry regression coverage');

// Remove noise, not page loading, model updates, completion hints or error reporting.
assert(!main.includes('读取稿件第 {} 页，获得 {} 条'));
assert(!main.includes('共 {} 个稿件，勾选后点「开始采集」'));
for(const behavior of ['client.archives_page(pn, ps)','all.extend(list)','refresh_rows(&shared, &ui)','稿件已载入，勾选后开始采集'])assert(main.includes(behavior));
