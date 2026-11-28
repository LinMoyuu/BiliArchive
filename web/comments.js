window.createCommentsWorkspace=function(ctx){
const realDocument=window.document;
const mapped=selector=>selector;
const document={getElementById(id){return ctx.root.querySelector('#'+id)||ctx.head.querySelector('#'+id);},querySelectorAll(selector){return [...ctx.root.querySelectorAll(mapped(selector)),...ctx.head.querySelectorAll(mapped(selector))];},addEventListener(type,handler){realDocument.addEventListener(type,event=>{if(ctx.active())handler(event);});},get activeElement(){return realDocument.activeElement;}};
const location={get href(){return ctx.url().href;},get search(){return ctx.url().search;},get hash(){return ctx.url().hash;}};
const history={replaceState(state,title,url){ctx.replaceHash(url);}};
// BEGIN WORKSPACE LOGIC

const $ = id => document.getElementById(id);
const loading = (id, mode, label, valid) => globalThis.archiveLoading?.begin($(id), mode, label, valid);
const PAGE = 30;              // 每批往页面上拼多少条

const state = {
  doc: null,
  key: '',                    // 当前选中的内容：type-aid（动态的 id 和视频 aid 可能撞号）
  filter: 'all',
  type: 'all',                // 左侧内容类型：all | video | dynamic | article | audio
  vq: '',                     // 左侧：稿件搜索
  q: '',                      // 右侧：评论区搜索
  videoSort: 'new',           // 稿件排序，与评论排序独立
  sort: 'like',               // like 点赞（默认）| old 最早 | new 最新
  onlyUp: false,
  withReplies: true,
  shown: 0,
  list: [],                   // 当前稿件过滤 + 排序后的评论
  rendered: 0,
};

const STATUS_BADGE = {deleted:'deleted', private:'private', rejected:'rejected'};
const STATUS_LABEL = {deleted:'已删除', private:'仅自己可见', rejected:'未公开', unknown:''};

function esc(s){return String(s||'').replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));}
function num(n){return (n||0).toLocaleString('zh-CN');}
function shortNum(n){
  if(n>=10000) return (n/10000).toFixed(1).replace(/\.0$/,'')+' 万';
  return num(n);
}
function hl(text, q){
  const src = String(text||'');
  if(!q) return esc(src);
  const lower = src.toLowerCase(), needle = q.toLowerCase();
  if(!needle) return esc(src);
  let out = '', i = 0;
  while(i <= src.length){
    const p = lower.indexOf(needle, i);
    if(p < 0){ out += esc(src.slice(i)); break; }
    out += esc(src.slice(i, p)) + '<mark>' + esc(src.slice(p, p + needle.length)) + '</mark>';
    i = p + needle.length;
  }
  return out;
}
function spaceURL(mid){
  return mid ? 'https://space.bilibili.com/' + mid : '';
}
function nameHTML(user, extra){
  const name = (user && user.uname) || '?';
  const cls = 'name' + (extra ? ' ' + extra : '') + (user && user.is_vip ? ' vip' : '') + (user && user.is_up ? ' up' : '');
  const url = spaceURL(user && user.mid);
  const inner = esc(name);
  return url
    ? `<a class="${cls}" href="${url}" target="_blank" rel="noopener" title="打开用户主页">${inner}</a>`
    : `<span class="${cls}">${inner}</span>`;
}
function avatarHTML(user, extra){
  const name = (user && user.uname) || '?';
  const original = String(user?._original_resources?.avatar || user?.avatar || '');
  const src = resourceImages.get(original)||(user?._original_resources?.avatar && user.avatar)||original.replace(/^http:\/\//i, 'https://');
  const cls = 'avatar' + (extra ? ' ' + extra : '') + (user && user.is_up ? ' up' : '');
  const inner = src
    ? `<span class="archive-image-slot archive-avatar-slot"><img data-archive-src="${esc(src)}" referrerpolicy="no-referrer" loading="lazy" alt="" data-archive-original="${esc(original)}" data-archive-avatar></span>`
    : esc(name.slice(0,1));
  const url = spaceURL(user && user.mid);
  return `<div class="${cls}">` + (url
    ? `<a href="${url}" target="_blank" rel="noopener" title="打开用户主页">${inner}</a>`
    : inner) + `</div>`;
}
function picturesHTML(list){
  const arr = (list || []).filter(Boolean);
  if(!arr.length) return '';
  // B 站图床按 Referer 防盗链：带本地地址的 Referer 一律 403，
  // 所以图片本身和「点开原图」的新标签页都必须不带 Referer
  return '<div class="pics">' + arr.map(u =>
    `<a href="${esc(u)}" target="_blank" rel="noopener noreferrer" referrerpolicy="no-referrer"><span class="archive-image-slot"><img data-archive-src="${esc(resourceImages.get(u)||u)}" data-archive-original="${esc(u)}" data-archive-placeholder referrerpolicy="no-referrer" loading="lazy" alt=""></span></a>`
  ).join('') + '</div>';
}
function locationHTML(user){
  return (user && user.location) ? `<span class="loc">IP属地：${esc(user.location)}</span>` : '';
}

// ---------------- 稿件排序：默认发布日期从新到旧 ----------------
function timeVal(value){
  if(value === null || value === undefined || value === '') return null;
  const text = String(value).trim();
  if(!text) return null;
  if(typeof value === 'number' || /^\d+(?:\.\d+)?$/.test(text)){
    const n = Number(value);
    if(!Number.isFinite(n) || n <= 0) return null;
    const ms = n >= 1e11 ? n : n * 1000;
    return Number.isFinite(new Date(ms).getTime()) ? ms : null;
  }
  let t = Date.parse(text);
  if(!Number.isFinite(t) && /^\d{4}-\d{2}-\d{2} /.test(text)){
    t = Date.parse(text.replace(/-/g, '/'));
  }
  return Number.isFinite(t) ? t : null;
}
function pubMs(v){
  for(const value of [v.publish_time_text, v.publishTime, v.publish_time]){
    const t = timeVal(value);
    if(t !== null) return t;
  }
  return null;
}
// 发布时间缺失时，使用最早评论时间。
function firstCommentTime(v){
  let min = timeVal(v.first_comment_time);
  (v.comments || []).forEach(c=>{
    const t = timeVal(c.ctime_text) ?? timeVal(c.ctime);
    if(t !== null && (min === null || t < min)) min = t;
  });
  return min;
}
function dateKey(v){
  return pubMs(v) ?? firstCommentTime(v);
}
function compareVideoTime(a, b){
  const ka = dateKey(a), kb = dateKey(b);
  if(ka === kb) return 0;
  // 缺少时间始终置后，不随排序方向翻转。
  if(ka === null) return 1;
  if(kb === null) return -1;
  return state.videoSort === 'old' ? ka - kb : kb - ka;
}
function videoCommentCount(video){
  const reported = Number(video.comment_total);
  if (Number.isFinite(reported) && reported > 0) return reported;
  const comments = video.comments || [];
  const topLevel = video.fetched ?? comments.length;
  const replies = video.replies ?? comments.reduce((count, comment) => count + (comment.replies || []).length, 0);
  return (Number(topLevel) || 0) + (Number(replies) || 0);
}
function compareVideos(a, b){
  if (state.videoSort === 'comments' || state.videoSort === 'comments-asc') {
    const difference = videoCommentCount(a) - videoCommentCount(b);
    if (difference) return state.videoSort === 'comments-asc' ? difference : -difference;
  }
  return compareVideoTime(a, b);
}
function renderVideoSort(){
  document.querySelectorAll('#videoSortSeg button').forEach(button=>{
    button.setAttribute('aria-pressed', String(button.dataset.videoSort === state.videoSort));
    button.onclick = ()=>{
      state.videoSort = button.dataset.videoSort;
      renderVideoSort();
      renderList();
    };
  });
}
const resourceImages=new Map();
function rememberResources(value){if(!value||typeof value!=='object')return;if(Array.isArray(value)){value.forEach(rememberResources);return;}for(const[key,original]of Object.entries(value._original_resources||{})){resourceImages.set(original,value[key]);}Object.values(value).forEach(v=>{if(v&&typeof v==='object')rememberResources(v);});}
function coverHTML(src, fallback){
  const imageSrc=resourceImages.get(src)||src;
  if(!src) return `<span>${esc(fallback)}</span>`;
  return `<a data-cover-link href="${esc(src)}" target="_blank" rel="noopener noreferrer" referrerpolicy="no-referrer" title="打开封面原图"><span class="archive-image-slot"><img data-archive-src="${esc(imageSrc)}" data-archive-original="${esc(src)}" data-archive-placeholder referrerpolicy="no-referrer" loading="lazy" alt=""></span></a>`;
}

function ymd(ms){
  const d = new Date(ms), p = n => String(n).padStart(2, '0');
  // 带年份，否则跨年的列表看起来像乱序
  return String(d.getFullYear()).slice(2) + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate());
}
function dateLabel(v){
  const p = pubMs(v);
  if(p !== null) return `<span title="发布 ${new Date(p).toLocaleString('zh-CN')}">${ymd(p)}</span>`;
  const f = firstCommentTime(v);
  if(f === null) return '';
  // 没有发布日期就标出「首评」，让人知道这行是按最早评论排的
  return `<span title="无发布时间，按最早一条评论 ${new Date(f).toLocaleString('zh-CN')}">首评 ${ymd(f)}</span>`;
}

// ---------------- 载入 ----------------
let detailRequest;
let detailVideo;
let searchRequest;
let matchedKeys;
async function searchArchive(q) {
  searchRequest?.abort();
  if (!q || !state.doc?.paged_details) {
    matchedKeys = null;
    return;
  }
  const controller = new AbortController();
  searchRequest = controller;
  const finish = loading('vlist', $('vlist').children?.length ? 'overlay' : 'skeleton', '正在搜索稿件', () => !controller.signal.aborted);
  try {
    const response = await fetch('/api/comments/data.json?view=search&q=' + encodeURIComponent(q), {signal: controller.signal});
    if (!response.ok) throw new Error('搜索归档失败');
    const keys = await response.json();
    if (!controller.signal.aborted) matchedKeys = new Set(keys);
  } finally { finish?.(); }
}
let loadRequest;
function loadArchive() {
  loadRequest?.abort();
  detailRequest?.abort();
  searchRequest?.abort();
  state.doc = null;
  state.key = '';
  state.list = [];
  matchedKeys = null;
  resourceImages.clear();
  detailVideo = null;
  $('boot').textContent = '';
  $('vlist').textContent = '';
  $('detail').style.display = 'none';
  $('tools').style.display = 'none';
  $('comments').textContent = '';
  $('stats').textContent = '';
  const controller = new AbortController();
  loadRequest = controller;
  const finish = loading('vlist', 'skeleton', '正在读取稿件', () => !controller.signal.aborted);
  return fetch('/api/comments/data.json?view=list', {signal: controller.signal}).then(r=>{
  if (!r.ok) throw new Error('读取归档列表失败');
  return r.json();
}).then(async d=>{
  if (controller.signal.aborted) return;
  rememberResources(d);
  for(const[source,target]of Object.entries(d.resource_images||{}))resourceImages.set(source,target);
  state.doc = d;
  if(!state.doc.videos) state.doc.videos = [];
  const usp = new URLSearchParams(location.search);
  if(['all','hidden','public'].includes(usp.get('filter'))) state.filter = usp.get('filter');
  if(['all','video','dynamic','article','audio'].includes(usp.get('type'))) state.type = usp.get('type');
  const sortParam = usp.get('sort');
  if(['new','old','like','time'].includes(sortParam)){
    state.sort = sortParam === 'time' ? 'new' : sortParam;   // time 是旧值，按最新处理
  }
  if(usp.get('q')){ state.q = usp.get('q'); $('cq').value = state.q; }
  if(usp.get('vq')){ state.vq = usp.get('vq'); $('q').value = state.vq; }

  finish?.();
  renderStats();
  renderFilters();
  renderTypeFilters();
  renderSort();
  renderVideoSort();
  syncSearchBoxes();

  // 选中稿件：?aid=123 或 #123；带了评论区搜索词时，优先落在第一个命中的稿件
  const wantAid = usp.get('aid');
  const wantKey = decodeURIComponent((location.hash || '').replace('#', ''));
  if (d.paged_details && (state.vq || (state.q && !wantAid && !wantKey))) {
    await searchArchive(state.vq || state.q);
  }
  if (controller.signal.aborted) return;
  const visible = visibleVideos();
  const hit = visible.find(v=>keyOf(v)===wantKey)
    || (wantAid ? visible.find(v=>String(v.aid)===wantAid) : null)
    || (state.q ? visible.find(v=>videoHitsQuery(v, state.q)) : null)
    || visible[0] || state.doc.videos[0];
  state.key = hit ? keyOf(hit) : '';
  renderList();
  await selectVideo(state.key, { keepScroll: true });
}).catch(e=>{
  if (!controller.signal.aborted && e.name !== 'AbortError') $('boot').textContent = '加载失败：' + e.message;
}).finally(() => finish?.());
}
let ready = loadArchive();

// ---------------- 左侧稿件列表 ----------------
function renderStats(){
  const s = state.doc.stats || {};
  const hidden = (state.doc.videos||[]).filter(v=>['deleted','private','rejected'].includes(v.status)).length;
  $('stats').innerHTML = `
    <div class="stat"><b>${num(s.videos||state.doc.videos.length)}</b>个稿件</div>
    <div class="stat"><b>${num(s.comments)}</b>条评论</div>
    <div class="stat"><b>${num(s.replies)}</b>条回复</div>
    ${s.dynamics?`<div class="stat"><b>${num(s.dynamics)}</b>个动态</div>`:''}
    ${hidden ? `<div class="stat warn"><b>${num(hidden)}</b>个已隐藏</div>` : ''}
  `;
}

function videoHitsQuery(v, q){
  const needle = q.toLowerCase();
  if((v.title||'').toLowerCase().includes(needle)) return true;
  if((v.text||'').toLowerCase().includes(needle)) return true;
  if((v.orig_text||'').toLowerCase().includes(needle)) return true;
  if((v.bvid||'').toLowerCase().includes(needle)) return true;
  if (state.doc?.paged_details) return matchedKeys?.has(keyOf(v)) ?? false;
  return (v.comments||[]).some(c =>
    (c.content||'').toLowerCase().includes(needle)
    || (c.uname||'').toLowerCase().includes(needle)
    || (c.replies||[]).some(r =>
      (r.content||'').toLowerCase().includes(needle)
      || (r.uname||'').toLowerCase().includes(needle)));
}

function matchVideo(v){
  if(state.filter==='hidden' && !['deleted','private','rejected'].includes(v.status)) return false;
  if(state.filter==='public' && v.status!=='public') return false;
  // 按内容类型筛选
  if(state.type !== 'all' && typeOf(v) !== state.type) return false;
  if(!state.vq) return true;
  return videoHitsQuery(v, state.vq);
}

function visibleVideos(){
  return (state.doc.videos||[]).filter(matchVideo).sort(compareVideos);
}

function renderFilters(){
  document.querySelectorAll('#filters .chip').forEach(b=>{
    b.setAttribute('aria-pressed', String(b.dataset.f===state.filter));
    b.onclick = ()=>{
      state.filter = b.dataset.f;
      renderFilters();
      renderList();
    };
  });
}

function renderTypeFilters(){
  document.querySelectorAll('#typeFilters .chip').forEach(b=>{
    b.setAttribute('aria-pressed', String(b.dataset.type===state.type));
    b.onclick = ()=>{
      state.type = b.dataset.type;
      renderTypeFilters();
      renderList();
    };
  });
}

const TYPE_NAME = {video:'视频', dynamic:'动态', article:'专栏', audio:'音频'};
function keyOf(v){ return (v.type||'video') + '-' + v.aid; }
function typeOf(v){ return v.type || 'video'; }
function isPlainType(v){ const k = typeOf(v); return k==='dynamic'||k==='article'||k==='audio'; }
// 每种内容的官方编号都不一样：动态是 opus id，专栏 cv，音频 au，视频 bvid/av
function idLabel(v){
  switch(typeOf(v)){
    case 'dynamic': return '动态 ID ' + (v.dynamic_id || v.aid);
    case 'article': return 'cv' + v.aid;
    case 'audio':   return 'au' + v.aid;
    default:        return v.bvid || ('av' + v.aid);
  }
}
// 动态没有标题：正文首行当标题，再退到 id
function dynTitle(v){
  const line = (v.text||'').split('\n').map(x=>x.trim()).find(Boolean) || '';
  if(line) return line.length > 80 ? line.slice(0,80)+'…' : line;
  const typeName = TYPE_NAME[typeOf(v)] || '内容';
  return typeName + ' ' + (typeOf(v)==='dynamic' && v.dynamic_id ? v.dynamic_id : v.aid);
}
function findContent(key){
  return (state.doc.videos||[]).find(x=>keyOf(x)===key);
}
function renderList(){
  const list = visibleVideos();
  if(!list.length){
    $('vlist').innerHTML = `<div class="empty" style="height:220px"><div class="box">空</div><div>没有匹配的稿件</div></div>`;
    return;
  }
  $('vlist').innerHTML = list.map(v=>{
    const cnt = v.fetched ?? (v.comments||[]).length;
    const badge = STATUS_LABEL[v.status] ? `<span class="badge ${STATUS_BADGE[v.status]}">${STATUS_LABEL[v.status]}</span>` : '';
    const KN = TYPE_NAME[typeOf(v)] || TYPE_NAME.video;
    const typeBadge = isPlainType(v) ? `<span class="badge k">${KN}</span>` : '';
    const coverSrc = v._original_resources?.cover || v.cover || (v.type==='dynamic' && (v.text_pics||[])[0]) || '';
    const cover = coverHTML(coverSrc, KN[0]);
    const dl = dateLabel(v);
    const rowTitle = v.title || (isPlainType(v) ? dynTitle(v) : ('av'+v.aid));
    return `<div class="vrow${keyOf(v)===state.key?' active':''}" data-key="${keyOf(v)}">
      <div class="thumb">${cover}</div>
      <div style="min-width:0">
        <p class="vtitle">${hl(rowTitle, state.vq)}</p>
        <div class="vmeta">
          ${typeBadge}
          ${badge}
          ${dl}
          <span>抓取 ${num(cnt)}</span>
          ${v.comment_total && v.status!=='deleted'?`<span class="dot"></span><span>官方 ${shortNum(v.comment_total)}</span>`:''}
        </div>
      </div>
    </div>`;
  }).join('');
  $('vlist').querySelectorAll('.vrow').forEach(el=>{
    el.onclick = e=>{
      if(e.target.closest('[data-cover-link]')) return;
      selectVideo(el.dataset.key);
    };
  });
  // 当前选中的稿件被筛掉了，就自动切到列表里的第一个
  if(!list.some(v=>keyOf(v)===state.key)) selectVideo(keyOf(list[0]), {keepScroll:true});
}

// ---------------- 右栏 ----------------
async function selectVideo(key, opts){
  if(!key) return;
  const sameVideo = detailVideo && keyOf(detailVideo) === key;
  detailRequest?.abort();
  if (!state.doc) return;
  if (!sameVideo) detailVideo = null;
  state.key = key;
  state.shown = 0;
  state.rendered = 0;
  history.replaceState(null,'','#'+encodeURIComponent(key));
  let v = findContent(key);
  if(!v) return;
  if (state.doc.paged_details && !sameVideo) {
    const controller = new AbortController();
    detailRequest = controller;
    $('tools').style.display = 'none';
    $('detail').style.display = 'flex';
    $('detail').textContent = '';
    $('comments').textContent = '';
    const finish = loading('comments', 'skeleton', '正在读取稿件详情', () => !controller.signal.aborted);
    try {
      const url = new URLSearchParams({view: 'detail'});
      if (v.comment_type) url.set('comment_type', v.comment_type);
      const response = await fetch('/api/comments/posts/' + encodeURIComponent(v.aid) + '?' + url, {signal: controller.signal});
      if (!response.ok) throw new Error('读取稿件失败');
      const detail = await response.json();
      if (detail.error) throw new Error('稿件暂不可用');
      if (controller.signal.aborted || state.key !== key) return;
      rememberResources(detail);
      for (const [source, target] of Object.entries(detail.resource_images || {})) {
        resourceImages.set(source, target);
      }
      detailVideo = detail;
    } catch (error) {
      if (!controller.signal.aborted) $('detail').textContent = error.message;
      return;
    } finally { finish?.(); }
  }
  v = detailVideo || v;
  const type = typeOf(v);
  const KN = TYPE_NAME[type] || TYPE_NAME.video;
  const plain = isPlainType(v);
  const dyn = type === 'dynamic';
  const badge = STATUS_LABEL[v.status] ? `<span class="badge ${STATUS_BADGE[v.status]}">${STATUS_LABEL[v.status]}</span>` : '';
  const typeBadge = plain ? `<span class="badge k">${KN}</span>` : '';
  const coverSrc = v._original_resources?.cover || v.cover || (dyn && (v.text_pics||[])[0]) || '';
  const cover = coverHTML(coverSrc, KN[0]);
  const cmts = (v.comments||[]);
  const replies = cmts.reduce((n,c)=>n+((c.replies||[]).length),0);
  const title = plain ? (v.title || dynTitle(v)) : (v.title||('av'+v.aid));
  const sub = [];
  sub.push(`<span class="kv">${esc(idLabel(v))}</span>`);
  if(v.publish_time_text) sub.push(`<span class="kv">发布 <b>${esc(v.publish_time_text)}</b></span>`);
  if(v.ip_location) sub.push(`<span class="loc">IP属地：${esc(v.ip_location)}</span>`);
  if(dyn){
    if(v.like_count) sub.push(`<span class="kv">赞 <b>${num(v.like_count)}</b></span>`);
    if(v.forward_count) sub.push(`<span class="kv">转发 <b>${num(v.forward_count)}</b></span>`);
  }
  sub.push(`<span class="kv">抓取 <b>${num(cmts.length)}</b> 条评论 · <b>${num(replies)}</b> 条回复</span>`);
  if(v.comment_total && v.status!=='deleted') sub.push(`<span class="kv">官方计数 <b>${num(v.comment_total)}</b></span>`);
  if(v.status_text && !STATUS_LABEL[v.status]) sub.push(`<span class="st ${esc(v.status)}">${esc(v.status_text)}</span>`);
  const body = (dyn || (plain && (v.text||v.orig_text))) ? `<div class="dyn-body">
      <div class="t">${esc(v.text||'（这条内容没有文字）')}</div>
      ${(v.text_pics||[]).length?picturesHTML(v.text_pics):''}
      ${v.orig_text?`<div class="orig"><span class="who">@${esc(v.orig_author||'原作者')}</span>${esc(v.orig_text)}</div>`:''}
    </div>` : '';
  const jsonParams = new URLSearchParams();
  if (v.comment_type) jsonParams.set('comment_type', v.comment_type);
  const openLabel = '打开' + KN;
  const openLink = (v.status==='deleted' || !v.url) ? ''
    : `<a class="btn" href="${esc(v.url)}" target="_blank" rel="noopener">${openLabel}</a>`;
  $('detail').style.display = 'flex';
  $('detail').innerHTML = `
    <div class="detail-cover">${cover}</div>
    <div class="detail-main">
      <h1>${esc(title)} ${typeBadge} ${badge}</h1>
      <div class="detail-sub">${sub.join('')}</div>
      ${body}
    </div>
    <div class="head-tools">
      ${openLink}
      <a class="btn" href="/api/comments/posts/${encodeURIComponent(v.aid)}${jsonParams.size ? `?${esc(jsonParams.toString())}` : ''}" target="_blank" rel="noopener">本条 JSON</a>
    </div>`;
  $('tools').style.display = 'flex';
  [...$('vlist').querySelectorAll('.vrow')].forEach(el=>{
    el.classList.toggle('active', el.dataset.key===state.key);
  });
  if(!opts || !opts.keepScroll){
    $('vlist').querySelector('.vrow.active')?.scrollIntoView({block:'nearest'});
  }
  resetComments();
}

function commentMatches(c, q){
  if(!q) return true;
  const needle = q.toLowerCase();
  if((c.content||'').toLowerCase().includes(needle)) return true;
  if((c.uname||'').toLowerCase().includes(needle)) return true;
  return (c.replies||[]).some(r =>
    (r.content||'').toLowerCase().includes(needle)
    || (r.uname||'').toLowerCase().includes(needle));
}

function commentList(){
  const v = detailVideo || findContent(state.key);
  if(!v) return [];
  let list = (v.comments||[]).filter(c=>commentMatches(c, state.q));
  if(state.onlyUp) list = list.filter(c=>c.is_up);
  const byNew = (a,b)=> (b.ctime||0) - (a.ctime||0);   // 新 → 旧
  const byOld = (a,b)=> (a.ctime||0) - (b.ctime||0);   // 旧 → 新
  const timeCmp = state.sort === 'old' ? byOld : byNew;
  list.sort((a,b)=>{
    if(a.is_pinned !== b.is_pinned) return a.is_pinned ? -1 : 1;   // 置顶恒定在最前
    if(state.sort === 'like') return (b.like||0) - (a.like||0) || byNew(a,b);
    return timeCmp(a,b);
  });
  return list;
}

function resetComments(){
  state.list = commentList();
  state.rendered = 0;
  const box = $('comments');
  if(!state.list.length){
    $('cmtCount').textContent = '0 条';
    box.innerHTML = `<div class="empty" style="height:300px"><div class="box">💬</div><div>${state.q?'没有匹配的评论':'这个稿件还没有抓到评论'}</div></div>`;
    return;
  }
  box.innerHTML = '';
  appendComments();
}

// 往页面上追加一批评论（滚动到底自动触发）
function appendComments(){
  if(typeof ctx!=='undefined' && !ctx.active())return;
  const box = $('comments');
  if(state.rendered >= state.list.length) return;
  const from = state.rendered;
  const to = Math.min(from + PAGE, state.list.length);
  const html = state.list.slice(from, to)
    .map(c=>commentHTML(c, state.q, state.withReplies))
    .join('');
  const footer = box.querySelector('.end');
  if(footer) footer.remove();
  box.insertAdjacentHTML('beforeend', html);
  state.rendered = to;
  $('cmtCount').textContent = `${num(state.list.length)} 条 · 已显示 ${num(state.rendered)}`;
  if(state.rendered < state.list.length){
    box.insertAdjacentHTML('beforeend', '<div class="end">向下滚动自动加载更多…</div>');
  } else {
    box.insertAdjacentHTML('beforeend', `<div class="end">已全部加载 · 共 ${num(state.list.length)} 条</div>`);
  }
  // 首屏没撑满（或窗口很高）时继续补，保证滚动条出现
  if(box.scrollHeight <= box.clientHeight + 40 && state.rendered < state.list.length){
    appendComments();
  }
}

function replyHTML(r, q){
  const top = `<div class="rtop">${nameHTML(r)}${r.is_up?'<span class="upflag">UP</span>':''}<span class="t">${esc(r.ctime_text)}</span>${locationHTML(r)}</div>`;
  return `<div class="reply">${avatarHTML(r,'sm')}<div class="rbody">${top}<div class="rtext">${hl(r.content,q)}</div>${picturesHTML(r.pictures)}</div></div>`;
}

function commentHTML(c, q, withReplies){
  const reps = c.replies||[];
  // 搜索时展示所有命中的楼中楼，不折叠
  const shown = q ? reps.filter(r => commentMatches(r, q)) : reps;
  const limit = q ? shown.length : 3;
  const repHTML = withReplies && shown.length ? `<div class="replies">${
      shown.slice(0,limit).map(r=>replyHTML(r,q)).join('')
    }${shown.length>limit?`<button class="more-replies" data-open="${c.rpid}">展开其余 ${shown.length-limit} 条回复</button>`:''}</div>` : '';
  return `<div class="cmt" data-rpid="${c.rpid}">
    ${avatarHTML(c)}
    <div class="cmt-body">
      <div class="cmt-top">
        ${nameHTML(c)}
        ${c.is_up?'<span class="upflag">UP</span>':''}
        ${c.level?`<span class="lv">LV${c.level}</span>`:''}
        ${c.is_pinned?'<span class="pin">置顶</span>':''}
        ${locationHTML(c)}
        <span class="t">${esc(c.ctime_text)}</span>
      </div>
      ${c.content?`<div class="cmt-text">${hl(c.content, q)}</div>`:''}
      ${picturesHTML(c.pictures)}
      <div class="cmt-foot">
        <span class="act">👍 ${num(c.like)}</span>
        ${reps.length?`<span class="act">💬 ${num(Math.max(reps.length, c.reply_count||0))}</span>`:''}
      </div>
      ${repHTML}
    </div>
  </div>`;
}

// 展开楼中楼：追加到已渲染的节点上（不重排整页，滚动位置不跳）
$('comments').addEventListener('click', e=>{
  const btn = e.target.closest('[data-open]');
  if(!btn) return;
  const c = state.list.find(x=>String(x.rpid)===btn.dataset.open);
  if(!c) return;
  const wrap = btn.closest('.replies');
  const reps = state.q ? (c.replies||[]).filter(r=>commentMatches(r, state.q)) : (c.replies||[]);
  wrap.innerHTML = reps.map(r=>replyHTML(r, state.q)).join('');
});

// 滚动到底部自动拼接下一批
let ticking = false;
$('comments').addEventListener('scroll', ()=>{
  if(ticking) return;
  ticking = true;
  requestAnimationFrame(()=>{
    ticking = false;
    const box = $('comments');
    if(box.scrollTop + box.clientHeight >= box.scrollHeight - 320){
      appendComments();
    }
  });
}, {passive:true});

function renderSort(){
  document.querySelectorAll('#sortSeg button').forEach(b=>{
    b.setAttribute('aria-pressed', String(b.dataset.sort===state.sort));
    b.onclick = ()=>{
      state.sort = b.dataset.sort;
      renderSort();
      resetComments();
      $('comments').scrollTop = 0;
    };
  });
}

function syncSearchBoxes(){
  $('q').value = state.vq;
  $('qClear').hidden = !$('q').value;
  $('cq').value = state.q;
  $('csearch').classList.toggle('filled', !!state.q);
}

// ---------------- 交互 ----------------
let timer = null;
let postSearchGeneration = 0;
async function applyPostSearch(query, generation) {
  $('boot').textContent = '';
  try {
    await searchArchive(query);
    if (generation === postSearchGeneration && query === $('q').value.trim() && state.doc) {
      state.vq = query;
      renderList();
    }
  } catch (error) {
    if (generation === postSearchGeneration && error.name !== 'AbortError') $('boot').textContent = error.message;
  }
}
function updatePostSearch() {
  clearTimeout(timer);
  searchRequest?.abort();
  const value = $('q').value;
  const query = value.trim();
  $('qClear').hidden = !value;
  const generation = ++postSearchGeneration;
  if (!query) return applyPostSearch(query, generation);
  timer = setTimeout(() => applyPostSearch(query, generation), 160);
}
$('q').oninput = updatePostSearch;
$('qClear').onclick = () => {
  $('q').value = '';
  $('q').focus();
  return updatePostSearch();
};

let ctimer = null;
$('cq').oninput = e => {
  $('csearch').classList.toggle('filled', !!e.target.value);
  clearTimeout(ctimer);
  ctimer = setTimeout(()=>{
    state.q = e.target.value.trim();
    resetComments();
    $('comments').scrollTop = 0;
  }, 160);
};
$('cqClear').onclick = ()=>{
  $('cq').value = '';
  state.q = '';
  $('csearch').classList.remove('filled');
  resetComments();
};

$('onlyUp').onchange = e => { state.onlyUp = e.target.checked; resetComments(); };
$('withReplies').onchange = e => { state.withReplies = e.target.checked; resetComments(); };

document.addEventListener('keydown', e=>{
  if(e.key==='/' && document.activeElement!==$('q') && document.activeElement!==$('cq')){
    e.preventDefault(); $('q').focus();
  }
  if(e.key==='Escape' && (document.activeElement===$('q') || document.activeElement===$('cq'))){
    document.activeElement.blur();
  }
});

// END WORKSPACE LOGIC
function applyRoute(){if(!ctx.active())return;const wanted=decodeURIComponent((ctx.url().hash||'').slice(1));if(wanted && findContent(wanted) && wanted!==state.key)selectVideo(wanted,{keepScroll:true});}
return {get ready(){return ready;},accountChanged(){ready = loadArchive();},activate(fromHistory){return ready.then(()=>{if(ctx.active()){if(fromHistory)applyRoute();if(state.rendered<Math.min(PAGE,state.list.length))appendComments();}});},deactivate(){},refresh(){}};
};
