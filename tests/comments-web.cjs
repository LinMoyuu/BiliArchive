const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const htmlPath=process.argv[2]||require('node:path').join(__dirname,'../web/index.html');
const html = fs.readFileSync(htmlPath, 'utf8');
assert.equal([...html.matchAll(/data-sort="([^"]+)"/g)].map(m=>m[1]).join(','),'like,old,new');
const script = fs.readFileSync(require('node:path').join(require('node:path').dirname(htmlPath),'comments.js'),'utf8').replace(/\r\n/g,'\n').match(/BEGIN WORKSPACE LOGIC\n([\s\S]*?)\n\/\/ END WORKSPACE LOGIC/)[1];
const nodes = new Map();
const row = {dataset:{key:'video-1'},classList:{toggle(){}},scrollIntoView(){}};
const buttons = ['new','old'].map(value=>({dataset:{videoSort:value},attributes:{},setAttribute(k,v){this.attributes[k]=v;}}));
function node(id){
  if(!nodes.has(id)) nodes.set(id, {innerHTML:'',style:{},value:'',scrollTop:0,
    classList:{toggle(){},remove(){}},addEventListener(){},
    insertAdjacentHTML(_,html){this.innerHTML += html;},
    querySelectorAll(selector){return id==='vlist' && selector==='.vrow' ? [row] : [];},querySelector(){return null;}});
  return nodes.get(id);
}
const ctx = vm.createContext({assert,console,
  document:{getElementById:node,querySelectorAll(selector){return selector==='#videoSortSeg button'?buttons:[];},addEventListener(){}},
  fetch:()=>new Promise(()=>{}),AbortController,URLSearchParams,location:{search:'',hash:''},history:{replaceState(){}},
  setTimeout,clearTimeout,requestAnimationFrame:()=>{},buttons,nodes,row});
vm.runInContext(script,ctx); // Parse and initialize the full page script without network requests.
vm.runInContext(`
assert.equal(state.sort,'like');
const sample={aid:999,comments:[{rpid:1,like:2,ctime:30,is_pinned:false},{rpid:2,like:9,ctime:10,is_pinned:false},{rpid:3,like:9,ctime:20,is_pinned:false}]};
state.doc={videos:[sample]};state.key='video-999';
assert.equal(commentList().map(c=>c.rpid).join(','),'3,2,1');
state.sort='old';assert.equal(commentList().map(c=>c.rpid).join(','),'2,3,1');
state.sort='new';assert.equal(commentList().map(c=>c.rpid).join(','),'1,3,2');
const seconds=1700000000, millis=1700000000000;
for(const value of [seconds,String(seconds),millis,String(millis)]) assert.equal(timeVal(value),millis);
assert.equal(timeVal('2023-11-14T22:13:20Z'),millis);
assert.equal(timeVal('2023-11-15T06:13:20+08:00'),millis);
assert.ok(Number.isFinite(timeVal('2023-11-15 06:13:20')));
for(const value of [undefined,null,'','invalid',0,Infinity,'1e999']) assert.equal(timeVal(value),null);
assert.equal(pubMs({publish_time_text:'broken',publish_time:seconds}),millis);
assert.equal(firstCommentTime({comments:[{ctime_text:'broken',ctime:seconds},{ctime:seconds+1}]}),millis);
const videos=[
  {aid:1,title:'alpha old',type:'video',status:'public',publish_time:100,cover:'https://img.example/original.jpg?x=1&y=2'},
  {aid:2,title:'alpha missing',type:'video',status:'public'},
  {aid:3,title:'alpha latest',type:'video',status:'public',publish_time:300},
  {aid:4,title:'other tie',type:'article',status:'private',publish_time:300},
  {aid:5,title:'alpha fallback',type:'video',status:'public',comments:[{ctime:200}]},
];
state.doc={videos};state.key='video-1';
assert.equal(state.videoSort,'new');
assert.equal(visibleVideos().map(v=>v.aid).join(','),'3,4,5,1,2');
state.videoSort='old';assert.equal(visibleVideos().map(v=>v.aid).join(','),'1,5,3,4,2');
assert.equal(videos.map(v=>v.aid).join(','),'1,2,3,4,5','source order must remain unchanged');
state.vq='alpha';state.type='video';state.filter='public';state.sort='like';
let selected=0,commentsReset=0;
const realSelect=selectVideo,realReset=resetComments;
selectVideo=()=>selected++;
resetComments=()=>commentsReset++;
renderVideoSort();buttons[0].onclick();
assert.equal(visibleVideos().map(v=>v.aid).join(','),'3,5,1,2');
assert.equal(state.key,'video-1');assert.equal(state.vq,'alpha');assert.equal(state.type,'video');assert.equal(state.filter,'public');
assert.equal(state.sort,'like');assert.equal(selected,0);assert.equal(commentsReset,0);
assert.equal(buttons[0].attributes['aria-pressed'],'true');
assert.ok(nodes.get('vlist').innerHTML.includes('data-cover-link href="https://img.example/original.jpg?x=1&amp;y=2"'));
let prevented=false;
row.onclick({target:{closest:()=>({})},preventDefault(){prevented=true;}});
assert.equal(selected,0);assert.equal(prevented,false,'image link must retain default navigation');
row.onclick({target:{closest:()=>null}});assert.equal(selected,1);
assert.equal(row.dataset.key,'video-1');
selectVideo=realSelect;selectVideo('video-1',{keepScroll:true});
assert.ok(nodes.get('detail').innerHTML.includes('data-cover-link href="https://img.example/original.jpg?x=1&amp;y=2"'));
resetComments=realReset;
const cover=coverHTML('https://img.example/raw.jpg','V');
for(const attribute of ['target="_blank"','rel="noopener noreferrer"','referrerpolicy="no-referrer"']) assert.ok(cover.includes(attribute));
assert.equal(coverHTML('','V'),'<span>V</span>');
assert.equal(coverHTML('','V').includes('<a'),false);
console.log('PASS: full script syntax, time formats, both sort directions, stable ties, missing times, filters, selection, comment sort independence, cover links and row-click behavior');
`,ctx);


async function verifyDiskPreview() {
  const requests = [];
  ctx.fetch = (url, {signal} = {}) => new Promise(resolve => requests.push({
    url, signal, resolve(body){ resolve({ok:true,json:async()=>body}); },
  }));
  ctx.location.hash = '#video-116644617717979';
  vm.runInContext("Object.assign(state, {vq:'',q:'',filter:'all',type:'all'})", ctx);
  const loaded = vm.runInContext('loadArchive()', ctx);
  assert.equal(requests[0].url, '/api/comments/data.json?view=list');
  requests[0].resolve({paged_details:true,stats:{},videos:[
    {aid:'1',type:'video',title:'One',comment_type:1,fetched:0},
    {aid:'116644617717979',type:'video',title:'From link',comment_type:1,fetched:1,first_comment_time:1700000000},
  ]});
  await new Promise(setImmediate);
  assert.equal(requests.length, 2, 'read only the selected detail');
  assert.match(requests[1].url, /^\/api\/comments\/posts\/116644617717979\?view=detail&comment_type=1$/);
  const newer = vm.runInContext("selectVideo('video-1')", ctx);
  assert.equal(requests[1].signal.aborted, true);
  requests[2].resolve({aid:'1',type:'video',title:'One',comment_type:1,comments:[{rpid:'123',content:'new detail'}]});
  await newer;
  requests[1].resolve({aid:'116644617717979',type:'video',title:'Old',comments:[]});
  await loaded;
  assert.equal(vm.runInContext('state.key',ctx),'video-1');
  assert.match(nodes.get('comments').innerHTML,/new detail/);
  assert.doesNotMatch(nodes.get('detail').innerHTML,/>Old</);
  assert.match(nodes.get('detail').innerHTML, /href="\/api\/comments\/posts\/1\?comment_type=1"/);
  const searched = vm.runInContext("searchArchive('from replies')",ctx);
  assert.equal(requests[3].url,'/api/comments/data.json?view=search&q=from%20replies');
  requests[3].resolve(['video-116644617717979']);
  await searched;
  assert.equal(vm.runInContext("videoHitsQuery(state.doc.videos[1], 'from replies')",ctx),true);
  assert.equal(vm.runInContext("videoHitsQuery(state.doc.videos[0], 'from replies')",ctx),false);
  console.log('PASS: on-demand details, deep links, aborted stale responses, and archive-wide search');
}
verifyDiskPreview().catch(error=>{console.error(error);process.exitCode=1;});
