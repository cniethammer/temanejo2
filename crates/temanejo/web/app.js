// Temanejo browser GUI. Plain canvas rendering, no dependencies.
//
// Task record keys (see graph_json in src/main.rs):
//   id   OMPT-side task id          k    0 initial, 1 implicit, 2 explicit
//   fl   ompt task flags            par  index of creating / encountering task (-1 none)
//   pr   index of parallel region   ix   thread number in team (implicit tasks)
//   ct   creation time [ns]         cth  creating thread
//   loc  index into g.locs          e    completion time [ns]
//   w    exclusive work time [ns]   cr   work time on the critical path [ns]
//   tl   1 if created by a taskloop construct
//   x,y  layout position (grid)     segs [thread, cpu, cpu_end, numa, start, end]
//   deps [address, ompt dependence type]
'use strict';

const $ = id => document.getElementById(id);
const canvas = $('canvas'), view = canvas.getContext('2d');
// The graph or timeline is rendered into an off-screen base canvas; the
// visible canvas shows a copy of it plus the overlay (hover and selection
// marks, selection rectangle). Hovering and drawing a selection rectangle
// only redraw the overlay. `ctx` points at the canvas being drawn on.
// The base is rasterised in software (`willReadFrequently` selects a CPU
// canvas in Chromium): GPU canvases are slow at stroking tens of thousands
// of thin lines, while copying the finished picture to the screen is cheap.
// `#gpubase` in the URL switches back to a GPU canvas for comparison.
const base = document.createElement('canvas');
const baseCtx = base.getContext('2d', { willReadFrequently: !location.hash.includes('gpubase') });
let ctx = view;
const DX = 26, DY = 64, R = 9;
const KIND = ['initial', 'implicit', 'explicit'];
const DEP_TYPE = { 1: 'in', 2: 'out', 3: 'inout', 4: 'mutexinoutset', 5: 'source', 6: 'sink', 7: 'inoutset', 34: 'out (all memory)', 35: 'inout (all memory)' };
const FLAGS = [[0x08000000, 'undeferred'], [0x10000000, 'untied'], [0x20000000, 'final'], [0x40000000, 'mergeable'], [0x80000000, 'merged'], [0x8, 'target'], [0x10, 'taskwait']];

const S = {
  g: null, view: 'graph', colorBy: 'thread',
  showCrit: true, showCreate: true, showDep: true, fadeOthers: true,
  sel: -1, region: new Set(), regionInfo: null, regionCrit: new Map(), regionEdges: new Set(),
  cam: { x: 0, y: 0, k: 1 },           // graph camera: screen = world * k + (x, y)
  tl: { t0: 0, t1: 1 },                // visible time window of the timeline
  drag: null, hover: -1, css: {}, children: [], succ: [], pred: [],
  originSort: { key: 'w', dir: -1 },   // task origins table: column and direction
  originFilter: '',                    // task origins table: lower-case filter text
};

// ------------------------------------------------------------------ helpers

function fmt(ns) {
  if (ns >= 1e9) return (ns / 1e9).toFixed(3) + ' s';
  if (ns >= 1e6) return (ns / 1e6).toFixed(2) + ' ms';
  if (ns >= 1e3) return (ns / 1e3).toFixed(1) + ' µs';
  return Math.round(ns) + ' ns';
}
// Percentage with one decimal; tiny non-zero shares are not shown as 0.0.
function pctText(part, whole) {
  const p = 100 * part / Math.max(whole, 1);
  return p > 0 && p < 0.1 ? '<0.1' : p.toFixed(1);
}
const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

function readCss() {
  const cs = getComputedStyle(document.documentElement);
  const v = n => cs.getPropertyValue(n).trim();
  S.css = {
    surface: v('--surface'), ink: v('--ink'), ink2: v('--ink2'), muted: v('--muted'), grid: v('--grid'), axis: v('--axis'),
    edgeCreate: v('--edge-create'), edgeDep: v('--edge-dep'), critical: v('--critical'),
    cat: [1, 2, 3, 4, 5, 6, 7, 8].map(i => v('--s' + i)), seqLo: v('--seq-lo'), seqHi: v('--seq-hi'),
  };
}
const hex = h => [1, 3, 5].map(i => parseInt(h.slice(i, i + 2), 16));
function mix(a, b, f) {
  const [x, y] = [hex(a), hex(b)];
  return 'rgb(' + x.map((c, i) => Math.round(c + (y[i] - c) * f)).join(',') + ')';
}

// Value a task is coloured by, for the categorical modes.
function catValue(t) {
  switch (S.colorBy) {
    case 'thread': return t.segs.length ? t.segs[0][0] : t.cth;
    case 'cpu': return t.segs.length ? t.segs[0][1] : -1;
    case 'numa': return t.segs.length ? t.segs[0][3] : -1;
    case 'loc': return t.loc;
    case 'region': return t.pr;
    case 'kind': return t.k;
  }
  return 0;
}
function catLabel(v) {
  const g = S.g;
  switch (S.colorBy) {
    case 'thread': return 'thread ' + v;
    case 'cpu': return 'CPU ' + v;
    case 'numa': return 'NUMA ' + v;
    case 'loc': return v < 0 ? 'unknown' : locShort(g.locs[v]);
    case 'region': return v < 0 ? 'none' : 'region ' + g.parallels[v].id;
    case 'kind': return KIND[v];
  }
}
// Start of a task: begin of its first execution segment, creation if it never ran.
const taskStart = t => t.segs.length ? t.segs[0][4] : t.ct;

// Sequential colour modes: value per task, legend label, and whether the
// scale is logarithmic (for quantities spanning orders of magnitude).
const SEQUENTIAL = {
  work: { value: t => t.w, label: 'work time', log: true },
  create: { value: t => t.ct, label: 'create time', log: false },
  start: { value: taskStart, label: 'start time', log: false },
  wait: { value: t => Math.max(taskStart(t) - t.ct, 0), label: 'wait time (start − create)', log: true },
};

// Sequential ramp in 64 precomputed steps (rebuilt when the theme changes):
// no colour string is built per task, and equal colours can be batched.
function seqColor(f) {
  const c = S.css, key = c.seqLo + c.seqHi;
  if (!S.seqPalette || S.seqPalette.key !== key)
    S.seqPalette = { key, cols: Array.from({ length: 64 }, (_, i) => mix(c.seqLo, c.seqHi, i / 63)) };
  return S.seqPalette.cols[Math.round(f * 63)];
}

function color(t) {
  const c = S.css, seq = SEQUENTIAL[S.colorBy];
  if (seq) {
    const { lo, hi } = S.seqRange, x = seq.log ? Math.log(Math.max(seq.value(t), 1)) : seq.value(t);
    return seqColor(hi > lo ? Math.min(1, Math.max(0, (x - lo) / (hi - lo))) : 1);
  }
  const slot = S.catSlots.get(catValue(t));
  return slot === undefined ? c.muted : c.cat[slot % 8];
}
// Colour follows the entity: slots are assigned in sorted value order per rank and mode.
function assignSlots() {
  const vals = [...new Set(S.g.tasks.map(catValue))].sort((a, b) => a - b);
  S.catSlots = new Map(vals.map((v, i) => [v, i]));
  const seq = SEQUENTIAL[S.colorBy];
  if (seq) {
    let min = Infinity, max = -Infinity;
    for (const t of S.g.tasks) {
      const v = seq.log ? Math.max(seq.value(t), 1) : seq.value(t);
      min = Math.min(min, v); max = Math.max(max, v);
    }
    S.seqRange = { min, max, lo: seq.log ? Math.log(min) : min, hi: seq.log ? Math.log(max) : max };
  }
}

// Function of a location as written in the program. Code inside OpenMP
// constructs is compiled into outlined helper functions named
// "<function>.omp_outlined..." (clang); those are reported by their enclosing
// function. Returns '' if the location has no function name.
function funcName(l) {
  if (!l || !l.func) return '';
  const i = l.func.indexOf('.omp_outlined');
  return i > 0 ? l.func.slice(0, i) : l.func;
}

function locShort(l) {
  if (!l) return 'unknown';
  if (l.file) return l.file.split('/').pop() + ':' + l.line;
  const mod = l.module ? ' (' + l.module.split('/').pop() + ')' : '';
  return (l.func || l.addr) + mod;
}

// ------------------------------------------------------------------ data

// Shows which trace is loaded: directory name (full path as tooltip) and
// recording time, also in the browser tab title.
async function loadInfo() {
  const info = await (await fetch('api/info')).json();
  const when = info.recorded_ms ? new Date(info.recorded_ms).toLocaleString() : '';
  $('case').innerHTML = `<span class="mono">${esc(info.name)}</span>` + (when ? ` <span class="muted">recorded ${esc(when)}</span>` : '');
  $('case').title = `${info.dir}\n${info.ranks} rank${info.ranks === 1 ? '' : 's'}${when ? ', recorded ' + when : ''}`;
  document.title = `${info.name} – Temanejo`;
}

async function loadRanks() {
  loadInfo().catch(() => {});
  const ranks = await (await fetch('api/ranks')).json();
  $('rank').innerHTML = ranks.map(r => `<option value="${r.rank}">${r.rank} – ${esc(r.host)} (${r.tasks} tasks)</option>`).join('');
  if (ranks.length) await loadRank(ranks[0].rank);
}

async function loadRank(rank) {
  const g = await (await fetch(`api/rank/${rank}/graph`)).json();
  S.g = g;
  S.sel = -1; S.hover = -1;
  const n = g.tasks.length;
  S.children = Array.from({ length: n }, () => []);
  S.succ = Array.from({ length: n }, () => []);
  S.pred = Array.from({ length: n }, () => []);
  for (const [a, b, kind] of g.edges) {
    if (kind === 0) S.children[a].push(b);
    else { S.succ[a].push(b); S.pred[b].push(a); }
  }
  S.byId = new Map(g.tasks.map((t, i) => [t.id, i]));
  findFans();
  S.tl = { t0: 0, t1: Math.max(g.wall, 1) };
  assignSlots();
  setRegion(new Set());
  renderSummary();
  renderOrigins();
  renderTask();
  renderLegend();
  fit();
}

function renderSummary() {
  const g = S.g;
  const par = g.work / Math.max(g.critical, 1);
  const tile = (v, l, cls = '') => `<div class="tile ${cls}"><b>${v}</b><span>${l}</span></div>`;
  $('summary').innerHTML =
    tile(par.toFixed(2), 'average parallelism of the graph (work ÷ critical path)', 'hero') +
    tile(g.tasks.length, 'tasks') + tile(g.threads.length, 'threads') +
    tile(fmt(g.work), 'total work T₁') + tile(fmt(g.critical), 'critical path T∞') +
    tile(fmt(g.wall), 'wall time') + tile((g.work / Math.max(g.wall, 1)).toFixed(2), 'measured avg. concurrency');
  const n = criticalTasks().size;
  $('summary-actions').innerHTML =
    `<button id="sel-crit" title="Select all tasks with work on the critical path for the region analysis">Select critical path</button>` +
    `<span class="muted">${n} task${n === 1 ? '' : 's'} on the path</span>`;
  $('sel-crit').onclick = () => setRegion(criticalTasks());
}

// Indices of all tasks with work on the critical path of the whole graph.
function criticalTasks() {
  const sel = new Set();
  S.g.tasks.forEach((t, i) => { if (t.cr > 0) sel.add(i); });
  return sel;
}

// ------------------------------------------------------------------ task origins

// Task origins as computed by the server (analysis::origins): tasks grouped
// by the source location of the construct that created them, the initial
// task in a group of its own. Adds the display fields.
function origins() {
  return S.g.origins.map(g => {
    const l = S.g.locs[g.loc];
    const kinds = [[1, 'parallel region'], [2, 'task'], [4, 'taskloop']].filter(([bit]) => g.k & bit).map(k => k[1]);
    return {
      key: g.initial ? 'initial' : g.loc, loc: g.loc, n: g.n, w: g.w, cr: g.cr, tasks: g.tasks,
      avg: g.n ? g.w / g.n : 0,   // average work per task
      // Average parallelism of these tasks alone, and measured peak.
      par: g.own_cr ? g.w / g.own_cr : 0, peak: g.peak,
      name: g.initial ? 'initial task' : l ? locShort(l) : 'unknown location',
      func: g.initial ? 'serial code outside parallel regions' : l ? (l.func || l.module) : '',
      // Function in the program; omitted where the location is already a function name.
      fn: !g.initial && l && l.file ? funcName(l) : '',
      // Text the filter field searches in.
      search: (g.initial ? 'initial task serial code' :
        [l && locShort(l), l && l.file, l && l.func, l && l.module, kinds.join(' ')].filter(Boolean).join('\n')).toLowerCase(),
      kind: g.initial ? 'initial' : kinds.join(' + '),
    };
  });
}

// Origins matching the filter field (all if it is empty).
function filteredOrigins() {
  const f = S.originFilter;
  return origins().filter(o => !f || o.search.includes(f));
}

function renderOrigins() {
  const g = S.g, { key, dir } = S.originSort, filtered = !!S.originFilter;
  const rows = filteredOrigins().sort((a, b) => key === 'name' ? dir * a.name.localeCompare(b.name) : dir * (a[key] - b[key]));
  const total = Math.max(g.work, 1), showDot = S.colorBy === 'loc';
  const head = (k, label, title) =>
    `<th data-k="${k}" title="${title}" class="sortable">${label}${k === key ? (dir < 0 ? ' ▾' : ' ▴') : ''}</th>`;
  $('origins').innerHTML = `<div class="scroll"><table class="origins">
    <thead><tr class="name">${head('name', 'Location', 'Source location of the task or parallel construct').replace('<th ', '<th colspan="6" ')}</tr>
      <tr>${head('n', 'Tasks', 'Number of tasks created there')}${head('avg', 'Avg.', 'Average work per task (work ÷ tasks)')}${head('w', 'Work', 'Total exclusive work time of these tasks')}${head('cr', 'Crit.', 'Part of that work on the critical path, and its share of the critical path length T∞')}${head('par', 'Par.', 'Average parallelism of these tasks alone: their work ÷ their own critical path')}${head('peak', 'Peak', 'Measured peak: the largest number of these tasks running at the same time')}</tr></thead>
    ${rows.map(o => {
      const dot = showDot ? `<span class="dot" style="background:${S.css.cat[(S.catSlots.get(o.loc) ?? 0) % 8]}"></span>` : '';
      const pct = 100 * o.w / total;
      // One tbody per origin: the location on its own line, the numbers below it.
      return `<tbody data-o="${esc(String(o.key))}" title="${esc(o.func)} · click to select these tasks">
        <tr class="name"><td colspan="6">${dot}<span class="mono">${esc(o.name)}</span>${o.fn ? ` <span class="fn">${esc(o.fn)}</span>` : ''} <span class="muted">· ${esc(o.kind)}</span>
          <div class="bar" style="width:${pct.toFixed(1)}%"></div></td></tr>
        <tr><td>${o.n}</td><td>${fmt(o.avg)}</td><td>${fmt(o.w)}<br><span class="muted">${pctText(o.w, total)} %</span></td>
        <td>${o.cr ? `${fmt(o.cr)}<br><span class="muted">${pctText(o.cr, g.critical)} %</span>` : '–'}</td>
        <td>${o.par ? o.par.toFixed(1) : '–'}</td><td>${o.peak}</td></tr></tbody>`;
    }).join('')}
    ${filtered ? matchesRow(rows, total) : `<tbody><tr class="name"><td colspan="6"><b>all tasks</b></td></tr><tr class="total"><td>${g.tasks.length}</td><td>${fmt(g.work / Math.max(g.tasks.length, 1))}</td><td>${fmt(g.work)}</td><td>${fmt(g.critical)}</td><td>${(g.work / Math.max(g.critical, 1)).toFixed(1)}</td><td>${g.peak}</td></tr></tbody>`}
  </table></div><p class="muted">Work is exclusive task time; Avg. is work per task. Percentages: share of T₁ under Work, share of the critical path T∞ under Crit. Par.: average parallelism of the row's tasks alone (work ÷ their own critical path); Peak: most of them running at once. Click a row to analyse its tasks as a region.</p>`;
}

// Total row while the filter is active: sums over the matching origins.
// Their joint parallelism and peak are not simple sums; select them to see.
function matchesRow(rows, total) {
  const g = S.g;
  if (!rows.length) return `<tbody><tr class="name"><td colspan="6" class="muted">No location matches “${esc(S.originFilter)}”.</td></tr></tbody>`;
  const n = rows.reduce((a, o) => a + o.n, 0), w = rows.reduce((a, o) => a + o.w, 0), cr = rows.reduce((a, o) => a + o.cr, 0);
  return `<tbody data-o="__matches" title="Click to select the tasks of all matching locations">
    <tr class="name"><td colspan="6"><b>${rows.length} matching location${rows.length === 1 ? '' : 's'}</b> <span class="muted">· click to select their tasks</span></td></tr>
    <tr class="total"><td>${n}</td><td>${fmt(w / Math.max(n, 1))}</td><td>${fmt(w)}<br><span class="muted">${pctText(w, total)} %</span></td>
    <td>${cr ? `${fmt(cr)}<br><span class="muted">${pctText(cr, g.critical)} %</span>` : '–'}</td><td>–</td><td>–</td></tr></tbody>`;
}

$('origin-filter').addEventListener('input', e => {
  S.originFilter = e.target.value.trim().toLowerCase();
  if (S.g) renderOrigins();
});

$('origins').addEventListener('click', e => {
  const th = e.target.closest('th[data-k]');
  if (th) {
    const k = th.dataset.k, s = S.originSort;
    S.originSort = { key: k, dir: s.key === k ? -s.dir : (k === 'name' ? 1 : -1) };
    return renderOrigins();
  }
  const tr = e.target.closest('tbody[data-o]');
  if (!tr) return;
  if (tr.dataset.o === '__matches') return setRegion(new Set(filteredOrigins().flatMap(o => o.tasks)));
  const o = origins().find(o => String(o.key) === tr.dataset.o);
  if (o) setRegion(new Set(o.tasks));
});

// ------------------------------------------------------------------ canvas

function resize() {
  const r = canvas.getBoundingClientRect(), d = window.devicePixelRatio || 1;
  base.width = canvas.width = Math.round(r.width * d); base.height = canvas.height = Math.round(r.height * d);
  view.setTransform(d, 0, 0, d, 0, 0); baseCtx.setTransform(d, 0, 0, d, 0, 0);
  S.dpr = d;
  S.w = r.width; S.h = r.height;
  draw();
}

function fit() {
  if (!S.g) return;
  if (S.view === 'timeline') { S.tl = { t0: 0, t1: Math.max(S.g.wall, 1) }; return draw(); }
  let x0 = Infinity, x1 = -Infinity, y1 = 0;
  for (const t of S.g.tasks) { x0 = Math.min(x0, t.x); x1 = Math.max(x1, t.x); y1 = Math.max(y1, t.y); }
  const w = (x1 - x0) * DX + 4 * R, h = y1 * DY + 4 * R;
  const k = Math.min(S.w / w, S.h / h, 1.6);
  S.cam = { k, x: S.w / 2 - ((x0 + x1) / 2) * DX * k, y: (S.h - y1 * DY * k) / 2 };
  draw();
}

function centerOn(i) {
  const t = S.g.tasks[i];
  if (S.view === 'graph') {
    S.cam.k = Math.max(S.cam.k, 0.9);
    S.cam.x = S.w / 2 - t.x * DX * S.cam.k; S.cam.y = S.h / 2 - t.y * DY * S.cam.k;
  }
  draw();
}

// Frame time display (key P or #perf): the time between consecutive frames
// while the view keeps changing, which includes the browser's own drawing
// work that JavaScript timing cannot see.
const perf = { on: false, last: 0, full: [], over: [], script: 0 };
function perfMeter(now, full) {
  if (!perf.on) return;
  const dt = now - perf.last;
  perf.last = now;
  if (dt > 1000) return;   // not a continuous interaction
  const list = full ? perf.full : perf.over;
  list.push(dt); if (list.length > 30) list.shift();
  const avg = l => l.length ? (l.reduce((a, b) => a + b, 0) / l.length).toFixed(0) + ' ms' : '–';
  // "script" is the JavaScript part of the last full render; the rest of a
  // full redraw frame is the browser rasterising the drawing commands.
  $('perf').textContent = `frame time: full redraw ${avg(perf.full)} (script ${perf.script.toFixed(0)} ms), move/overlay ${avg(perf.over)}`;
}
function togglePerf(on = !perf.on) {
  perf.on = on; perf.full = []; perf.over = [];
  $('perf').hidden = !on;
  $('perf').textContent = 'frame time: pan, zoom or hover to measure';
}

let raf = 0;
// Schedules a redraw. `mode`:
//   false   re-render the base (anything that changes the picture);
//   true    overlay only (hover, clicked task, selection rectangle);
//   'move'  the graph camera moved (pan, zoom): the rendered picture is
//           shifted and scaled, as in map viewers, and re-rendered sharp once
//           the movement pauses. A full render can take 100+ ms, a shifted
//           copy a few ms. The timeline always re-renders (cheap, and its
//           axis labels cannot be shifted).
let sharpTimer = 0;
function draw(mode = false) {
  if (mode === 'move' && S.view === 'graph' && S.baseValid && S.baseCam) {
    clearTimeout(sharpTimer);
    sharpTimer = setTimeout(() => draw(), 150);
  } else if (mode !== true) {
    S.baseValid = false;
    clearTimeout(sharpTimer);
  }
  if (raf) return;
  raf = requestAnimationFrame(now => {
    raf = 0;
    if (!S.g) return;
    perfMeter(now, !S.baseValid);
    if (!S.baseValid) {
      const t0 = performance.now();
      ctx = baseCtx;
      (S.view === 'graph' ? drawGraph : drawTimeline)();
      ctx = view;
      S.baseValid = true;
      S.baseCam = S.view === 'graph' ? { ...S.cam } : null;   // camera the base was rendered with
      perf.script = performance.now() - t0;
    }
    // Copy the base, shifted and scaled if the camera moved since it was rendered.
    const b = S.baseCam, s = b ? S.cam.k / b.k : 1;
    view.setTransform(1, 0, 0, 1, 0, 0);
    view.clearRect(0, 0, canvas.width, canvas.height);
    if (b) view.setTransform(s, 0, 0, s, (S.cam.x - b.x * s) * S.dpr, (S.cam.y - b.y * s) * S.dpr);
    view.drawImage(base, 0, 0);
    view.setTransform(S.dpr, 0, 0, S.dpr, 0, 0);
    (S.view === 'graph' ? graphOverlay : timelineOverlay)();
    drawBand();
  });
}

function drawGraph() {
  const { g, cam, css } = S, { k } = cam, T = g.tasks;
  ctx.clearRect(0, 0, S.w, S.h);
  const sx = t => t.x * DX * k + cam.x, sy = t => t.y * DY * k + cam.y;
  const r = Math.max(R * k, 1.2);

  // Edges, batched per style in paths of at most 1024 lines (one huge path
  // is slow to rasterise). An edge is skipped when both ends lie off the
  // same side.
  const pass = (pred, style, width, alpha) => {
    ctx.strokeStyle = style; ctx.lineWidth = width; ctx.globalAlpha = alpha;
    let count = 0;
    ctx.beginPath();
    for (const e of g.edges) {
      if (!pred(e)) continue;
      const a = T[e[0]], b = T[e[1]];
      const ax = sx(a), ay = sy(a), bx = sx(b), by = sy(b);
      if ((ax < 0 && bx < 0) || (ax > S.w && bx > S.w) || (ay < 0 && by < 0) || (ay > S.h && by > S.h)) continue;
      ctx.moveTo(ax, ay + r); ctx.lineTo(bx, by - r);
      if (++count === 1024) { ctx.stroke(); ctx.beginPath(); count = 0; }
    }
    ctx.stroke(); ctx.globalAlpha = 1;
  };
  const far = k < 0.25;
  const isCrit = e => S.showCrit && e[3];
  // With "grey out unselected", everything outside the selected region is
  // drawn first in grey; only edges between two selected tasks keep their style.
  const fade = S.fadeOthers && S.region.size > 0;
  const inSel = e => !fade || (S.region.has(e[0]) && S.region.has(e[1]));
  const shown = e => e[2] === 0 ? S.showCreate : S.showDep;
  // Creation edges of large fans are drawn as a bus (see drawFans); only
  // their critical path edges are drawn as direct lines.
  const direct = e => e[2] !== 0 || !S.fanChild[e[1]];
  const greyAlpha = far ? 0.25 : 0.4, createAlpha = far ? 0.5 : 0.9;
  if (fade) pass(e => shown(e) && !inSel(e) && direct(e), css.axis, 1, greyAlpha);
  if (S.showCreate) drawFans(sx, sy, r, fade, greyAlpha, createAlpha);
  if (S.showCreate) pass(e => e[2] === 0 && !isCrit(e) && inSel(e) && direct(e), css.edgeCreate, 1, createAlpha);
  if (S.showDep) pass(e => e[2] === 1 && !isCrit(e) && inSel(e), css.edgeDep, 1, far ? 0.35 : 0.7);
  const n = T.length;
  if (S.regionEdges.size) pass(e => S.regionEdges.has(e[0] * n + e[1]) || S.regionEdges.has(e[1] * n + e[0]), css.ink, 2.5, 1);
  if (S.showCrit) pass(e => e[3] && shown(e) && inSel(e), css.critical, Math.max(2, 3 * Math.min(k, 1)), 1);

  // Nodes, batched: one path per fill colour and one per kind of mark, so a
  // frame needs a few dozen canvas calls instead of one or more per task.
  // When greying out, the grey path is filled first, so the selected tasks
  // lie on top in their true colours; marks are drawn above all nodes.
  const simple = r < 2.5;
  const fills = new Map();   // fill colour -> Path2D, filled in insertion order
  const path = c => { let p = fills.get(c); if (!p) fills.set(c, p = new Path2D()); return p; };
  if (fade) path(css.axis);
  const regionRing = new Path2D(), regionCritRing = new Path2D(), critRing = new Path2D();
  const critSquare = new Path2D(), regionBar = new Path2D();
  for (let i = 0; i < T.length; i++) {
    const t = T[i], x = sx(t), y = sy(t);
    if (x < -r - 4 || x > S.w + r + 4 || y < -r - 4 || y > S.h + r + 4) continue;
    const inRegion = S.region.has(i);
    if (fade && !inRegion) { node(path(css.axis), t, x, y, r, simple); continue; }
    node(path(color(t)), t, x, y, r, simple);
    const crit = S.showCrit && t.cr > 0;
    if (simple) {
      if (crit) critSquare.rect(x - r - 1, y - r - 1, 2 * r + 2, 2 * r + 2);
      if (inRegion && !fade) regionBar.rect(x - r, y + r, 2 * r, 1.5);
    } else {
      // Ink rings mark the selection; with greying out only the region's
      // critical path keeps its (thick) ring.
      if (inRegion && (!fade || S.regionCrit.has(i))) shape(S.regionCrit.has(i) ? regionCritRing : regionRing, t, x, y, r + 2.5);
      if (crit) shape(critRing, t, x, y, r + (inRegion ? 6 : 2.5));
    }
  }
  for (const [c, p] of fills) { ctx.fillStyle = c; ctx.fill(p); }
  if (simple) {
    ctx.fillStyle = css.critical; ctx.fill(critSquare);
    ctx.fillStyle = css.ink; ctx.globalAlpha = 0.55; ctx.fill(regionBar); ctx.globalAlpha = 1;
  } else {
    ctx.strokeStyle = css.ink;
    ctx.lineWidth = 1.5; ctx.stroke(regionRing);
    ctx.lineWidth = 3.5; ctx.stroke(regionCritRing);
    ctx.strokeStyle = css.critical; ctx.lineWidth = 2; ctx.stroke(critRing);
  }
  if (k >= 1.1) {
    ctx.fillStyle = css.ink2; ctx.font = `${Math.min(11 * k, 13)}px system-ui, sans-serif`; ctx.textAlign = 'center'; ctx.textBaseline = 'top';
    for (const t of T) {
      const x = sx(t), y = sy(t);
      if (x > -30 && x < S.w + 30 && y > -30 && y < S.h + 30) ctx.fillText(t.id, x, y + r + 5);
    }
  }
}

// Rings around the hovered and the selected task.
function graphOverlay() {
  const { cam } = S, k = cam.k, r = Math.max(R * k, 1.2);
  for (const [i, w] of [[S.hover, 1.5], [S.sel, 3]]) {
    if (i < 0) continue;
    const t = S.g.tasks[i];
    ctx.lineWidth = w; ctx.strokeStyle = S.css.ink;
    ctx.beginPath(); ctx.arc(t.x * DX * k + cam.x, t.y * DY * k + cam.y, Math.max(r + 5, 7), 0, 6.2832); ctx.stroke();
  }
}

// Fans: parents with at least FAN children. Instead of one line per child,
// a fan is drawn as a trunk below the parent, a horizontal bus above each
// row of children, and a short stub down to every child, which needs a
// fraction of the pixels of thousands of long crossing lines.
const FAN = 8;
function findFans() {
  S.fans = [];
  S.fanChild = new Uint8Array(S.g.tasks.length);
  S.children.forEach((ch, p) => {
    if (ch.length < FAN) return;
    S.fans.push(p);
    for (const c of ch) S.fanChild[c] = 1;
  });
}
function drawFans(sx, sy, r, fade, greyAlpha, alpha) {
  const { css, cam } = S, T = S.g.tasks, gap = 0.3 * DY * cam.k;
  const styled = new Path2D(), grey = new Path2D();
  for (const p of S.fans) {
    const pt = T[p], px = sx(pt), py = sy(pt), pSel = !fade || S.region.has(p);
    // Children by row (layout y), with their horizontal extent.
    const rows = new Map();
    for (const c of S.children[p]) {
      const t = T[c];
      let row = rows.get(t.y);
      if (!row) rows.set(t.y, row = { y: sy(t), x0: Infinity, x1: -Infinity, kids: [] });
      const x = sx(t);
      row.x0 = Math.min(row.x0, x); row.x1 = Math.max(row.x1, x); row.kids.push([c, x]);
    }
    let deepest = py + r;
    for (const row of rows.values()) {
      const busY = Math.max(row.y - r - gap, (py + r + row.y - r) / 2);
      if (row.y + r < 0 || busY > S.h || Math.max(px, row.x1) < 0 || Math.min(px, row.x0) > S.w) continue;
      deepest = Math.max(deepest, busY);
      const bus = pSel ? styled : grey;
      bus.moveTo(Math.min(px, row.x0), busY); bus.lineTo(Math.max(px, row.x1), busY);
      for (const [c, x] of row.kids) {
        if (x < -2 || x > S.w + 2) continue;
        const stub = pSel && (!fade || S.region.has(c)) ? styled : grey;
        stub.moveTo(x, busY); stub.lineTo(x, row.y - r);
      }
    }
    if (deepest > py + r) (pSel ? styled : grey).moveTo(px, py + r), (pSel ? styled : grey).lineTo(px, deepest);
  }
  ctx.lineWidth = 1;
  ctx.strokeStyle = css.axis; ctx.globalAlpha = greyAlpha; ctx.stroke(grey);
  ctx.strokeStyle = css.edgeCreate; ctx.globalAlpha = alpha; ctx.stroke(styled);
  ctx.globalAlpha = 1;
}

// Explicit tasks are circles, implicit tasks squares, the initial task a
// diamond. Adds the outline as a closed subpath to `p` (a Path2D or ctx).
function shape(p, t, x, y, r) {
  if (t.k === 2) { p.moveTo(x + r, y); p.arc(x, y, r, 0, 6.2832); }
  else if (t.k === 1) p.roundRect(x - r, y - r, 2 * r, 2 * r, r * 0.25);
  else { p.moveTo(x, y - r * 1.2); p.lineTo(x + r * 1.2, y); p.lineTo(x, y + r * 1.2); p.lineTo(x - r * 1.2, y); p.closePath(); }
}
// A node: a small square when zoomed out, its shape otherwise.
function node(p, t, x, y, r, simple) {
  if (simple) p.rect(x - r, y - r, 2 * r, 2 * r);
  else shape(p, t, x, y, r);
}

function drawBand() {
  const d = S.drag;
  if (!d || !d.band) return;
  ctx.fillStyle = S.css.ink; ctx.globalAlpha = 0.08;
  const x = Math.min(d.x0, d.x), y = S.view === 'graph' ? Math.min(d.y0, d.y) : 0;
  const w = Math.abs(d.x - d.x0), h = S.view === 'graph' ? Math.abs(d.y - d.y0) : S.h;
  ctx.fillRect(x, y, w, h); ctx.globalAlpha = 1;
  ctx.strokeStyle = S.css.ink; ctx.lineWidth = 1; ctx.strokeRect(x + .5, y + .5, w, h);
}

// ------------------------------------------------------------------ timeline

const TL = { left: 86, top: 30, row: 30, bar: 20 };
const tlX = t => TL.left + (t - S.tl.t0) / (S.tl.t1 - S.tl.t0) * (S.w - TL.left - 12);
const tlT = x => S.tl.t0 + (x - TL.left) / (S.w - TL.left - 12) * (S.tl.t1 - S.tl.t0);

function drawTimeline() {
  const { g, css } = S;
  ctx.clearRect(0, 0, S.w, S.h);
  ctx.font = '11px system-ui, sans-serif'; ctx.textBaseline = 'middle';
  // Time axis: about 8 round ticks.
  const span = S.tl.t1 - S.tl.t0, raw = span / 8, mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const step = [1, 2, 5, 10].map(m => m * mag).find(s => s >= raw);
  ctx.strokeStyle = css.grid; ctx.fillStyle = css.muted; ctx.lineWidth = 1; ctx.textAlign = 'center';
  ctx.beginPath();
  for (let t = Math.ceil(S.tl.t0 / step) * step; t <= S.tl.t1; t += step) {
    const x = Math.round(tlX(t)) + .5;
    ctx.moveTo(x, TL.top - 6); ctx.lineTo(x, TL.top + g.threads.length * TL.row);
    ctx.fillText(fmt(t), x, 12);
  }
  ctx.stroke();
  const rowOf = new Map(g.threads.map((t, i) => [t.id, i]));
  ctx.textAlign = 'left'; ctx.fillStyle = css.ink2;
  g.threads.forEach((t, i) => ctx.fillText(`thread ${t.id}`, 10, TL.top + i * TL.row + TL.row / 2));

  ctx.save(); ctx.beginPath(); ctx.rect(TL.left, 0, S.w - TL.left, S.h); ctx.clip();
  // Batched like the graph: one path per fill colour and per kind of mark.
  const fills = new Map(), path = c => { let p = fills.get(c); if (!p) fills.set(c, p = new Path2D()); return p; };
  const fade = S.fadeOthers && S.region.size > 0;
  if (fade) path(css.axis);
  const critStrip = new Path2D(), regionBar = new Path2D();
  g.tasks.forEach((t, i) => {
    const inRegion = S.region.has(i), grey = fade && !inRegion;
    for (const s of t.segs) {
      if (s[5] < S.tl.t0 || s[4] > S.tl.t1) continue;
      const row = rowOf.get(s[0]); if (row === undefined) continue;
      const x = tlX(s[4]), w = Math.max(tlX(s[5]) - x, 0.6), y = TL.top + row * TL.row + (TL.row - TL.bar) / 2;
      // A 1px surface gap separates adjacent segments once they are wide enough.
      const wg = w > 3 ? w - 1 : w;
      path(grey ? css.axis : color(t)).rect(x, y, wg, TL.bar);
      if (grey) continue;
      if (S.showCrit && t.cr > 0) critStrip.rect(x, y + TL.bar + 1, wg, 3);
      if (inRegion && !S.fadeOthers) regionBar.rect(x, y - 4, wg, 2);
    }
  });
  for (const [c, p] of fills) { ctx.fillStyle = c; ctx.fill(p); }
  ctx.fillStyle = css.critical; ctx.fill(critStrip);
  ctx.fillStyle = css.ink; ctx.fill(regionBar);
  ctx.restore();
}

// Outlines of the hovered and the selected task's segments.
function timelineOverlay() {
  const rowOf = new Map(S.g.threads.map((t, i) => [t.id, i]));
  ctx.save(); ctx.beginPath(); ctx.rect(TL.left, 0, S.w - TL.left, S.h); ctx.clip();
  ctx.strokeStyle = S.css.ink;
  for (const [i, lw] of [[S.hover, 1], [S.sel, 2]]) {
    if (i < 0) continue;
    ctx.lineWidth = lw;
    for (const s of S.g.tasks[i].segs) {
      if (s[5] < S.tl.t0 || s[4] > S.tl.t1) continue;
      const row = rowOf.get(s[0]); if (row === undefined) continue;
      const x = tlX(s[4]), w = Math.max(tlX(s[5]) - x, 0.6), y = TL.top + row * TL.row + (TL.row - TL.bar) / 2;
      ctx.strokeRect(x - 1, y - 1, w + 1, TL.bar + 2);
    }
  }
  ctx.restore();
}

// ------------------------------------------------------------------ picking

function pick(mx, my) {
  const T = S.g.tasks;
  if (S.view === 'graph') {
    const { cam } = S, lim = Math.max(R * cam.k + 4, 6);
    let best = -1, bd = lim * lim;
    for (let i = 0; i < T.length; i++) {
      const dx = T[i].x * DX * cam.k + cam.x - mx, dy = T[i].y * DY * cam.k + cam.y - my, d = dx * dx + dy * dy;
      if (d < bd) { bd = d; best = i; }
    }
    return best;
  }
  const row = Math.floor((my - TL.top) / TL.row), th = S.g.threads[row];
  if (!th || mx < TL.left) return -1;
  const t = tlT(mx), slack = (S.tl.t1 - S.tl.t0) / S.w;
  for (let i = 0; i < T.length; i++)
    for (const s of T[i].segs) if (s[0] === th.id && s[4] - slack <= t && t <= s[5] + slack) return i;
  return -1;
}

function showTip(i, mx, my) {
  const tip = $('tip');
  if (i < 0) { tip.hidden = true; return; }
  const t = S.g.tasks[i];
  tip.innerHTML = `<b>task ${t.id}</b> · ${KIND[t.k]} · ${fmt(t.w)}<br>${esc(locShort(S.g.locs[t.loc]))}`;
  tip.hidden = false;
  tip.style.left = Math.min(mx + 14, S.w - tip.offsetWidth - 6) + 'px';
  tip.style.top = Math.max(my - tip.offsetHeight - 10, 4) + 'px';
}

// ------------------------------------------------------------------ input

const pos = e => { const r = canvas.getBoundingClientRect(); return [e.clientX - r.left, e.clientY - r.top]; };

canvas.addEventListener('pointerdown', e => {
  if (!S.g) return;
  const [x, y] = pos(e);
  S.drag = { x0: x, y0: y, x, y, band: e.shiftKey, moved: false, cam: { ...S.cam }, tl: { ...S.tl } };
  canvas.setPointerCapture(e.pointerId);
  canvas.classList.add(e.shiftKey ? 'selecting' : 'dragging');
});
canvas.addEventListener('pointermove', e => {
  if (!S.g) return;
  const [x, y] = pos(e), d = S.drag;
  if (!d) {
    const h = pick(x, y);
    if (h !== S.hover) { S.hover = h; draw(true); }
    return showTip(h, x, y);
  }
  d.x = x; d.y = y;
  if (Math.abs(x - d.x0) + Math.abs(y - d.y0) > 3) d.moved = true;
  if (!d.band && d.moved) {
    if (S.view === 'graph') { S.cam.x = d.cam.x + x - d.x0; S.cam.y = d.cam.y + y - d.y0; }
    else {
      const dt = (x - d.x0) / (S.w - TL.left - 12) * (d.tl.t1 - d.tl.t0);
      S.tl = { t0: d.tl.t0 - dt, t1: d.tl.t1 - dt };
    }
  }
  $('tip').hidden = true;
  draw(d.band ? true : 'move');   // a selection rectangle only changes the overlay
});
canvas.addEventListener('pointerup', e => {
  const d = S.drag; S.drag = null;
  canvas.classList.remove('dragging', 'selecting');
  if (!d || !S.g) return;
  if (!d.moved) { select(pick(d.x, d.y)); return; }
  if (d.band) {
    const sel = new Set(), T = S.g.tasks;
    if (S.view === 'graph') {
      const [x0, x1, y0, y1] = [Math.min(d.x0, d.x), Math.max(d.x0, d.x), Math.min(d.y0, d.y), Math.max(d.y0, d.y)];
      T.forEach((t, i) => {
        const x = t.x * DX * S.cam.k + S.cam.x, y = t.y * DY * S.cam.k + S.cam.y;
        if (x >= x0 && x <= x1 && y >= y0 && y <= y1) sel.add(i);
      });
    } else {
      const [t0, t1] = [tlT(Math.min(d.x0, d.x)), tlT(Math.max(d.x0, d.x))];
      T.forEach((t, i) => { if (t.segs.some(s => s[5] >= t0 && s[4] <= t1)) sel.add(i); });
    }
    setRegion(sel);
  }
  draw();
});
canvas.addEventListener('pointerleave', () => { if (S.hover >= 0) { S.hover = -1; draw(true); } $('tip').hidden = true; });
canvas.addEventListener('wheel', e => {
  e.preventDefault();
  if (!S.g) return;
  const [x, y] = pos(e), f = Math.exp(-e.deltaY * 0.0015);
  if (S.view === 'graph') {
    const k = Math.min(Math.max(S.cam.k * f, 0.01), 6), r = k / S.cam.k;
    S.cam = { k, x: x - (x - S.cam.x) * r, y: y - (y - S.cam.y) * r };
  } else {
    const t = tlT(x), span = Math.max((S.tl.t1 - S.tl.t0) / f, 1000), frac = (t - S.tl.t0) / (S.tl.t1 - S.tl.t0);
    S.tl = { t0: t - span * frac, t1: t + span * (1 - frac) };
  }
  draw('move');
}, { passive: false });

function select(i) { S.sel = i; renderTask(); draw(true); }   // only the ring changes

$('rank').onchange = e => loadRank(e.target.value);
$('color').onchange = e => { S.colorBy = e.target.value; assignSlots(); renderLegend(); renderOrigins(); renderTask(); draw(); };
for (const [id, key] of [['show-crit', 'showCrit'], ['show-create', 'showCreate'], ['show-dep', 'showDep'], ['fade-others', 'fadeOthers']])
  $(id).onchange = e => { S[key] = e.target.checked; renderLegend(); draw(); };
for (const v of ['graph', 'timeline']) $('view-' + v).onclick = () => {
  S.view = v;
  $('view-graph').setAttribute('aria-selected', v === 'graph'); $('view-timeline').setAttribute('aria-selected', v === 'timeline');
  $('hint').innerHTML = v === 'graph'
    ? 'Drag to pan · wheel to zoom · click a task · <kbd>Shift</kbd>+drag to select a region · <kbd>Esc</kbd> to clear'
    : 'Drag to pan · wheel to zoom time · click a segment · <kbd>Shift</kbd>+drag to select a time range · <kbd>Esc</kbd> to clear';
  draw();
};
$('fit').onclick = fit;
$('find').onchange = e => {
  const i = S.byId.get(Number(e.target.value));
  if (i !== undefined) { select(i); centerOn(i); }
};
window.addEventListener('keydown', e => {
  if (['INPUT', 'SELECT', 'TEXTAREA'].includes(e.target.tagName) || e.ctrlKey || e.metaKey || e.altKey) return;
  if (e.key === 'f') fit();
  if (e.key === 'p') togglePerf();
  if (e.key === 'Escape') {
    // Cancel a selection rectangle being drawn; otherwise clear the region
    // selection and the clicked task.
    if (S.drag) {
      S.drag = null;
      canvas.classList.remove('dragging', 'selecting');
      return draw();
    }
    if (S.region.size) setRegion(new Set());
    if (S.sel >= 0) select(-1);
  }
});
window.addEventListener('resize', () => { setAsideWidth(asideWidth()); resize(); });

// ------------------------------------------------------------------ side panel width

const splitter = $('splitter'), ASIDE_DEFAULT = 400, ASIDE_MIN = 260;
const asideWidth = () => document.querySelector('aside').getBoundingClientRect().width;
function setAsideWidth(w, save = false) {
  w = Math.round(Math.min(Math.max(w, ASIDE_MIN), Math.max(ASIDE_MIN, window.innerWidth * 0.7)));
  document.documentElement.style.setProperty('--aside-w', w + 'px');
  splitter.setAttribute('aria-valuenow', w);
  if (save) try { localStorage.setItem('temanejo.asideWidth', w); } catch { /* storage unavailable */ }
  resize();
}
splitter.addEventListener('pointerdown', e => {
  splitter.setPointerCapture(e.pointerId);
  splitter.classList.add('active'); document.body.classList.add('resizing');
});
splitter.addEventListener('pointermove', e => {
  if (splitter.hasPointerCapture(e.pointerId)) setAsideWidth(window.innerWidth - e.clientX);
});
splitter.addEventListener('pointerup', e => {
  splitter.releasePointerCapture(e.pointerId);
  splitter.classList.remove('active'); document.body.classList.remove('resizing');
  setAsideWidth(asideWidth(), true);
});
splitter.addEventListener('dblclick', () => {
  try { localStorage.removeItem('temanejo.asideWidth'); } catch { /* storage unavailable */ }
  setAsideWidth(ASIDE_DEFAULT);
});
splitter.addEventListener('keydown', e => {
  const step = e.shiftKey ? 80 : 20;
  if (e.key === 'ArrowLeft') setAsideWidth(asideWidth() + step, true);
  else if (e.key === 'ArrowRight') setAsideWidth(asideWidth() - step, true);
  else return;
  e.preventDefault();
});
try {
  const saved = Number(localStorage.getItem('temanejo.asideWidth'));
  if (saved) document.documentElement.style.setProperty('--aside-w', saved + 'px');
} catch { /* storage unavailable */ }
window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => { readCss(); draw(); });

// ------------------------------------------------------------------ legend

function renderLegend() {
  const g = S.g; if (!g) return;
  let html = '';
  const seq = SEQUENTIAL[S.colorBy];
  if (seq) {
    const { min, max } = S.seqRange;
    html = `<span>${seq.label} ${fmt(min)}<span class="ramp"></span>${fmt(max)}${seq.log ? ' (log scale)' : ''}</span>`;
  } else {
    const vals = [...S.catSlots.keys()];
    html = vals.slice(0, 8).map(v => `<span><span class="sw" style="background:${S.css.cat[S.catSlots.get(v) % 8]}"></span>${esc(catLabel(v))}</span>`).join('');
    if (vals.length > 8) html += `<span class="muted">+${vals.length - 8} more reuse these colours – see the task details</span>`;
  }
  if (S.showCrit) html += `<span><span class="sw ring"></span>critical path</span>`;
  if (S.fadeOthers && S.region.size) html += `<span><span class="sw" style="background:${S.css.axis}"></span>not selected</span>`;
  html += `<span class="muted">○ explicit · □ implicit · ◇ initial</span>`;
  $('legend').innerHTML = html;
}

// ------------------------------------------------------------------ task details

function taskLink(i) { return i < 0 ? '–' : `<a class="t" data-t="${i}">${S.g.tasks[i].id}</a>`; }

function renderTask() {
  const el = $('task'), g = S.g;
  if (!g || S.sel < 0) { el.innerHTML = '<p class="muted">Click a task to see its details.</p>'; return; }
  const t = g.tasks[S.sel], l = g.locs[t.loc], p = g.parallels[t.pr];
  const threads = [...new Set(t.segs.map(s => s[0]))], cpus = [...new Set(t.segs.flatMap(s => [s[1], s[2]]))];
  const first = t.segs[0], start = first ? first[4] : t.ct;
  const badges = [`<span class="badge">${KIND[t.k]}</span>`];
  if (t.tl) badges.push(`<span class="badge" title="Created by the runtime for a taskloop construct; this may also be an internal helper task of the runtime">taskloop</span>`);
  for (const [bit, name] of FLAGS) if (t.fl & bit) badges.push(`<span class="badge">${name}</span>`);
  if (t.cr > 0) badges.push(`<span class="badge crit">critical path</span>`);
  if (t.k === 2 && first && first[0] !== t.cth) badges.push(`<span class="badge">ran on another thread than its creator</span>`);
  if (threads.length > 1) badges.push(`<span class="badge">migrated between threads</span>`);
  if (cpus.length > 1) badges.push(`<span class="badge">moved between CPUs</span>`);

  let src = '<span class="muted">unknown</span>';
  if (l) src = (l.file ? `<span class="mono">${esc(l.file)}:${l.line}</span>` : `<span class="mono">${esc(l.module)}</span> <span class="muted">(no debug info)</span>`) +
    ` <span class="muted mono">${esc(l.addr)}</span>`;
  const fn = funcName(l), outlined = fn && l.func !== fn;
  const func = !l ? '–' : fn
    ? `<span class="fn">${esc(fn)}</span>` + (outlined ? `<br><span class="muted">inside an OpenMP construct; compiled as <span class="mono">${esc(l.func)}</span></span>` : '')
    : '<span class="muted">unknown</span>';
  // Where the task's parallel region starts.
  const pl = p ? S.g.locs[p.loc] : null;
  const regionAt = pl ? ` <span class="muted">at</span> <span class="mono">${esc(locShort(pl))}</span>${funcName(pl) && pl.file ? ` <span class="muted">in</span> <span class="fn">${esc(funcName(pl))}</span>` : ''}` : '';

  const list = a => a.length ? a.slice(0, 40).map(taskLink).join(', ') + (a.length > 40 ? ` … (${a.length})` : '') : '–';
  let html = `<div><b style="font-size:15px">Task ${t.id}</b></div><div style="margin:4px 0 8px">${badges.join('')}</div><dl>
    <dt>Source</dt><dd>${src}</dd>
    <dt>Function</dt><dd>${func}</dd>
    <dt>Work time</dt><dd>${fmt(t.w)} <span class="muted">exclusive, in ${t.segs.length} segment${t.segs.length === 1 ? '' : 's'}</span></dd>
    <dt>Lifetime</dt><dd>${fmt(start)} → ${fmt(t.e)} <span class="muted">(${fmt(Math.max(t.e - start, 0))})</span></dd>
    <dt>On critical path</dt><dd>${t.cr > 0 ? fmt(t.cr) + ` <span class="muted">(${(100 * t.cr / g.critical).toFixed(1)} % of T∞)</span>` : 'no'}</dd>
    <dt>Created</dt><dd>at ${fmt(t.ct)} by ${t.par < 0 ? 'the runtime' : 'task ' + taskLink(t.par)} on thread ${t.cth}${first && t.k === 2 ? ` <span class="muted">· waited ${fmt(Math.max(first[4] - t.ct, 0))}</span>` : ''}</dd>
    <dt>Parallel region</dt><dd>${p ? `${p.id}${regionAt}<br><span class="muted">team of ${p.actual}${t.k === 1 ? ', thread number ' + t.ix : ''}${p.parent >= 0 ? ', forked by task' : ''}</span> ${p.parent >= 0 ? taskLink(p.parent) : ''}` : '–'}</dd>
    <dt>Threads</dt><dd>${threads.join(', ') || '–'}</dd>
    <dt>CPUs</dt><dd>${cpus.join(', ') || '–'} <span class="muted">NUMA ${[...new Set(t.segs.map(s => s[3]))].join(', ') || '–'}</span></dd>
    <dt>Children</dt><dd>${S.children[S.sel].length}</dd>
    <dt>Depends on</dt><dd>${list(S.pred[S.sel])}</dd>
    <dt>Required by</dt><dd>${list(S.succ[S.sel])}</dd>
  </dl>
  <div class="actions"><button data-a="center">Centre</button><button data-a="subtree">Select subtree</button>${p ? '<button data-a="region">Select parallel region</button>' : ''}</div>`;

  if (t.deps.length) html += `<h3>depend clause items</h3><table><tr><th>type</th><th>address</th></tr>` +
    t.deps.map(d => `<tr><td>${DEP_TYPE[d[1]] || d[1]}</td><td class="mono">${d[0]}</td></tr>`).join('') + '</table>';

  if (t.segs.length) {
    const rows = t.segs.slice(0, 200).map(s => `<tr><td>${s[0]}</td><td>${s[1] === s[2] ? s[1] : s[1] + ' → ' + s[2]}</td><td>${s[3]}</td><td>${fmt(s[4])}</td><td>${fmt(s[5] - s[4])}</td></tr>`).join('');
    html += `<h3>Execution segments</h3><table><tr><th>thread</th><th>CPU</th><th>NUMA</th><th>start</th><th>duration</th></tr>${rows}</table>` +
      (t.segs.length > 200 ? `<p class="muted">${t.segs.length - 200} more segments not shown.</p>` : '');
  }
  el.innerHTML = html;
}

$('task').addEventListener('click', e => {
  const link = e.target.closest('a.t'), btn = e.target.closest('button');
  if (link) { const i = Number(link.dataset.t); select(i); centerOn(i); }
  if (!btn || S.sel < 0) return;
  if (btn.dataset.a === 'center') centerOn(S.sel);
  if (btn.dataset.a === 'subtree') {
    const sel = new Set(), stack = [S.sel];
    while (stack.length) { const i = stack.pop(); if (!sel.has(i)) { sel.add(i); stack.push(...S.children[i]); } }
    setRegion(sel);
  }
  if (btn.dataset.a === 'region') {
    // All tasks of the region, plus everything they spawn (nested regions included).
    const pr = S.g.tasks[S.sel].pr, sel = new Set(), stack = [];
    S.g.tasks.forEach((t, i) => { if (t.pr === pr) stack.push(i); });
    while (stack.length) { const i = stack.pop(); if (!sel.has(i)) { sel.add(i); stack.push(...S.children[i]); } }
    setRegion(sel);
  }
});

// ------------------------------------------------------------------ region analysis

async function setRegion(sel) {
  S.region = sel; S.regionInfo = null; S.regionCrit = new Map(); S.regionEdges = new Set();
  renderLegend();
  const el = $('region');
  if (!sel.size) {
    el.innerHTML = '<p class="muted">Shift+drag over the graph (or a time range in the timeline) to analyse the degree of parallelism of a region.</p>';
    return draw();
  }
  el.innerHTML = '<p class="muted">Analysing…</p>';
  draw();
  const rank = S.g.rank;
  const r = await (await fetch(`api/rank/${rank}/region`, { method: 'POST', body: [...sel].join(',') })).json();
  if (S.region !== sel) return; // superseded by a newer selection
  S.regionInfo = r;
  S.regionCrit = new Map(r.crit_tasks);
  S.regionEdges = new Set(r.crit_edges.map(e => e[0] * S.g.tasks.length + e[1]));
  renderRegion();
  draw();
}

function renderRegion() {
  const r = S.regionInfo, el = $('region');
  const span = Math.max(r.t_end - r.t_begin, 1), par = r.work / Math.max(r.critical, 1);
  const tile = (v, l, cls = '') => `<div class="tile ${cls}"><b>${v}</b><span>${l}</span></div>`;
  const total = r.per_thread.reduce((a, p) => a + p[1], 0) || 1;
  el.innerHTML = `<div class="tiles">` +
    tile(par.toFixed(2), 'average parallelism of the region (T₁ ÷ T∞)', 'hero') +
    tile(r.n, 'tasks') + tile(fmt(span), 'wall-clock span') +
    tile(fmt(r.work), 'work T₁') + tile(fmt(r.critical), 'critical path T∞') +
    tile((r.work / span).toFixed(2), 'measured avg. concurrency') + tile(r.max, 'measured peak concurrency') +
    `</div>
    <p class="muted">Tasks with a thick outline form the region's critical path (${r.crit_tasks.length} tasks).</p>
    <h3>Tasks of the region executing concurrently, over time</h3>
    <div class="chart" id="chart"></div>
    <h3>Work per thread</h3>
    <table>${r.per_thread.map(([th, w]) => `<tr><td>thread ${th}</td><td style="width:45%"><div class="bar" style="width:${(100 * w / total).toFixed(1)}%"></div></td><td>${fmt(w)}</td><td>${(100 * w / total).toFixed(0)} %</td></tr>`).join('')}</table>
    <div class="actions"><button id="clear-region" title="Esc">Clear selection</button></div>`;
  $('clear-region').onclick = () => setRegion(new Set());
  renderChart(r);
}

// Step-area chart of measured concurrency with crosshair tooltip.
function renderChart(r) {
  const el = $('chart'), W = 330, H = 130, L = 26, B = 18, T = 8, Rt = 6;
  const pts = r.profile;
  if (!pts.length) { el.innerHTML = '<p class="muted">No execution recorded for this selection.</p>'; return; }
  const ymax = Math.max(1, Math.ceil(Math.max(...pts.map(p => p[1]))));
  const bw = (W - L - Rt) / pts.length;
  const X = i => L + i * bw, Y = v => T + (H - T - B) * (1 - v / ymax);
  let area = `M${X(0)},${Y(0)}`, line = '';
  pts.forEach((p, i) => {
    const y = Y(p[1]).toFixed(1), x1 = X(i + 1).toFixed(1);
    area += `V${y}H${x1}`;
    line += (i ? `V${y}` : `M${X(0)},${y}`) + `H${x1}`;
  });
  const ticks = ymax <= 4 ? [...Array(ymax + 1).keys()] : [0, Math.round(ymax / 2), ymax];
  el.innerHTML = `<svg viewBox="0 0 ${W} ${H}" role="img" aria-label="Concurrency over time">
    ${ticks.map(v => `<line x1="${L}" x2="${W - Rt}" y1="${Y(v)}" y2="${Y(v)}" stroke="var(--grid)"/><text x="${L - 5}" y="${Y(v) + 3.5}" text-anchor="end">${v}</text>`).join('')}
    <path d="${area}V${Y(0)}Z" fill="var(--s1)" opacity=".18"/>
    <path d="${line}" fill="none" stroke="var(--s1)" stroke-width="2" stroke-linejoin="round"/>
    <line x1="${L}" x2="${W - Rt}" y1="${Y(0)}" y2="${Y(0)}" stroke="var(--axis)"/>
    <text x="${L}" y="${H - 4}">${fmt(r.t_begin)}</text><text x="${W - Rt}" y="${H - 4}" text-anchor="end">${fmt(r.t_end)}</text>
    <line id="xh" y1="${T}" y2="${Y(0)}" stroke="var(--ink2)" visibility="hidden"/>
  </svg><div id="ctip" class="muted" style="min-height:1.5em">Hover the chart for values.</div>`;
  const svg = el.querySelector('svg'), xh = svg.querySelector('#xh');
  svg.onpointermove = e => {
    const b = svg.getBoundingClientRect(), x = (e.clientX - b.left) / b.width * W;
    const i = Math.min(pts.length - 1, Math.max(0, Math.floor((x - L) / bw)));
    xh.setAttribute('x1', X(i + .5)); xh.setAttribute('x2', X(i + .5)); xh.setAttribute('visibility', 'visible');
    $('ctip').textContent = `${fmt(pts[i][0])}: ${pts[i][1].toFixed(2)} tasks executing`;
  };
  svg.onpointerleave = () => { xh.setAttribute('visibility', 'hidden'); $('ctip').textContent = 'Hover the chart for values.'; };
}

// ------------------------------------------------------------------ start

// Deep links: #rank=1&view=timeline&color=loc&task=<id>&select=subtree|region
async function applyHash() {
  const q = new URLSearchParams(location.hash.slice(1));
  if (q.has('rank') && String(S.g.rank) !== q.get('rank')) { $('rank').value = q.get('rank'); await loadRank(q.get('rank')); }
  if (q.has('color')) { $('color').value = q.get('color'); $('color').onchange({ target: $('color') }); }
  if (q.get('view') === 'timeline') $('view-timeline').onclick();
  if (q.has('perf')) togglePerf(true);
  if (q.get('select') === 'critical') setRegion(criticalTasks());
  const i = S.byId.get(Number(q.get('task')));
  if (i !== undefined) {
    select(i); centerOn(i);
    const btn = $('task').querySelector(`button[data-a="${q.get('select')}"]`);
    if (btn) btn.click();
  }
}

readCss();
resize();
loadRanks().then(applyHash).catch(e => { $('task').innerHTML = `<p>Cannot load trace data: ${esc(e.message)}</p>`; });
