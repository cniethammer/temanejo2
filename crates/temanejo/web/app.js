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
//   i    index of the task (position in S.g.tasks)
//
// The graph is loaded with all groups collapsed (graph_json in main.rs).
// Groups form a tree: expanding one shows its direct tasks and child groups,
// never more than the server's size limit (S.g.max_group). S.g.tasks has an
// entry for every task, but tasks inside groups start as stubs with only id,
// parent and critical time (`loaded` is false); the records of a group's
// direct tasks are fetched when it is first expanded (loadGroup). The
// dependence edges between the visible nodes come from the server
// (refreshView); creation edges are derived from the parents.
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
  sel: -1, region: new Set(), regionInfo: null, regionCrit: new Map(), regionPairs: [],
  cam: { x: 0, y: 0, k: 1 },           // graph camera: screen = world * k + (x, y)
  tl: { t0: 0, t1: 1 },                // visible time window of the timeline
  drag: null, hover: -1, hoverGroup: -1, css: {}, children: [], succ: [], pred: [],
  groups: [], hidden: new Int32Array(0), repGroup: new Int32Array(0), groupOf: new Int32Array(0), vEdges: [],
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
    gap: [v('--critical'), v('--gap-busy'), v('--gap-wait'), v('--gap-idle')],
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

// Colour for a task's value in the current colour mode, as the server
// reports it for timeline items (see timeline_json in main.rs).
function colorOfValue(v) {
  const seq = SEQUENTIAL[S.colorBy];
  if (seq) {
    const { lo, hi } = S.seqRange, x = seq.log ? Math.log(Math.max(v, 1)) : v;
    return seqColor(hi > lo ? Math.min(1, Math.max(0, (x - lo) / (hi - lo))) : 1);
  }
  const slot = S.catSlots.get(v);
  return slot === undefined ? S.css.muted : S.css.cat[slot % 8];
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
  // Categories and ranges cover all tasks, loaded or not (computed by the
  // server), so colours stay the same while groups are loaded.
  S.catSlots = new Map((S.g.cats[S.colorBy] || []).map((v, i) => [v, i]));
  const seq = SEQUENTIAL[S.colorBy];
  if (seq) {
    let [min, max] = S.g.ranges[S.colorBy];
    if (seq.log) { min = Math.max(min, 1); max = Math.max(max, 1); }
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
  $('rank').innerHTML = ranks.map(r => `<option value="${r.rank}">${r.rank} – ${esc(r.host)} (${r.tasks} tasks${r.complete ? '' : ', incomplete'})</option>`).join('');
  if (ranks.length) await loadRank(ranks[0].rank);
}

async function loadRank(rank, maxGroup) {
  const g = await (await fetch(`api/rank/${rank}/graph` + (maxGroup ? `?max_group=${maxGroup}` : ''))).json();
  S.g = g;
  S.sel = -1; S.hover = -1;
  S.tlData = null; S.selSerial = 0;
  const n = g.n;
  $('max-group').value = g.max_group;
  // Every task gets an entry; those inside groups are stubs until loaded.
  const records = g.tasks;
  g.tasks = g.ids.map((id, i) => ({ i, id, par: g.par[i], k: 2, fl: 0, pr: -1, ix: 0, ct: 0, cth: 0, loc: -1, e: 0, w: 0, tl: 0, cr: 0, segs: [], deps: [], dp: [], ds: [], x: 0, y: 0, loaded: false }));
  for (const r of records) Object.assign(g.tasks[r.i], r, { loaded: true });
  for (const [i, cr] of g.crit) g.tasks[i].cr = cr;
  S.byId = new Map(g.ids.map((id, i) => [id, i]));
  // Creation edges are the parent relation, known for all tasks.
  const critCreate = new Set(g.cc);
  S.children = Array.from({ length: n }, () => []);
  g.edges = [];
  g.par.forEach((p, i) => { if (p >= 0) { S.children[p].push(i); g.edges.push([p, i, 0, critCreate.has(i) ? 1 : 0]); } });
  S.depView = [];   // dependence edges between the visible nodes, see refreshView
  S.tl = { t0: 0, t1: Math.max(g.wall, 1) };
  assignSlots();
  S.hoverGroup = -1; S.region = new Set(); S.regionPairs = [];
  // The server's group tree, all collapsed. `tasks` are a group's direct
  // tasks, `sub` its child groups.
  S.groups = g.groups.map((sg, id) => ({ id, kind: sg.kind, anchor: sg.anchor, loc: sg.loc, parent: sg.parent, rep: sg.rep, part: sg.part,
    sum: sg.sum, n: sg.sum.n, cr: sg.sum.cr, tasks: sg.tasks, sub: sg.sub, members: null,
    collapsed: true, drawn: false, inRegion: false, server: true, loaded: false, loading: null }));
  S.groupOf = new Int32Array(n).fill(-1);   // group a task is a direct member of
  for (const grp of S.groups) for (const i of grp.tasks) S.groupOf[i] = grp.id;
  await refreshView();
  if (S.g !== g) return;
  rebuildVisible();
  setRegion(new Set());
  renderSummary();
  renderOrigins();
  renderTask();
  renderLegend();
  fit();
}

// All tasks of a group: its direct tasks and those of its child groups.
// Groups made in the browser ("Collapse subtree") have an explicit list.
function membersOf(grp) {
  if (grp.members) return grp.members;
  const out = [], stack = [grp];
  while (stack.length) {
    const x = stack.pop();
    for (const i of x.tasks) out.push(i);
    for (const c of x.sub) stack.push(S.groups[c]);
  }
  return out;
}

// Asks the server for the dependence edges between the nodes visible with
// the currently expanded server groups: [from, to, critical], a collapsed
// group standing in by its `rep` task.
async function refreshView() {
  const g = S.g;
  const expanded = S.groups.filter(x => x.server && !x.collapsed).map(x => x.id);
  const view = await (await fetch(`api/rank/${g.rank}/view`, { method: 'POST', body: expanded.join(',') })).json();
  if (S.g === g) S.depView = view.deps;
}

// Fetches the records of a server group's direct tasks; resolves at once if
// they are loaded. Concurrent calls share one request.
function loadGroup(grp) {
  if (grp.loaded || !grp.server) return Promise.resolve();
  if (!grp.loading) {
    const g = S.g;
    grp.loading = fetch(`api/rank/${g.rank}/group/${grp.id}`).then(r => r.json()).then(data => {
      if (S.g !== g) return;   // another rank was loaded meanwhile
      for (const r of data.tasks) Object.assign(g.tasks[r.i], r, { loaded: true });
      grp.loaded = true;
    });
  }
  return grp.loading;
}
// Loads the records of the given tasks, group by group. Without argument
// all tasks are loaded in one request (the complete graph document).
function ensureLoaded(tasks) {
  const g = S.g;
  if (tasks) {
    const need = new Set();
    for (const i of tasks) if (!g.tasks[i].loaded) need.add(S.groups[S.groupOf[i]]);
    return Promise.all([...need].map(loadGroup));
  }
  if (S.groups.every(x => x.loaded || !x.server)) return Promise.resolve();
  if (!g.loadingAll) {
    g.loadingAll = fetch(`api/rank/${g.rank}/full`).then(r => r.json()).then(full => {
      if (S.g !== g) return;
      for (const [i, r] of full.tasks.entries()) if (!g.tasks[i].loaded) Object.assign(g.tasks[i], r, { i, x: g.tasks[i].x, y: g.tasks[i].y, dp: [], ds: [], loaded: 'all' });
      // The complete document lists dependences as edges, not per task.
      for (const [a, b, kind] of full.edges) {
        if (kind !== 1) continue;
        if (g.tasks[b].loaded === 'all') g.tasks[b].dp.push(a);
        if (g.tasks[a].loaded === 'all') g.tasks[a].ds.push(b);
      }
      for (const x of S.groups) x.loaded = true;
    });
  }
  return g.loadingAll;
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
    `<button id="exp-crit" title="Expand every group that contains a task on the critical path, through all nesting levels, so the whole path is visible">Expand critical path</button>` +
    `<span class="muted">${n} task${n === 1 ? '' : 's'} on the path</span>`;
  $('sel-crit').onclick = () => setRegion(criticalTasks());
  $('exp-crit').onclick = expandCriticalPath;
  renderRunInfo();
}

// Optional information about the run from the trace header (S.g.info:
// [key, value] pairs). Known keys get a label; "env.NAME" entries are
// environment variables; anything else is shown under its key.
const INFO_LABEL = {
  'omp.runtime_version': 'OpenMP runtime', 'omp.runtime_library': 'OpenMP runtime library', 'omp.version': 'OpenMP version (_OPENMP)', 'omp.num_procs': 'Processors seen by the runtime',
  'process.exe': 'Executable', 'process.args': 'Command line', 'process.cwd': 'Working directory', 'process.cpus_allowed': 'CPUs the process may use',
  'mpi.world_size': 'MPI processes', 'tool.version': 'Recording tool version',
};
function renderRunInfo() {
  const el = $('runinfo'), info = S.g.info || [];
  el.hidden = !info.length;
  if (!info.length) return;
  const row = (k, v) => `<tr><td>${esc(k)}</td><td class="mono">${v === '' ? '<span class="muted">(empty)</span>' : esc(v)}</td></tr>`;
  const env = info.filter(([k]) => k.startsWith('env.')), other = info.filter(([k]) => !k.startsWith('env.'));
  el.innerHTML = `<summary>Run information</summary><table class="kv">` +
    other.map(([k, v]) => row(INFO_LABEL[k] || k, v)).join('') +
    (env.length ? `<tr><th colspan="2">Environment variables</th></tr>` + env.map(([k, v]) => row(k.slice(4), v)).join('') : '') + `</table>`;
}

// Expands every server group containing a task on the critical path,
// through all nesting levels, so that each of these tasks is visible.
// Groups made in the browser that hide one of them are expanded too.
function expandCriticalPath() {
  const open = new Set();
  for (const i of criticalTasks()) {
    for (let g = S.groupOf[i]; g >= 0 && !open.has(S.groups[g]); g = S.groups[g].parent) open.add(S.groups[g]);
    for (const grp of S.groups) if (!grp.server && grp.collapsed && grp.members.includes(i)) open.add(grp);
  }
  const closed = [...open].filter(grp => grp.collapsed);
  return closed.length ? setCollapsed(closed, false) : Promise.resolve();
}

// Indices of all tasks with work on the critical path of the whole graph.
function criticalTasks() {
  return new Set(S.g.crit.map(c => c[0]));
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
    for (const e of S.vEdges) {
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
  const inReg = i => S.repGroup[i] >= 0 ? S.groups[S.repGroup[i]].inRegion : S.region.has(i);
  const inSel = e => !fade || (inReg(e[0]) && inReg(e[1]));
  const shown = e => e[2] === 0 ? S.showCreate : S.showDep;
  // Creation edges of large fans are drawn as a bus (see drawFans); only
  // their critical path edges are drawn as direct lines.
  const direct = e => e[2] !== 0 || !S.fanChild[e[1]];
  const greyAlpha = far ? 0.25 : 0.4, createAlpha = far ? 0.5 : 0.9;
  if (fade) pass(e => shown(e) && !inSel(e) && direct(e), css.axis, 1, greyAlpha);
  if (S.showCreate) drawFans(sx, sy, r, fade, greyAlpha, createAlpha);
  if (S.showCreate) pass(e => e[2] === 0 && !isCrit(e) && inSel(e) && direct(e), css.edgeCreate, 1, createAlpha);
  if (S.showDep) pass(e => e[2] === 1 && !isCrit(e) && inSel(e), css.edgeDep, 1, far ? 0.35 : 0.7);
  if (S.regionPairs.length) pass(e => e[4], css.ink, 2.5, 1);
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
    if (S.hidden[i] >= 0) continue;   // inside a collapsed group
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
  drawGroups(sx, sy, r, fade);
  if (k >= 1.1) {
    ctx.fillStyle = css.ink2; ctx.font = `${Math.min(11 * k, 13)}px system-ui, sans-serif`; ctx.textAlign = 'center'; ctx.textBaseline = 'top';
    for (let i = 0; i < T.length; i++) {
      if (S.hidden[i] >= 0) continue;
      const t = T[i], x = sx(t), y = sy(t);
      if (x > -30 && x < S.w + 30 && y > -30 && y < S.h + 30) ctx.fillText(t.id, x, y + r + 5);
    }
  }
}

// Rings around the hovered and the selected task.
function graphOverlay() {
  const { cam } = S, k = cam.k, r = Math.max(R * k, 1.2);
  if (S.hoverGroup >= 0 && S.groups[S.hoverGroup]?.drawn) {
    const grp = S.groups[S.hoverGroup], t = S.g.tasks[grp.rep], [hw, hh] = groupSize(grp, r);
    ctx.lineWidth = 2.5; ctx.strokeStyle = S.css.ink;
    ctx.beginPath(); ctx.roundRect(t.x * DX * k + cam.x - hw - 3, t.y * DY * k + cam.y - hh - 3, 2 * hw + 6, 2 * hh + 6, hh * 0.5); ctx.stroke();
  }
  for (const [i, w] of [[S.hover, 1.5], [S.sel, 3]]) {
    if (i < 0) continue;
    const t = S.g.tasks[i];
    ctx.lineWidth = w; ctx.strokeStyle = S.css.ink;
    ctx.beginPath(); ctx.arc(t.x * DX * k + cam.x, t.y * DY * k + cam.y, Math.max(r + 5, 7), 0, 6.2832); ctx.stroke();
  }
}

// ------------------------------------------------------------------ collapsible groups
//
// A group is a set of tasks that can be shown as one node. Collapsing only
// changes the view: all metrics are computed by the server on the full graph.
//   taskloop  the tasks one task created for a taskloop construct
//   fan       FAN or more sibling tasks created at the same source location
//   subtree   all descendants of a task (created on demand)
// The first two are detected when a rank is loaded and include the
// descendants of their tasks. Hidden tasks take the position of their group
// node, so edges, selection rectangles and "centre" work on them unchanged.

function descendants(roots) {
  const out = [], stack = [...roots];
  while (stack.length) { const i = stack.pop(); out.push(i); for (const c of S.children[i]) stack.push(c); }
  return out;
}

// `sum` is the server's summary of the members (group_summary_json): their
// statistics and a typical task used for the node's colour.
function makeGroup(kind, anchor, loc, members, sum) {
  // A group made in the browser, with an explicit member list. The
  // representative is the first-created task: its position is the group
  // node's, and it decides where a dependence cycle between groups is cut.
  const grp = { id: S.groups.length, kind, anchor, loc, members, rep: members.reduce((a, b) => Math.min(a, b)), sum, n: sum.n, cr: sum.cr,
    parent: -1, part: null, tasks: members, sub: [], collapsed: false, drawn: false, inRegion: false, server: false, loaded: false, loading: null };
  S.groups.push(grp);
  return grp;
}

// Recomputes which tasks are hidden, the merged edges and the fans; lays the
// visible graph out again unless `relayout` is false (selection changes).
function rebuildVisible(relayout = true) {
  const g = S.g, T = g.tasks, n = T.length;
  S.hidden = new Int32Array(n).fill(-1);
  S.repGroup = new Int32Array(n).fill(-1);
  for (const grp of S.groups) grp.drawn = false;
  // Server groups: walk the tree from the top; a collapsed group hides all
  // its tasks, an expanded one shows its direct tasks and its child groups.
  const walk = S.groups.filter(x => x.server && x.parent < 0);
  while (walk.length) {
    const grp = walk.pop();
    if (grp.collapsed) {
      for (const i of membersOf(grp)) S.hidden[i] = grp.id;
      grp.drawn = true;
    } else for (const c of grp.sub) walk.push(S.groups[c]);
  }
  // Groups made in the browser lie across those; larger ones first. A
  // server group whose tasks they cover is not drawn.
  for (const grp of S.groups.filter(x => !x.server && x.collapsed).sort((a, b) => b.n - a.n)) {
    let any = false;
    for (const i of grp.members) {
      const h = S.hidden[i];
      if (h >= 0 && !S.groups[h].server) continue;   // inside a larger browser group
      if (h >= 0) S.groups[h].drawn = false;
      S.hidden[i] = grp.id; any = true;
    }
    grp.drawn = any;
  }
  for (const grp of S.groups) {
    if (!grp.drawn) { grp.inRegion = false; continue; }
    S.repGroup[grp.rep] = grp.id;
    grp.inRegion = S.region.size > 0 && membersOf(grp).some(i => S.region.has(i));
  }
  const rep = i => S.hidden[i] < 0 ? i : S.groups[S.hidden[i]].rep;
  // Edges between visible nodes: [from, to, kind, critical, region critical].
  // All edges between the same two nodes are merged into one.
  const merged = new Map();
  for (const e of g.edges) {
    const a = rep(e[0]), b = rep(e[1]);
    if (a === b) continue;
    const key = (a * n + b) * 2 + e[2];
    let v = merged.get(key);
    if (!v) merged.set(key, v = [a, b, e[2], 0, 0]);
    if (e[3]) v[3] = 1;
  }
  // Dependence edges of the current view from the server; `rep` maps their
  // end points on if a group made in the browser hides them.
  for (const e of S.depView) {
    const a = rep(e[0]), b = rep(e[1]);
    if (a === b) continue;
    const key = (a * n + b) * 2 + 1;
    let v = merged.get(key);
    if (!v) merged.set(key, v = [a, b, 1, 0, 0]);
    if (e[2]) v[3] = 1;
  }
  // Edges on the critical path of the selected region.
  for (const [p, q] of S.regionPairs) {
    const a = rep(p), b = rep(q);
    for (const key of [(a * n + b) * 2, (a * n + b) * 2 + 1, (b * n + a) * 2, (b * n + a) * 2 + 1]) { const v = merged.get(key); if (v) v[4] = 1; }
  }
  S.vEdges = [...merged.values()];
  findFans();
  if (relayout) layoutVisible();
}

// Layered layout of the visible graph, like the server's (src/layout.rs):
// layer = longest path from a root, ordering by the mean position of the
// predecessors, wide layers wrapped. A collapsed group takes three slots.
// Merged edges can form cycles (two groups depending on each other); a cycle
// is broken at its earliest task, so some edges then point upwards.
function layoutVisible() {
  const T = S.g.tasks, n = T.length;
  const nodes = [];
  for (let i = 0; i < n; i++) if (S.hidden[i] < 0 || S.repGroup[i] >= 0) nodes.push(i);
  const succ = new Map(), pred = new Map(), indeg = new Int32Array(n), layer = new Int32Array(n), done = new Uint8Array(n);
  for (const [a, b] of S.vEdges) {
    if (!succ.has(a)) succ.set(a, []);
    if (!pred.has(b)) pred.set(b, []);
    succ.get(a).push(b); pred.get(b).push(a); indeg[b]++;
  }
  const queue = nodes.filter(i => indeg[i] === 0);
  for (const i of queue) done[i] = 1;
  let head = 0, next = 0;
  while (head < nodes.length) {
    if (head === queue.length) {              // only cycles left: release the earliest task
      while (done[nodes[next]]) next++;
      done[nodes[next]] = 1; queue.push(nodes[next]);
    }
    const u = queue[head++];
    for (const v of succ.get(u) || []) {
      if (done[v]) continue;
      layer[v] = Math.max(layer[v], layer[u] + 1);
      if (--indeg[v] === 0) { done[v] = 1; queue.push(v); }
    }
  }
  const layers = [];
  for (const i of nodes) (layers[layer[i]] ||= []).push(i);
  const maxRow = Math.max(48, Math.ceil(Math.sqrt(nodes.length) * 2));
  const placed = new Uint8Array(n);
  let row = 0;
  for (const list of layers) {
    if (!list) continue;
    const keyed = list.map(v => {
      let sum = 0, cnt = 0;
      for (const u of pred.get(v) || []) if (placed[u]) { sum += T[u].x; cnt++; }
      return [cnt ? sum / cnt : 0, v];
    }).sort((a, b) => a[0] - b[0] || a[1] - b[1]);
    let i = 0, rows = 0;
    while (i < keyed.length) {
      let width = 0, j = i;
      while (j < keyed.length && (j === i || width + slots(keyed[j][1]) <= maxRow)) width += slots(keyed[j++][1]);
      let x = -width / 2;
      for (; i < j; i++) {
        const v = keyed[i][1], s = slots(v);
        T[v].x = x + s / 2 - 0.5; T[v].y = row; placed[v] = 1;
        x += s;
      }
      row++; rows++;
    }
    if (rows > 1) row += 0.5;
  }
  for (let i = 0; i < n; i++) if (S.hidden[i] >= 0 && S.repGroup[i] < 0) { const r = T[S.groups[S.hidden[i]].rep]; T[i].x = r.x; T[i].y = r.y; }
}
const slots = i => S.repGroup[i] >= 0 ? 3 : 1;

// Collapses or expands groups and lays the graph out again. `keep` is a task
// whose position on screen is preserved; without it the view is fitted.
async function setCollapsed(groups, collapsed, keep = -1) {
  const g = S.g, T = g.tasks;
  if (!collapsed) {
    // A group can only be expanded once its direct tasks are loaded.
    await Promise.all(groups.map(loadGroup));
    if (S.g !== g) return;
  }
  const before = keep >= 0 ? { x: T[keep].x, y: T[keep].y } : null;
  for (const grp of groups) grp.collapsed = collapsed;
  if (groups.some(x => x.server)) {
    await refreshView();
    if (S.g !== g) return;
  }
  S.hoverGroup = -1; $('tip').hidden = true;
  rebuildVisible();
  renderLegend(); renderTask();
  if (!before || S.view !== 'graph') return fit();
  S.cam.x += (before.x - T[keep].x) * DX * S.cam.k;
  S.cam.y += (before.y - T[keep].y) * DY * S.cam.k;
  draw();
}

// The subtree group of task i (all its descendants), created on first use.
async function subtreeGroup(i) {
  let grp = S.groups.find(x => x.kind === 'subtree' && x.anchor === i);
  if (!grp && S.children[i].length) {
    // The server summarises the members; their records need not be loaded.
    const g = S.g, members = descendants(S.children[i]);
    const sum = await (await fetch(`api/rank/${g.rank}/summary`, { method: 'POST', body: members.join(',') })).json();
    if (S.g !== g) return undefined;
    grp = S.groups.find(x => x.kind === 'subtree' && x.anchor === i) || makeGroup('subtree', i, g.tasks[i].loc, members, sum);
  }
  return grp;
}

// Half width and half height of a group node for node radius r.
const groupSize = (grp, r) => [r * (1.6 + 0.45 * Math.log10(grp.n)), r * 1.15];

// Colour of a group node: that of the typical task the server computed for
// it (most frequent thread, CPU, location ..., mean times).
const groupColor = grp => color(grp.sum.typ);

// Group nodes: a stack of cards, wider with more tasks, with the task count below.
function drawGroups(sx, sy, r, fade) {
  const { css, cam } = S, T = S.g.tasks, off = Math.max(1.5, r * 0.32);
  for (const grp of S.groups) {
    if (!grp.drawn) continue;
    const x = sx(T[grp.rep]), y = sy(T[grp.rep]), [hw, hh] = groupSize(grp, r);
    if (x < -hw - 10 || x > S.w + hw + 10 || y < -hh - 20 || y > S.h + hh + 10) continue;
    const grey = fade && !grp.inRegion;
    ctx.lineWidth = Math.max(1, Math.min(1.5, r / 4)); ctx.strokeStyle = grey ? css.muted : css.ink;
    for (const d of [2, 1]) {                       // the cards behind
      ctx.fillStyle = css.surface;
      ctx.beginPath(); ctx.roundRect(x - hw + d * off, y - hh - d * off, 2 * hw, 2 * hh, hh * 0.35); ctx.fill(); ctx.stroke();
    }
    ctx.fillStyle = grey ? css.axis : groupColor(grp);
    ctx.beginPath(); ctx.roundRect(x - hw, y - hh, 2 * hw, 2 * hh, hh * 0.35); ctx.fill(); ctx.stroke();
    if (S.showCrit && grp.cr > 0 && !grey) {
      ctx.strokeStyle = css.critical; ctx.lineWidth = 2;   // around the whole stack
      ctx.beginPath(); ctx.roundRect(x - hw - 3, y - hh - 2 * off - 3, 2 * hw + 2 * off + 6, 2 * hh + 2 * off + 6, hh * 0.5); ctx.stroke();
    }
    if (cam.k >= 0.45) {
      ctx.fillStyle = css.ink2; ctx.font = `600 ${Math.max(10, Math.min(11 * cam.k, 13))}px system-ui, sans-serif`;
      ctx.textAlign = 'center'; ctx.textBaseline = 'top';
      ctx.fillText('×' + grp.n, x, y + hh + 7);
    }
  }
}

const GROUP_KIND = { taskloop: 'taskloop', fan: 'tasks from one source line', subtree: 'subtree' };

function groupTip(grp) {
  const g = S.g, l = g.locs[grp.loc], s = grp.sum;
  let where = grp.kind === 'subtree' ? `below task ${g.tasks[grp.anchor].id}` : esc(locShort(l)) + (funcName(l) && l.file ? ' in ' + esc(funcName(l)) : '');
  if (grp.part) where += ` · sibling tasks ${grp.part[0] + 1}–${grp.part[0] + grp.part[1]} of ${grp.part[2]}`;
  const opens = grp.server && grp.sub.length
    ? `click to expand: shows ${grp.tasks.length} task${grp.tasks.length === 1 ? '' : 's'} and ${grp.sub.length} group${grp.sub.length === 1 ? '' : 's'}`
    : 'click to expand';
  return `<b>${s.n} tasks</b> · collapsed ${GROUP_KIND[grp.kind]}<br>${where}<br>` +
    `work ${fmt(s.w)} (${pctText(s.w, g.work)} % of T₁) · avg ${fmt(s.w / s.n)} · min ${fmt(s.min)} · max ${fmt(s.max)}<br>` +
    (s.t1 > s.t0 ? `ran ${fmt(s.t0)} → ${fmt(s.t1)} on ${s.threads} thread${s.threads === 1 ? '' : 's'}<br>` : '') +
    (s.own_cr != null ? `parallelism ${(s.w / Math.max(s.own_cr, 1)).toFixed(1)} · peak ${s.peak} at once<br>` : `<span style="opacity:.75">parallelism: computing…</span><br>`) +
    (s.cr > 0 ? `on critical path: ${fmt(s.cr)} (${pctText(s.cr, g.critical)} % of T∞)<br>` : '') +
    `<span style="opacity:.75">${opens} · Shift+click to select its tasks</span>`;
}

// Parallelism and peak of a server group need a pass over the graph, so
// the server computes them when the group is first hovered.
async function loadGroupStats(grp) {
  if (grp.sum.own_cr != null || grp.statsLoading || !grp.server) return;
  grp.statsLoading = true;
  const g = S.g;
  try {
    const st = await (await fetch(`api/rank/${g.rank}/group/${grp.id}/stats`)).json();
    grp.sum.own_cr = st.own_cr; grp.sum.peak = st.peak;
    if (S.g === g && S.hoverGroup === grp.id && !$('tip').hidden) $('tip').innerHTML = groupTip(grp);
  } finally { grp.statsLoading = false; }
}

// Fans: parents with at least FAN children. Instead of one line per child,
// a fan is drawn as a trunk below the parent, a horizontal bus above each
// row of children, and a short stub down to every child, which needs a
// fraction of the pixels of thousands of long crossing lines.
const FAN = 8;
function findFans() {
  S.fans = [];
  S.fanKids = new Map();   // parent -> its visible children
  S.fanChild = new Uint8Array(S.g.tasks.length);
  S.children.forEach((ch, p) => {
    if (ch.length < FAN || S.hidden[p] >= 0) return;
    const kids = ch.filter(c => S.hidden[c] < 0);
    if (kids.length < FAN) return;
    S.fans.push(p); S.fanKids.set(p, kids);
    for (const c of kids) S.fanChild[c] = 1;
  });
}
function drawFans(sx, sy, r, fade, greyAlpha, alpha) {
  const { css, cam } = S, T = S.g.tasks, gap = 0.3 * DY * cam.k;
  const styled = new Path2D(), grey = new Path2D();
  for (const p of S.fans) {
    const pt = T[p], px = sx(pt), py = sy(pt), pSel = !fade || S.region.has(p);
    // Children by row (layout y), with their horizontal extent.
    const rows = new Map();
    for (const c of S.fanKids.get(p)) {
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
// The timeline is drawn from what the server sends for the visible time
// range (S.tlData): the real segments when few are in view, else per thread
// and pixel column the busy fraction and the task taking most of the column.
// A range half a view wider on each side is requested, so panning has data
// while the next answer is on its way.
const tlCols = () => Math.max(1, Math.ceil(S.w - TL.left - 12));
function timelineStale() {
  const d = S.tlData, g = S.g;
  if (!d || d.rank !== g.rank || d.mode !== S.colorBy || d.sel !== S.selSerial) return true;
  if (S.tl.t0 < d.a && d.a > 0 || S.tl.t1 > d.b && d.b < g.wall) return true;
  // Exact segments and intervals stay valid at any zoom; binned data does not.
  if (d.kind !== 'bins' && !d.gaps.cls) return false;
  const ratio = (S.tl.t1 - S.tl.t0) / tlCols() / d.per;   // visible time per pixel vs. per bin
  return ratio < 0.6 || ratio > 1.6;
}
function requestTimeline() {
  if (S.tlBusy) { S.tlAgain = true; return; }
  const g = S.g, span = S.tl.t1 - S.tl.t0, cols = tlCols();
  const a = Math.max(0, S.tl.t0 - span / 2), b = Math.min(Math.max(g.wall, 1), S.tl.t1 + span / 2);
  const want = { rank: g.rank, mode: S.colorBy, sel: S.selSerial, a, b };
  S.tlBusy = true;
  fetch(`api/rank/${g.rank}/timeline`, { method: 'POST', body: `t0=${a}&t1=${b}&cols=${Math.round(cols * (b - a) / span)}&mode=${S.colorBy}` })
    .then(r => r.json())
    .then(d => { if (S.g === g) { S.tlData = Object.assign(d, want, { per: (d.t1 - d.t0) / d.cols }); renderLegend(); } })
    .finally(() => {
      S.tlBusy = false;
      if (S.tlAgain) { S.tlAgain = false; if (S.view === 'timeline' && timelineStale()) requestTimeline(); }
      if (S.view === 'timeline') draw();
    });
}
// Calls f(row, x, width, busy fraction, task, value, flags) for every item
// of the timeline data that is in view; flags: 1 critical, 2 selected.
function eachTimelineItem(f) {
  const d = S.tlData;
  if (!d) return;
  const rowOf = new Map(S.g.threads.map((t, i) => [t.id, i]));
  if (d.kind === 'segs') {
    for (const [th, a, b, task, v, fl] of d.items) {
      if (b < S.tl.t0 || a > S.tl.t1) continue;
      const row = rowOf.get(th); if (row === undefined) continue;
      const x = tlX(a);
      f(row, x, Math.max(tlX(b) - x, 0.6), 1, task, v, fl, true);
    }
    return;
  }
  for (const r of d.rows) {
    const row = rowOf.get(r.thread); if (row === undefined) continue;
    for (const [c, busy, task, v, fl] of r.bins) {
      const a = d.t0 + c * d.per;
      if (a + d.per < S.tl.t0 || a > S.tl.t1) continue;
      const x = tlX(a);
      f(row, x, tlX(a + d.per) - x, busy, task, v, fl, false);
    }
  }
}

// Critical path strip above the thread rows (with the "Critical path" overlay).
const STRIP = { top: 24, height: 16 };
const GAP_LABEL = ['critical path running', 'gap: all threads busy with other work', 'gap: critical task ready, a thread idle', 'gap: threads idle, no critical task ready'];
const GAP_SHORT = ['on critical path', 'gap, threads busy', 'gap, critical task waiting', 'gap, idle'];

const stripShown = () => S.showCrit && S.view === 'timeline';

// The strip is part of the timeline data (S.tlData.gaps, see gap_strip_json
// in main.rs): the exact intervals when few are in the window, else the
// dominant class per pixel column of the requested window.
function drawGapStrip() {
  ctx.fillStyle = S.css.ink2; ctx.textAlign = 'left';
  ctx.fillText('critical path', 10, STRIP.top + STRIP.height / 2);
  const d = S.tlData;
  if (!d || !d.gaps) return;
  const paths = [new Path2D(), new Path2D(), new Path2D(), new Path2D()], per = (d.t1 - d.t0) / d.cols;
  if (d.gaps.iv) {
    for (const [a, b, kind] of d.gaps.iv) {
      if (b < S.tl.t0 || a > S.tl.t1) continue;
      const x = tlX(a);
      paths[kind].rect(x, STRIP.top, Math.max(tlX(b) - x, 0.6), STRIP.height);
    }
  } else {
    // Runs of equal class become one rectangle.
    const cls = d.gaps.cls;
    for (let c = 0, start = 0; c <= cls.length; c++) {
      if (c < cls.length && cls[c] === cls[start]) continue;
      if (cls[start] >= 0) { const x = tlX(d.t0 + start * per); paths[cls[start]].rect(x, STRIP.top, tlX(d.t0 + c * per) - x, STRIP.height); }
      start = c;
    }
  }
  ctx.save(); ctx.beginPath(); ctx.rect(TL.left, 0, S.w - TL.left, S.h); ctx.clip();
  // Idle first, so the problem classes stay visible where exact intervals overlap a pixel.
  for (const kind of [3, 1, 2, 0]) { ctx.fillStyle = S.css.gap[kind]; ctx.fill(paths[kind]); }
  ctx.restore();
}

// What the strip shows under the mouse: [begin, end, class, exact], or null.
function pickGap(mx, my) {
  const d = S.tlData;
  if (!stripShown() || !d || !d.gaps || mx < TL.left || my < STRIP.top - 2 || my > STRIP.top + STRIP.height + 2) return null;
  const t = tlT(mx);
  if (d.gaps.iv) {
    const iv = d.gaps.iv.find(i => i[0] <= t && t <= i[1]);
    return iv ? [iv[0], iv[1], iv[2], true] : null;
  }
  const per = (d.t1 - d.t0) / d.cols, c = Math.floor((t - d.t0) / per);
  return c >= 0 && c < d.gaps.cls.length && d.gaps.cls[c] >= 0 ? [d.t0 + c * per, d.t0 + (c + 1) * per, d.gaps.cls[c], false] : null;
}

const tlX = t => TL.left + (t - S.tl.t0) / (S.tl.t1 - S.tl.t0) * (S.w - TL.left - 12);
const tlT = x => S.tl.t0 + (x - TL.left) / (S.w - TL.left - 12) * (S.tl.t1 - S.tl.t0);

function drawTimeline() {
  const { g, css } = S;
  TL.top = stripShown() ? STRIP.top + STRIP.height + 12 : 30;
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
  if (stripShown()) drawGapStrip();

  if (timelineStale()) requestTimeline();
  ctx.save(); ctx.beginPath(); ctx.rect(TL.left, 0, S.w - TL.left, S.h); ctx.clip();
  // Batched like the graph: one path per fill colour and per kind of mark.
  const fills = new Map(), path = c => { let p = fills.get(c); if (!p) fills.set(c, p = new Path2D()); return p; };
  const fade = S.fadeOthers && S.region.size > 0;
  if (fade) path(css.axis);
  const critStrip = new Path2D(), regionBar = new Path2D();
  eachTimelineItem((row, x, w, busy, task, v, fl, exact) => {
    const y = TL.top + row * TL.row + (TL.row - TL.bar) / 2;
    // A real segment gets a 1px gap to its neighbour once it is wide enough;
    // a pixel column is as tall as the thread was busy in it.
    const wg = exact && w > 3 ? w - 1 : w, h = Math.max(busy * TL.bar, 1.5);
    const grey = fade && !(fl & 2);
    path(grey ? css.axis : colorOfValue(v)).rect(x, y + TL.bar - h, wg, h);
    if (grey) return;
    if (S.showCrit && (fl & 1)) critStrip.rect(x, y + TL.bar + 1, wg, 3);
    if ((fl & 2) && !S.fadeOthers) regionBar.rect(x, y - 4, wg, 2);
  });
  for (const [c, p] of fills) { ctx.fillStyle = c; ctx.fill(p); }
  ctx.fillStyle = css.critical; ctx.fill(critStrip);
  ctx.fillStyle = css.ink; ctx.fill(regionBar);
  ctx.restore();
  if (!S.tlData) { ctx.fillStyle = css.muted; ctx.textAlign = 'left'; ctx.fillText('Loading…', TL.left + 8, TL.top + TL.row / 2); }
}

// Outlines of the hovered and the selected task's segments in view.
function timelineOverlay() {
  ctx.save(); ctx.beginPath(); ctx.rect(TL.left, 0, S.w - TL.left, S.h); ctx.clip();
  ctx.strokeStyle = S.css.ink;
  eachTimelineItem((row, x, w, busy, task, v, fl, exact) => {
    if (!exact || (task !== S.hover && task !== S.sel)) return;
    ctx.lineWidth = task === S.sel ? 2 : 1;
    ctx.strokeRect(x - 1, TL.top + row * TL.row + (TL.row - TL.bar) / 2 - 1, w + 1, TL.bar + 2);
  });
  ctx.restore();
}

// ------------------------------------------------------------------ picking

function pick(mx, my) {
  const T = S.g.tasks;
  if (S.view === 'graph') {
    const { cam } = S, lim = Math.max(R * cam.k + 4, 6);
    // A collapsed group is returned as -(group id + 2).
    const r = Math.max(R * cam.k, 1.2);
    for (const grp of S.groups) {
      if (!grp.drawn) continue;
      const t = T[grp.rep], [hw, hh] = groupSize(grp, r);
      if (Math.abs(t.x * DX * cam.k + cam.x - mx) <= hw + 3 && Math.abs(t.y * DY * cam.k + cam.y - my) <= hh + 5) return -(grp.id + 2);
    }
    let best = -1, bd = lim * lim;
    for (let i = 0; i < T.length; i++) {
      if (S.hidden[i] >= 0) continue;
      const dx = T[i].x * DX * cam.k + cam.x - mx, dy = T[i].y * DY * cam.k + cam.y - my, d = dx * dx + dy * dy;
      if (d < bd) { bd = d; best = i; }
    }
    return best;
  }
  const row = Math.floor((my - TL.top) / TL.row), th = S.g.threads[row];
  if (!th || mx < TL.left) return -1;
  // The segment under the mouse, or the task taking most of that pixel column.
  let found = -1;
  eachTimelineItem((r, x, w, busy, task) => { if (r === row && mx >= x - 1 && mx <= x + w + 1) found = task; });
  return found;
}

function showTip(i, mx, my) {
  const tip = $('tip');
  if (i === -1) { tip.hidden = true; return; }
  if (i < -1) {
    const grp = S.groups[-i - 2];
    tip.innerHTML = groupTip(grp);
    loadGroupStats(grp);
  } else {
    const t = S.g.tasks[i];
    // A task that is not loaded is known by its id only; a click loads it.
    tip.innerHTML = t.loaded ? `<b>task ${t.id}</b> · ${KIND[t.k]} · ${fmt(t.w)}<br>${esc(locShort(S.g.locs[t.loc]))}`
      : `<b>task ${t.id}</b><br><span style="opacity:.75">click for details</span>`;
  }
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
    const gap = S.view === 'timeline' ? pickGap(x, y) : null;
    if (gap) {
      if (S.hover >= 0) { S.hover = -1; draw(true); }
      const tip = $('tip'), total = Math.max(S.g.wall, 1);
      const totals = S.tlData.gaps.total;
      tip.innerHTML = `<b>${GAP_LABEL[gap[2]]}</b><br>` +
        (gap[3] ? `${fmt(gap[0])} → ${fmt(gap[1])} (${fmt(gap[1] - gap[0])})` : `most of ${fmt(gap[0])} → ${fmt(gap[1])}; zoom in for exact intervals`) + `<br>` +
        `<span style="opacity:.75">${GAP_SHORT[gap[2]]} in total: ${fmt(totals[gap[2]])} (${pctText(totals[gap[2]], total)} % of the run)</span>`;
      tip.hidden = false;
      tip.style.left = Math.min(x + 14, S.w - tip.offsetWidth - 6) + 'px';
      tip.style.top = Math.max(y + 16, 4) + 'px';
      return;
    }
    const h = pick(x, y), task = Math.max(h, -1), grp = h < -1 ? -h - 2 : -1;
    if (task !== S.hover || grp !== S.hoverGroup) { S.hover = task; S.hoverGroup = grp; draw(true); }
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
  if (!d.moved) {
    const p = pick(d.x, d.y);
    if (p >= -1) return select(p);
    // A collapsed group: Shift+click selects its tasks, a click expands it.
    const grp = S.groups[-p - 2];
    return e.shiftKey ? setRegion(new Set(membersOf(grp))) : setCollapsed([grp], false, grp.rep);
  }
  if (d.band) {
    const sel = new Set(), T = S.g.tasks;
    if (S.view === 'graph') {
      const [x0, x1, y0, y1] = [Math.min(d.x0, d.x), Math.max(d.x0, d.x), Math.min(d.y0, d.y), Math.max(d.y0, d.y)];
      T.forEach((t, i) => {
        const x = t.x * DX * S.cam.k + S.cam.x, y = t.y * DY * S.cam.k + S.cam.y;
        if (x >= x0 && x <= x1 && y >= y0 && y <= y1) sel.add(i);
      });
    } else {
      // Which tasks ran in the time range is resolved by the server.
      const [t0, t1] = [tlT(Math.min(d.x0, d.x)), tlT(Math.max(d.x0, d.x))], g = S.g;
      fetch(`api/rank/${g.rank}/timeline-select`, { method: 'POST', body: `t0=${Math.max(t0, 0)}&t1=${Math.max(t1, 0)}` })
        .then(r => r.json()).then(r => { if (S.g === g) setRegion(new Set(r.tasks)); });
      return draw();
    }
    setRegion(sel);
  }
  draw();
});
canvas.addEventListener('pointerleave', () => { if (S.hover >= 0 || S.hoverGroup >= 0) { S.hover = -1; S.hoverGroup = -1; draw(true); } $('tip').hidden = true; });
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

// Selects a task for the details panel; its record is loaded first if
// necessary. Only the ring changes in the picture.
function select(i) {
  if (i >= 0 && !S.g.tasks[i].loaded) {
    const g = S.g;
    return ensureLoaded([i]).then(() => { if (S.g === g) select(i); });
  }
  S.sel = i; renderTask(); draw(true);
}

$('rank').onchange = e => loadRank(e.target.value);
$('color').onchange = e => { S.colorBy = e.target.value; assignSlots(); renderLegend(); renderOrigins(); renderTask(); draw(); };
for (const [id, key] of [['show-crit', 'showCrit'], ['show-create', 'showCreate'], ['show-dep', 'showDep'], ['fade-others', 'fadeOthers']])
  $(id).onchange = e => { S[key] = e.target.checked; renderLegend(); draw(); };
for (const v of ['graph', 'timeline']) $('view-' + v).onclick = async () => {
  S.view = v;
  $('view-graph').setAttribute('aria-selected', v === 'graph'); $('view-timeline').setAttribute('aria-selected', v === 'timeline');
  $('hint').innerHTML = v === 'graph'
    ? 'Drag to pan · wheel to zoom · click a task · <kbd>Shift</kbd>+drag to select a region · <kbd>Esc</kbd> to clear'
    : 'Drag to pan · wheel to zoom time · click a segment · <kbd>Shift</kbd>+drag to select a time range · <kbd>Esc</kbd> to clear';
  renderLegend();
  draw();
};
$('fit').onclick = fit;
$('collapse-all').onclick = () => S.g && setCollapsed(S.groups.filter(x => x.server), true);
$('expand-all').onclick = async () => {
  if (!S.g) return;
  // Everything at once is only reasonable for small graphs.
  if (S.g.n > 50000 && !window.confirm(`Expanding all groups loads and draws all ${S.g.n} tasks, which will be slow and needs a lot of memory. Continue?`)) return;
  const g = S.g;
  await ensureLoaded();
  if (S.g === g) setCollapsed(S.groups, false);
};
$('max-group').onchange = e => {
  const v = Math.max(16, Math.round(Number(e.target.value)) || 0);
  if (S.g && v !== S.g.max_group) loadRank(S.g.rank, v);
  else if (S.g) e.target.value = S.g.max_group;
};
$('find').onchange = e => {
  const i = S.byId.get(Number(e.target.value));
  if (i !== undefined) { select(i); centerOn(i); }   // a task inside a collapsed group is shown at its group
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
  const gapTotals = S.tlData && S.tlData.gaps && S.tlData.gaps.total;
  if (stripShown() && gapTotals) {
    const total = Math.max(g.wall, 1);
    html += `<span class="muted">strip:</span>` + [0, 1, 2, 3].map(k =>
      `<span title="${GAP_LABEL[k]}"><span class="sw" style="border-radius:2px;background:${S.css.gap[k]}"></span>${GAP_SHORT[k]} ${pctText(gapTotals[k], total)} %</span>`).join('');
  }
  if (S.fadeOthers && S.region.size) html += `<span><span class="sw" style="background:${S.css.axis}"></span>not selected</span>`;
  html += `<span class="muted">○ explicit · □ implicit · ◇ initial${S.groups.some(x => x.drawn) ? ' · ▭ collapsed group (×tasks)' : ''}</span>`;
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
    <dt>Depends on</dt><dd>${list(t.dp || [])}</dd>
    <dt>Required by</dt><dd>${list(t.ds || [])}</dd>
  </dl>
  <div class="actions"><button data-a="center">Centre</button><button data-a="subtree">Select subtree</button>${p ? '<button data-a="region">Select parallel region</button>' : ''}${S.children[S.sel].length ? '<button data-a="collapse-subtree" title="Show all tasks created below this task as one node">Collapse subtree</button>' : ''}${S.groupOf[S.sel] >= 0 && !S.groups[S.groupOf[S.sel]].collapsed ? `<button data-a="collapse-group" title="Show this task and the ${S.groups[S.groupOf[S.sel]].n - 1} others of its ${GROUP_KIND[S.groups[S.groupOf[S.sel]].kind]} group as one node">Collapse its group (×${S.groups[S.groupOf[S.sel]].n})</button>` : ''}</div>`;

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
  if (btn.dataset.a === 'collapse-subtree') { const i = S.sel; subtreeGroup(i).then(grp => { if (grp) setCollapsed([grp], true, i); }); }
  if (btn.dataset.a === 'collapse-group') { const grp = S.groups[S.groupOf[S.sel]]; setCollapsed([grp], true, grp.anchor); }
  if (btn.dataset.a === 'subtree') {
    const sel = new Set(), stack = [S.sel];
    while (stack.length) { const i = stack.pop(); if (!sel.has(i)) { sel.add(i); stack.push(...S.children[i]); } }
    setRegion(sel);
  }
  if (btn.dataset.a === 'region') {
    // All tasks of the region, plus everything they spawn (nested regions included).
    // Which region a task belongs to is part of its record: load all first.
    const g = S.g, pr = g.tasks[S.sel].pr;
    ensureLoaded().then(() => {
      if (S.g !== g) return;
      const sel = new Set(), stack = [];
      g.tasks.forEach((t, i) => { if (t.pr === pr) stack.push(i); });
      while (stack.length) { const i = stack.pop(); if (!sel.has(i)) { sel.add(i); stack.push(...S.children[i]); } }
      setRegion(sel);
    });
  }
});

// ------------------------------------------------------------------ region analysis

async function setRegion(sel) {
  S.region = sel; S.regionInfo = null; S.regionCrit = new Map(); S.regionPairs = [];
  rebuildVisible(false);
  renderLegend();
  const el = $('region');
  if (!sel.size) {
    el.innerHTML = '<p class="muted">Shift+drag over the graph (or a time range in the timeline) to analyse the degree of parallelism of a region.</p>';
    draw();
    // The server keeps the region to mark it in the timeline; clear it there.
    const g = S.g;
    await fetch(`api/rank/${g.rank}/region`, { method: 'POST', body: '' });
    if (S.g === g && S.region === sel) { S.selSerial = (S.selSerial || 0) + 1; if (S.view === 'timeline') draw(); }
    return;
  }
  el.innerHTML = '<p class="muted">Analysing…</p>';
  draw();
  const rank = S.g.rank;
  const r = await (await fetch(`api/rank/${rank}/region`, { method: 'POST', body: [...sel].join(',') })).json();
  if (S.region !== sel) return; // superseded by a newer selection
  S.selSerial = (S.selSerial || 0) + 1;   // the server now marks this region in the timeline
  S.regionInfo = r;
  S.regionCrit = new Map(r.crit_tasks);
  S.regionPairs = r.crit_edges;
  rebuildVisible(false);
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
  if (q.get('expand') === 'all') { await ensureLoaded(); await setCollapsed(S.groups, false); }
  if (q.get('expand') === 'critical') await expandCriticalPath();
  if (q.get('view') === 'timeline') await $('view-timeline').onclick();
  if (q.has('perf')) togglePerf(true);
  if (q.get('select') === 'critical') setRegion(criticalTasks());
  const i = S.byId.get(Number(q.get('task')));
  if (i !== undefined) {
    await select(i); centerOn(i);
    const btn = $('task').querySelector(`button[data-a="${q.get('select')}"]`);
    if (btn) btn.click();
  }
}

readCss();
resize();
loadRanks().then(applyHash).catch(e => { $('task').innerHTML = `<p>Cannot load trace data: ${esc(e.message)}</p>`; });
