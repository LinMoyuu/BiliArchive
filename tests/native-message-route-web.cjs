const fs=require('node:fs'),vm=require('node:vm'),assert=require('node:assert/strict'),path=require('node:path');
const repo=path.join(__dirname,'..'),source=fs.readFileSync(path.join(repo,'web/messages.js'),'utf8').replace(/\r\n/g,'\n').match(/BEGIN WORKSPACE LOGIC\n([\s\S]*?)\n\/\/ END WORKSPACE LOGIC/)[1];
const nodes=new Map(),requests=[];const node=id=>{if(!nodes.has(id))nodes.set(id,{value:'',querySelectorAll(){return[]}});return nodes.get(id)};
const ctx=vm.createContext({URL,URLSearchParams,setTimeout,clearTimeout,location:{href:'http://localhost/messages'},document:{getElementById:node,querySelectorAll(){return[]}},fetch:async url=>{requests.push(url);return new Promise(()=>{})}});vm.runInContext(source,ctx);const run=s=>vm.runInContext(s,ctx);
const expected='https://www.bilibili.com/video/BV1NqGk6wEmS/#reply304227065552';
const fixture={id:'1081079214088206',type:'mentions',url:'https://www.bilibili.com/video/BV1NqGk6wEmS',raw:{item:{native_uri:'bilibili://video/116644617717979?page=0&comment_root_id=300986161089&comment_secondary_id=304227065552',uri:'https://www.bilibili.com/video/BV1NqGk6wEmS',subject_id:'116644617717979',source_id:'304227065552',target_id:'301100008641',source_content:'回复内容',type:'reply'}}};
ctx.fixture=fixture;const original=JSON.stringify(fixture),count=requests.length;
let d=run('notificationDestination(fixture)');assert.equal(d.url,expected);assert.equal(d.label,'查看评论');assert.equal(JSON.stringify(fixture),original);assert.equal(requests.length,count,'route conversion is local');
const route=native=>{ctx.native=native;return run('nativeMessageRoute(native)')};
assert.equal(route('bilibili://video/116644617717979?comment_root_id=300986161089').url,'https://www.bilibili.com/video/BV1NqGk6wEmS/#reply300986161089');
assert.equal(route('bilibili://video/116644617717979?page=0').comment,false);
assert.equal(route('BILIBILI://VIDEO/170001').url,'https://www.bilibili.com/video/BV17x411w7KC/');
assert.equal(route('bilibili://video/170001').url,'https://www.bilibili.com/video/BV17x411w7KC/');
assert.equal(route('bilibili://video/BV17x411w7KC?comment_secondary_id=18446744073709551614').url,'https://www.bilibili.com/video/BV17x411w7KC/#reply18446744073709551614');
assert.equal(route('bilibili://video/18446744073709551615?comment_root_id=18446744073709551613').url,'https://www.bilibili.com/video/av18446744073709551615/#reply18446744073709551613');
for(const invalid of ['0','-1','1.5','1e3','Infinity','NaN','%3Cscript%3E'])assert(route('bilibili://video/170001?comment_secondary_id='+invalid+'&comment_root_id=42').url.endsWith('#reply42'));
assert(route('bilibili://video/170001?comment_secondary_id=1&comment_secondary_id=2&comment_root_id=42').url.endsWith('#reply42'));
assert(route('bilibili://video/170001?comment_secondary_id=00012&comment_root_id=0000').url.endsWith('#reply12'));
assert.equal(route('bilibili://video/170001?comment_secondary_id=0&comment_root_id=0').comment,false);
assert.equal(route('bilibili://dynamic/18446744073709551614').url,'https://www.bilibili.com/opus/18446744073709551614');
assert.equal(route('bilibili://opus/42?comment_secondary_id=12').comment,false);
assert.equal(route('bilibili://following/detail/42').url,'https://www.bilibili.com/opus/42');
assert.equal(route('bilibili://following/detail?dynamic_id=42').url,'https://www.bilibili.com/opus/42');
assert.equal(route('bilibili://article/42').url,'https://www.bilibili.com/read/cv42');
for(const bad of ['javascript:alert(1)','file:///C:/secret','data:text/html,hello','bilibili://unknown/42','bilibili://video/0','bilibili://video/not-a-video','bilibili://video/42/extra','bilibili://user:pass@video/42','bilibili://video:123/42','https://www.bilibili.com.evil.test/video/av42','https://evil.test/video/av42','https://user:pass@www.bilibili.com/video/av42','https://www.bilibili.com:8443/video/av42'])assert.equal(route(bad),null,bad);
ctx.other={...fixture,url:'https://www.bilibili.com/video/av99',raw:{item:{native_uri:'bilibili://video/170001'}}};assert(run('notificationDestination(other).url').includes('BV17x411w7KC'),'native identity wins over conflicting archive URL');
ctx.other={...fixture,url:'http://www.bilibili.com/video/av42#reply9',raw:{item:{native_uri:'bilibili://unsupported/42'}}};d=run('notificationDestination(other)');assert.equal(d.url,'https://www.bilibili.com/video/av42#reply9');assert.equal(d.label,'查看评论');
ctx.other={...fixture,url:'javascript:bad',raw:{item:{uri:'https://t.bilibili.com/42',source_id:'999',target_id:'888'}}};d=run('notificationDestination(other)');assert.equal(d.url,'https://t.bilibili.com/42');assert.equal(d.label,'查看主题');
ctx.other={url:'https://evil.test/',raw:{item:{native_uri:'bilibili://unknown/42'}}};assert.equal(run('notificationLink(other)'),'');
for(const type of ['replies','mentions','likes']){ctx.card={...fixture,type,like_count:1};const html=run('renderItem(card)');assert(html.includes('href="'+expected+'"'));assert(html.includes('查看评论'));assert(!html.includes('查看原站'));assert(!html.includes('href="bilibili:'));for(const attr of ['target="_blank"','rel="noopener noreferrer"','referrerpolicy="no-referrer"'])assert(html.includes(attr));}
// Clicking a native-route operation must not activate the enclosing like detail.
const card={dataset:{likeCard:fixture.id},querySelector(selector){return selector==='[data-like-open]'?{}:null}};
ctx.cardRoot={querySelectorAll(selector){return selector==='[data-like-card]'?[card]:[]}};
run('bindLikeCards(cardRoot)');card.onclick({target:{closest(){return{tagName:'A'}}}});assert.equal(run('likeView'),null);
// Notification operations change; subject identifiers remain canonical topic links.
ctx.subject={key:'video:aid:116644617717979',type:'video',id:'116644617717979',bvid:'BV1NqGk6wEmS',title:'主题',url:'https://www.bilibili.com/video/BV1NqGk6wEmS',cover_checked:true};assert(!run("subjectContent(subject,'group')").includes('#reply'));
console.log('PASS: exact native comment example, root/secondary priority, BigInt BV conversion and AV fallback, large/invalid/duplicate IDs, supported theme routes, safe fallback, native precedence, immutable archives and unified browser actions');
