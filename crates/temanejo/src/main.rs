//! temanejo: loads the per-rank trace files of one run, reconstructs and
//! analyses the task graphs and serves the browser GUI.

mod analysis;
mod layout;
mod model;
mod symbolize;
#[cfg(test)]
mod tests;

use analysis::PathResult;
use model::{Model, NONE};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use symbolize::Loc;

struct Rank {
    model: Model,
    crit: PathResult,
    /// Source locations and the index of each code address in them.
    locs: Vec<Loc>,
    loc_of: HashMap<u64, usize>,
    /// The task groups for the current maximum group size and the collapsed
    /// graph built from them; replaced when a client asks for another size.
    view: std::sync::RwLock<Grouped>,
    /// Wall-clock time (ns since the Unix epoch) when the tool started in this process.
    epoch_ns: u64,
    /// False if the trace has no end marker: the run did not finish cleanly.
    complete: bool,
    /// Classification of the run's time around the critical path, computed on first request.
    gaps: std::sync::OnceLock<analysis::CriticalGaps>,
    /// Execution segments per thread for timeline requests, built on first use.
    timeline: std::sync::OnceLock<Vec<ThreadSegments>>,
    /// The tasks of the region the GUI analysed last (empty: none); the
    /// timeline marks them without the browser sending the list again.
    selection: std::sync::RwLock<Vec<bool>>,
}

/// The execution segments of one thread, ordered by time (segments of a
/// thread do not overlap).
struct ThreadSegments {
    thread: u32,
    start: Vec<u64>,
    end: Vec<u64>,
    task: Vec<u32>,
}

fn thread_segments(m: &Model) -> Vec<ThreadSegments> {
    let mut by_thread: std::collections::BTreeMap<u32, Vec<(u64, u64, u32)>> = Default::default();
    for (i, t) in m.tasks.iter().enumerate() {
        for s in t.segs.iter().filter(|s| s.end > s.start) {
            by_thread.entry(s.thread).or_default().push((s.start, s.end, i as u32));
        }
    }
    by_thread
        .into_iter()
        .map(|(thread, mut segs)| {
            segs.sort_unstable();
            ThreadSegments { thread, start: segs.iter().map(|s| s.0).collect(), end: segs.iter().map(|s| s.1).collect(), task: segs.iter().map(|s| s.2).collect() }
        })
        .collect()
}

/// Largest number of segments a timeline answer lists individually.
const TIMELINE_MAX_SEGMENTS: usize = 4000;

/// A value of `key=value&...` request parameters.
fn param<'a>(params: &'a str, key: &str) -> Option<&'a str> {
    params.split('&').find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
}

/// What the timeline shows between `t0` and `t1` at a width of `cols` pixel
/// columns, coloured by `mode` (the GUI's colour modes).
///
/// If the range holds few segments, they are listed (`"segs"`): `[thread,
/// start, end, task, value, flags]`. Otherwise each thread's time is binned
/// per pixel column (`"bins"`): `[column, busy fraction, task, value,
/// flags]` for the columns in which the thread executed anything, `task`
/// being the one taking most of the column. `value` is the task's value in
/// the colour mode; flags: 1 on the critical path, 2 in the selected region
/// (for bins: any task of the column). Both forms carry `gaps`, the critical
/// path strip for the same window (`gap_strip_json`).
fn timeline_json(r: &Rank, params: &str) -> String {
    let m = &r.model;
    let num = |key: &str| param(params, key).and_then(|v| v.parse::<f64>().ok());
    let t0 = num("t0").unwrap_or(0.0).max(0.0);
    let t1 = num("t1").unwrap_or(m.wall as f64).max(t0 + 1.0);
    let cols = (num("cols").unwrap_or(1000.0) as usize).clamp(1, 8192);
    let mode = param(params, "mode").unwrap_or("thread");
    let rows = r.timeline.get_or_init(|| thread_segments(m));
    let selection = r.selection.read().unwrap();
    let value = |i: u32| -> i64 {
        let t = &m.tasks[i as usize];
        let seg = t.segs.first();
        match mode {
            "cpu" => seg.map_or(-1, |s| s.cpu as i64),
            "numa" => seg.map_or(-1, |s| s.numa as i64),
            "loc" => r.loc_of.get(&t.codeptr).map_or(-1, |&l| l as i64),
            "region" => idx(t.parallel),
            "kind" => t.kind as i64,
            "work" => t.work as i64,
            "create" => t.create_ts as i64,
            "start" => analysis::task_start(t) as i64,
            "wait" => analysis::task_start(t).saturating_sub(t.create_ts) as i64,
            _ => analysis::task_thread(t),
        }
    };
    let flags = |i: u32| (r.crit.task_time[i as usize] > 0) as u8 | (selection.get(i as usize).copied().unwrap_or(false) as u8) << 1;
    // Index range of the segments of each thread that touch the window.
    let (lo_t, hi_t) = (t0 as u64, t1.ceil() as u64);
    let ranges: Vec<(usize, usize)> = rows.iter().map(|row| (row.end.partition_point(|&e| e <= lo_t), row.start.partition_point(|&s| s < hi_t))).collect();
    let total: usize = ranges.iter().map(|&(a, b)| b.saturating_sub(a)).sum();
    let mut o = String::new();
    if total <= TIMELINE_MAX_SEGMENTS {
        let _ = write!(o, "{{\"kind\":\"segs\",\"t0\":{t0},\"t1\":{t1},\"cols\":{cols},\"gaps\":{},\"items\":[", gap_strip_json(r, t0, t1, cols));
        let mut first = true;
        for (row, &(a, b)) in rows.iter().zip(&ranges) {
            for j in a..b.max(a) {
                let i = row.task[j];
                let _ = write!(o, "{}[{},{},{},{},{},{}]", if first { "" } else { "," }, row.thread, row.start[j], row.end[j], i, value(i), flags(i));
                first = false;
            }
        }
        o.push_str("]}");
        return o;
    }
    let per = (t1 - t0) / cols as f64;
    let _ = write!(o, "{{\"kind\":\"bins\",\"t0\":{t0},\"t1\":{t1},\"cols\":{cols},\"segments\":{total},\"gaps\":{},\"rows\":[", gap_strip_json(r, t0, t1, cols));
    let mut busy = vec![0f64; cols];
    let mut best = vec![(0f64, 0u32); cols];
    let mut flag = vec![0u8; cols];
    for (k, (row, &(a, b))) in rows.iter().zip(&ranges).enumerate() {
        busy.iter_mut().for_each(|v| *v = 0.0);
        best.iter_mut().for_each(|v| *v = (0.0, 0));
        flag.iter_mut().for_each(|v| *v = 0);
        for j in a..b.max(a) {
            let (fa, fb) = (((row.start[j] as f64).max(t0) - t0) / per, ((row.end[j] as f64).min(t1) - t0) / per);
            let f = flags(row.task[j]);
            for c in (fa as usize)..(fb.ceil() as usize).min(cols) {
                let part = fb.min(c as f64 + 1.0) - fa.max(c as f64);
                if part <= 0.0 {
                    continue;
                }
                busy[c] += part;
                flag[c] |= f;
                if part > best[c].0 {
                    best[c] = (part, row.task[j]);
                }
            }
        }
        let _ = write!(o, "{}{{\"thread\":{},\"bins\":[", if k > 0 { "," } else { "" }, row.thread);
        let mut first = true;
        for c in (0..cols).filter(|&c| busy[c] > 0.0) {
            let _ = write!(o, "{}[{},{:.3},{},{},{}]", if first { "" } else { "," }, c, busy[c].min(1.0), best[c].1, value(best[c].1), flag[c]);
            first = false;
        }
        o.push_str("]}");
    }
    o.push_str("]}");
    o
}

/// The tasks with an execution segment overlapping the time range of the
/// request (`t0`, `t1`), ascending.
fn timeline_select_json(r: &Rank, params: &str) -> String {
    let num = |key: &str| param(params, key).and_then(|v| v.parse::<f64>().ok());
    let (t0, t1) = (num("t0").unwrap_or(0.0).max(0.0) as u64, num("t1").unwrap_or(0.0).max(0.0).ceil() as u64);
    let rows = r.timeline.get_or_init(|| thread_segments(&r.model));
    let mut tasks: Vec<u32> = Vec::new();
    for row in rows {
        let (a, b) = (row.end.partition_point(|&e| e < t0), row.start.partition_point(|&s| s <= t1));
        if b > a {
            tasks.extend_from_slice(&row.task[a..b]);
        }
    }
    tasks.sort_unstable();
    tasks.dedup();
    let mut o = String::from("{\"tasks\":");
    list(&mut o, tasks.iter());
    o.push('}');
    o
}

/// The groups of a rank for one maximum group size.
struct Grouped {
    grouping: analysis::Grouping,
    /// What the GUI loads first: the graph with all groups collapsed.
    graph_json: String,
}

impl Rank {
    fn regroup(&self, max_group: usize) -> Grouped {
        let grouping = analysis::group_tasks(&self.model, max_group);
        let graph_json = graph_json(&self.model, &self.crit, &self.locs, &self.loc_of, &grouping);
        Grouped { grouping, graph_json }
    }
}

/// Describes the loaded run: trace directory and recording time.
fn info_json(dir: &str, ranks: &[Rank]) -> String {
    let abs = std::fs::canonicalize(dir).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| dir.to_string());
    let name = std::path::Path::new(&abs).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| abs.clone());
    // Milliseconds, so the value stays exact as a JavaScript number.
    let recorded_ms = ranks.iter().map(|r| r.epoch_ns).filter(|&e| e > 0).min().unwrap_or(0) / 1_000_000;
    format!("{{\"dir\":{},\"name\":{},\"recorded_ms\":{},\"ranks\":{}}}", esc(&abs), esc(&name), recorded_ms, ranks.len())
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn idx(v: u32) -> i64 {
    if v == NONE { -1 } else { v as i64 }
}

fn summary_json(r: &Rank) -> String {
    let m = &r.model;
    let work = m.total_work();
    format!(
        "{{\"rank\":{},\"host\":{},\"pid\":{},\"tasks\":{},\"threads\":{},\"events\":{},\"wall\":{},\"work\":{},\"critical\":{},\"complete\":{}}}",
        m.rank,
        esc(&m.host),
        m.pid,
        m.tasks.len(),
        m.threads.len(),
        m.n_events,
        m.wall,
        work,
        r.crit.length,
        r.complete
    )
}

/// Predecessors and successors by dependence of some tasks.
type DepLists = HashMap<u32, (Vec<u32>, Vec<u32>)>;

fn dep_lists(m: &Model, wanted: &[bool]) -> DepLists {
    let mut lists = DepLists::new();
    for e in m.edges.iter().filter(|e| e.kind == model::EdgeKind::Depend) {
        if wanted[e.to as usize] {
            lists.entry(e.to).or_default().0.push(e.from);
        }
        if wanted[e.from as usize] {
            lists.entry(e.from).or_default().1.push(e.to);
        }
    }
    lists
}

/// Writes one task record. Compact keys, see web/app.js for their meaning.
/// `i` is the task's index; `x`, `y` are only written with a layout position.
fn task_json(o: &mut String, m: &Model, crit: &PathResult, loc_of: &HashMap<u64, usize>, i: usize, pos: Option<(f32, f32)>, deps: Option<&DepLists>) {
    let t = &m.tasks[i];
    let _ = write!(
        o,
        "{{\"i\":{},\"id\":{},\"k\":{},\"fl\":{},\"par\":{},\"pr\":{},\"ix\":{},\"ct\":{},\"cth\":{},\"loc\":{},\"e\":{},\"w\":{},\"tl\":{},\"cr\":{},",
        i,
        t.id,
        t.kind as u8,
        t.flags,
        idx(t.parent),
        idx(t.parallel),
        t.team_index,
        t.create_ts,
        t.create_thread,
        loc_of.get(&t.codeptr).map_or(-1, |&l| l as i64),
        t.end,
        t.work,
        t.taskloop as u8,
        crit.task_time[i]
    );
    if let Some((x, y)) = pos {
        let _ = write!(o, "\"x\":{x},\"y\":{y},");
    }
    if let Some(deps) = deps {
        // Tasks this one depends on (dp) and tasks depending on it (ds).
        let empty = (Vec::new(), Vec::new());
        let (pred, succ) = deps.get(&(i as u32)).unwrap_or(&empty);
        o.push_str("\"dp\":");
        list(o, pred.iter());
        o.push_str(",\"ds\":");
        list(o, succ.iter());
        o.push(',');
    }
    o.push_str("\"segs\":[");
    for (j, s) in t.segs.iter().enumerate() {
        let _ = write!(o, "{}[{},{},{},{},{},{}]", if j > 0 { "," } else { "" }, s.thread, s.cpu, s.cpu_end, s.numa, s.start, s.end);
    }
    o.push_str("],\"deps\":[");
    for (j, d) in t.deps.iter().enumerate() {
        let _ = write!(o, "{}[\"{:#x}\",{}]", if j > 0 { "," } else { "" }, d.0, d.1);
    }
    o.push_str("]}");
}

/// Rank header, threads, source locations and parallel regions (without the
/// closing brace), shared by the full and the collapsed graph.
fn header_json(m: &Model, crit: &PathResult, locs: &[Loc], loc_of: &HashMap<u64, usize>) -> String {
    let loc = |addr: u64| loc_of.get(&addr).map_or(-1, |&i| i as i64);
    let peak = analysis::analyze_region(m, &vec![true; m.tasks.len()]).max_concurrency;
    let mut o = String::with_capacity(m.tasks.len() * 200);
    let _ = write!(
        o,
        "{{\"rank\":{},\"host\":{},\"pid\":{},\"wall\":{},\"work\":{},\"critical\":{},\"peak\":{},",
        m.rank,
        esc(&m.host),
        m.pid,
        m.wall,
        m.total_work(),
        crit.length,
        peak
    );
    o.push_str("\"info\":");
    list(&mut o, m.info.iter().map(|(k, v)| format!("[{},{}]", esc(k), esc(v))));
    o.push_str(",\"threads\":[");
    for (i, t) in m.threads.iter().enumerate() {
        let _ = write!(o, "{}{{\"id\":{},\"tid\":{},\"type\":{}}}", if i > 0 { "," } else { "" }, t.id, t.os_tid, t.thread_type);
    }
    o.push_str("],\"locs\":[");
    for (i, l) in locs.iter().enumerate() {
        let _ = write!(
            o,
            "{}{{\"addr\":\"{:#x}\",\"module\":{},\"func\":{},\"file\":{},\"line\":{}}}",
            if i > 0 { "," } else { "" },
            l.addr,
            esc(&l.module),
            esc(&l.func),
            esc(&l.file),
            l.line
        );
    }
    o.push_str("],\"parallels\":[");
    for (i, p) in m.parallels.iter().enumerate() {
        let _ = write!(
            o,
            "{}{{\"id\":{},\"parent\":{},\"requested\":{},\"actual\":{},\"loc\":{},\"begin\":{},\"end\":{}}}",
            if i > 0 { "," } else { "" },
            p.id,
            idx(p.parent_task),
            p.requested,
            p.actual,
            loc(p.codeptr),
            p.begin,
            p.end
        );
    }
    o.push(']');
    o
}

/// Task origins; "k" is a bit set: 1 parallel region (implicit tasks),
/// 2 task construct, 4 taskloop.
fn origins_json(m: &Model, crit: &PathResult, loc_of: &HashMap<u64, usize>) -> String {
    let loc = |addr: u64| loc_of.get(&addr).map_or(-1, |&i| i as i64);
    let mut o = String::from("[");
    for (i, g) in analysis::origins(m, crit).iter().enumerate() {
        let kinds = g.has_implicit as u8 | (g.has_explicit as u8) << 1 | (g.has_taskloop as u8) << 2;
        let _ = write!(
            o,
            "{}{{\"initial\":{},\"loc\":{},\"k\":{},\"n\":{},\"w\":{},\"cr\":{},\"own_cr\":{},\"peak\":{},\"tasks\":[",
            if i > 0 { "," } else { "" },
            g.initial as u8,
            if g.initial { -1 } else { loc(g.codeptr) },
            kinds,
            g.tasks.len(),
            g.work,
            g.critical,
            g.own_critical,
            g.peak
        );
        for (j, t) in g.tasks.iter().enumerate() {
            let _ = write!(o, "{}{}", if j > 0 { "," } else { "" }, t);
        }
        o.push_str("]}");
    }
    o.push(']');
    o
}

/// The complete graph of a rank in one document, with a server-side layout.
fn full_json(m: &Model, crit: &PathResult, locs: &[Loc], loc_of: &HashMap<u64, usize>) -> String {
    let pos = layout::layout(m);
    let mut o = header_json(m, crit, locs, loc_of);
    o.push_str(",\"tasks\":[");
    for i in 0..m.tasks.len() {
        if i > 0 {
            o.push(',');
        }
        task_json(&mut o, m, crit, loc_of, i, Some(pos[i]), None);
    }
    let _ = write!(o, "],\"origins\":{},\"edges\":[", origins_json(m, crit, loc_of));
    for (i, e) in m.edges.iter().enumerate() {
        let on_path = crit.transitions.contains(&(e.from, e.to)) || crit.transitions.contains(&(e.to, e.from));
        let _ = write!(o, "{}[{},{},{},{}]", if i > 0 { "," } else { "" }, e.from, e.to, e.kind as u8, on_path as u8);
    }
    o.push_str("]}");
    o
}

fn list<T: std::fmt::Display>(o: &mut String, values: impl Iterator<Item = T>) {
    o.push('[');
    for (i, v) in values.enumerate() {
        let _ = write!(o, "{}{}", if i > 0 { "," } else { "" }, v);
    }
    o.push(']');
}

/// Summary of a set of tasks (`own_cr` and `peak` are null where the
/// parallelism was not computed): statistics and a "typical task" record (`typ`,
/// with the keys of a task record) the GUI colours the group node with.
fn group_summary_json(s: &analysis::Summary, loc_of: &HashMap<u64, usize>) -> String {
    let segs = if s.cpu >= 0 {
        format!("[[{},{},{},{},{},{}]]", s.thread, s.cpu, s.cpu, s.numa.max(0), s.mean_start, s.mean_start)
    } else {
        "[]".to_string()
    };
    format!(
        "{{\"n\":{},\"w\":{},\"cr\":{},\"min\":{},\"max\":{},\"t0\":{},\"t1\":{},\"threads\":{},\"own_cr\":{},\"peak\":{},\
         \"typ\":{{\"k\":{},\"loc\":{},\"pr\":{},\"cth\":{},\"ct\":{},\"w\":{},\"cr\":0,\"segs\":{}}}}}",
        s.n,
        s.work,
        s.critical,
        s.min_work,
        s.max_work,
        s.t_begin,
        s.t_end,
        s.threads,
        s.parallelism.map_or("null".to_string(), |p| p.0.to_string()),
        s.parallelism.map_or("null".to_string(), |p| p.1.to_string()),
        s.kind,
        loc_of.get(&s.codeptr).map_or(-1, |&l| l as i64),
        idx(s.parallel),
        s.thread,
        s.mean_create,
        s.mean_work,
        segs
    )
}

/// The graph as the GUI loads it first, with all groups collapsed:
///
/// * `ids`, `par`: id and parent index of *every* task (creation edges are
///   exactly the parent relation); `crit`: the tasks on the critical path;
///   `cc`: tasks whose creation edge is on it;
/// * `cats`, `ranges`: the values of each colour category and the range of
///   each time over all tasks, so colours do not change as groups load;
/// * `tasks`: records of the tasks outside groups only;
/// * `groups`: the tree of groups, each with its summary, its direct tasks
///   and its child groups; `max_group`: the size limit they were made for.
///
/// The records of a group's direct tasks are served by `group_json`, the
/// dependence edges between the visible nodes by `view_json`.
fn graph_json(m: &Model, crit: &PathResult, locs: &[Loc], loc_of: &HashMap<u64, usize>, grouping: &analysis::Grouping) -> String {
    use std::collections::BTreeSet;
    let loc = |addr: u64| loc_of.get(&addr).map_or(-1, |&i| i as i64);
    let n = m.tasks.len();
    let mut o = header_json(m, crit, locs, loc_of);
    let _ = write!(o, ",\"n\":{n},\"ids\":");
    list(&mut o, m.tasks.iter().map(|t| t.id));
    o.push_str(",\"par\":");
    list(&mut o, m.tasks.iter().map(|t| idx(t.parent)));
    o.push_str(",\"crit\":");
    list(&mut o, crit.task_time.iter().enumerate().filter(|(_, &c)| c > 0).map(|(i, c)| format!("[{i},{c}]")));
    o.push_str(",\"cc\":");
    let on_path = |a: u32, b: u32| crit.transitions.contains(&(a, b)) || crit.transitions.contains(&(b, a));
    list(&mut o, m.tasks.iter().enumerate().filter(|(i, t)| t.parent != NONE && on_path(t.parent, *i as u32)).map(|(i, _)| i));

    // Category values and time ranges over all tasks; the rules match
    // catValue and SEQUENTIAL in web/app.js.
    let mut cats: [BTreeSet<i64>; 6] = Default::default();
    let mut ranges = [[u64::MAX, 0u64]; 4];
    for t in &m.tasks {
        let seg = t.segs.first();
        let start = analysis::task_start(t);
        cats[0].insert(analysis::task_thread(t));
        cats[1].insert(seg.map_or(-1, |s| s.cpu as i64));
        cats[2].insert(seg.map_or(-1, |s| s.numa as i64));
        cats[3].insert(loc(t.codeptr));
        cats[4].insert(idx(t.parallel));
        cats[5].insert(t.kind as i64);
        for (r, v) in ranges.iter_mut().zip([t.work, t.create_ts, start, start.saturating_sub(t.create_ts)]) {
            r[0] = r[0].min(v);
            r[1] = r[1].max(v);
        }
    }
    o.push_str(",\"cats\":{");
    for (i, name) in ["thread", "cpu", "numa", "loc", "region", "kind"].iter().enumerate() {
        let _ = write!(o, "{}\"{name}\":", if i > 0 { "," } else { "" });
        list(&mut o, cats[i].iter());
    }
    o.push_str("},\"ranges\":{");
    for (i, name) in ["work", "create", "start", "wait"].iter().enumerate() {
        let _ = write!(o, "{}\"{name}\":[{},{}]", if i > 0 { "," } else { "" }, ranges[i][0].min(ranges[i][1]), ranges[i][1]);
    }

    let top: Vec<bool> = grouping.task_group.iter().map(|&g| g == NONE).collect();
    let deps = dep_lists(m, &top);
    let _ = write!(o, "}},\"max_group\":{},\"tasks\":[", grouping.max_group);
    let mut first = true;
    for i in (0..n).filter(|&i| top[i]) {
        if !first {
            o.push(',');
        }
        first = false;
        task_json(&mut o, m, crit, loc_of, i, None, Some(&deps));
    }
    o.push_str("],\"groups\":[");
    let summaries = analysis::summarize_groups(m, crit, grouping);
    for (g, grp) in grouping.groups.iter().enumerate() {
        let kind = match grp.kind {
            analysis::GroupKind::Taskloop => "taskloop",
            analysis::GroupKind::Fan => "fan",
            analysis::GroupKind::Subtree => "subtree",
        };
        let part = grp.part.map_or("null".to_string(), |(first, count, total)| format!("[{first},{count},{total}]"));
        let _ = write!(
            o,
            "{}{{\"kind\":\"{kind}\",\"anchor\":{},\"loc\":{},\"parent\":{},\"rep\":{},\"part\":{part},\"sum\":{},\"tasks\":",
            if g > 0 { "," } else { "" },
            grp.anchor,
            loc(grp.codeptr),
            idx(grp.parent),
            grp.rep,
            group_summary_json(&summaries[g], loc_of)
        );
        list(&mut o, grp.tasks.iter());
        o.push_str(",\"sub\":");
        list(&mut o, grp.children.iter());
        o.push('}');
    }
    let _ = write!(o, "],\"origins\":{}}}", origins_json(m, crit, loc_of));
    o
}

/// The records of the tasks a group shows when it is expanded.
fn group_json(r: &Rank, grouping: &analysis::Grouping, g: usize) -> String {
    let (m, grp) = (&r.model, &grouping.groups[g]);
    let mut member = vec![false; m.tasks.len()];
    for &i in &grp.tasks {
        member[i as usize] = true;
    }
    let deps = dep_lists(m, &member);
    let mut o = String::from("{\"tasks\":[");
    for (j, &i) in grp.tasks.iter().enumerate() {
        if j > 0 {
            o.push(',');
        }
        task_json(&mut o, m, &r.crit, &r.loc_of, i as usize, None, Some(&deps));
    }
    o.push_str("]}");
    o
}

/// Parallelism of a group: critical path through its tasks alone and their
/// peak concurrency. Computed on request, since it needs a pass over the graph.
fn group_stats_json(r: &Rank, grouping: &analysis::Grouping, g: usize) -> String {
    let mut selected = vec![false; r.model.tasks.len()];
    for i in grouping.members(g) {
        selected[i as usize] = true;
    }
    let region = analysis::analyze_region(&r.model, &selected);
    format!("{{\"own_cr\":{},\"peak\":{}}}", region.critical, region.max_concurrency)
}

/// Dependence edges `[from, to, critical]` between the nodes visible when
/// the groups listed in the request body are expanded; a collapsed group is
/// represented by its `rep` task.
fn view_json(r: &Rank, grouping: &analysis::Grouping, body: &str) -> String {
    let mut expanded = vec![false; grouping.groups.len()];
    for g in parse_indices(body, grouping.groups.len()) {
        expanded[g as usize] = true;
    }
    let mut o = String::from("{\"deps\":");
    list(&mut o, analysis::visible_dependences(&r.model, &r.crit, grouping, &expanded).iter().map(|&(a, b, c)| format!("[{a},{b},{}]", c as u8)));
    o.push('}');
    o
}

/// The critical path strip for a time window: the run's time classified
/// around the critical path (`analysis::critical_gaps`; class 0 on the path,
/// 1 all threads busy, 2 critical task waiting, 3 idle). `total` is the time
/// per class over the whole run. If the window holds few intervals they are
/// listed in `iv` as `[begin, end, class]`; otherwise `cls` gives for each of
/// the `cols` pixel columns the class taking most of it (-1: outside the run).
fn gap_strip_json(r: &Rank, t0: f64, t1: f64, cols: usize) -> String {
    let gaps = r.gaps.get_or_init(|| analysis::critical_gaps(&r.model, &r.crit));
    let iv = &gaps.intervals;
    let (a, b) = (iv.partition_point(|i| (i.1 as f64) <= t0), iv.partition_point(|i| (i.0 as f64) < t1));
    let mut o = String::from("{\"total\":");
    list(&mut o, gaps.totals.iter());
    if b.saturating_sub(a) <= TIMELINE_MAX_SEGMENTS {
        o.push_str(",\"iv\":");
        list(&mut o, iv[a..b.max(a)].iter().map(|&(x, y, k)| format!("[{x},{y},{}]", k as u8)));
    } else {
        let per = (t1 - t0) / cols as f64;
        let mut time = vec![[0f64; 4]; cols];
        for &(x, y, k) in &iv[a..b] {
            let (fa, fb) = (((x as f64).max(t0) - t0) / per, ((y as f64).min(t1) - t0) / per);
            for c in (fa as usize)..(fb.ceil() as usize).min(cols) {
                time[c][k as usize] += (fb.min(c as f64 + 1.0) - fa.max(c as f64)).max(0.0);
            }
        }
        o.push_str(",\"cls\":");
        list(&mut o, time.iter().map(|t| (0..4).filter(|&k| t[k] > 0.0).max_by(|&x, &y| t[x].total_cmp(&t[y])).map_or(-1, |k| k as i32)));
    }
    o.push('}');
    o
}

/// Task indices from a comma-separated request body.
fn parse_indices(body: &str, n: usize) -> Vec<u32> {
    let mut v: Vec<u32> = body.split(',').filter_map(|t| t.trim().parse().ok()).filter(|&i| (i as usize) < n).collect();
    v.sort_unstable();
    v.dedup();
    v
}

fn region_json(m: &Model, body: &str) -> String {
    let mut selected = vec![false; m.tasks.len()];
    for tok in body.split(',') {
        if let Ok(i) = tok.trim().parse::<usize>() {
            if i < selected.len() {
                selected[i] = true;
            }
        }
    }
    let r = analysis::analyze_region(m, &selected);
    let span = r.t_end.saturating_sub(r.t_begin);
    let mut o = String::new();
    let _ = write!(
        o,
        "{{\"n\":{},\"work\":{},\"critical\":{},\"t_begin\":{},\"t_end\":{},\"max\":{},\"profile\":[",
        r.n_tasks, r.work, r.critical, r.t_begin, r.t_end, r.max_concurrency
    );
    for (i, (t, v)) in analysis::downsample(&r.profile, r.t_begin, r.t_end, 240).iter().enumerate() {
        let _ = write!(o, "{}[{},{:.3}]", if i > 0 { "," } else { "" }, t, v);
    }
    let _ = write!(o, "],\"bucket\":{},\"per_thread\":[", span as f64 / 240.0);
    for (i, (th, w)) in r.per_thread.iter().enumerate() {
        let _ = write!(o, "{}[{},{}]", if i > 0 { "," } else { "" }, th, w);
    }
    o.push_str("],\"crit_tasks\":[");
    let mut first = true;
    for (i, &t) in r.path.task_time.iter().enumerate() {
        if t > 0 {
            let _ = write!(o, "{}[{},{}]", if first { "" } else { "," }, i, t);
            first = false;
        }
    }
    o.push_str("],\"crit_edges\":[");
    for (i, (a, b)) in r.path.transitions.iter().enumerate() {
        let _ = write!(o, "{}[{},{}]", if i > 0 { "," } else { "" }, a, b);
    }
    o.push_str("]}");
    o
}

// ------------------------------------------------------------------ HTTP

fn respond(stream: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

fn handle(mut stream: TcpStream, info: &str, ranks: &[Rank]) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut it = line.split_whitespace();
    let (method, path) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let (path, query) = (path.to_string(), query.to_string());
    let method = method.to_string();
    let mut content_length = 0usize;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; content_length.min(16 << 20)];
    if reader.read_exact(&mut body).is_err() {
        return;
    }

    const JSON: &str = "application/json";
    let seg: Vec<&str> = path.trim_matches('/').split('/').collect();
    match (method.as_str(), seg.as_slice()) {
        ("GET", [""]) => respond(&mut stream, "200 OK", "text/html; charset=utf-8", include_bytes!("../web/index.html")),
        ("GET", ["app.js"]) => respond(&mut stream, "200 OK", "text/javascript; charset=utf-8", include_bytes!("../web/app.js")),
        ("GET", ["style.css"]) => respond(&mut stream, "200 OK", "text/css; charset=utf-8", include_bytes!("../web/style.css")),
        ("GET", ["api", "info"]) => respond(&mut stream, "200 OK", JSON, info.as_bytes()),
        ("GET", ["api", "ranks"]) => {
            let list: Vec<String> = ranks.iter().map(summary_json).collect();
            respond(&mut stream, "200 OK", JSON, format!("[{}]", list.join(",")).as_bytes());
        }
        ("GET", ["api", "rank", r, "group", g]) => {
            let rank = r.parse::<u32>().ok().and_then(|r| ranks.iter().find(|x| x.model.rank == r));
            let json = rank.zip(g.parse::<usize>().ok()).and_then(|(rank, g)| {
                let view = rank.view.read().unwrap();
                (g < view.grouping.groups.len()).then(|| group_json(rank, &view.grouping, g))
            });
            match json {
                Some(json) => respond(&mut stream, "200 OK", JSON, json.as_bytes()),
                None => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
            }
        }
        ("GET", ["api", "rank", r, "group", g, "stats"]) => {
            let rank = r.parse::<u32>().ok().and_then(|r| ranks.iter().find(|x| x.model.rank == r));
            let json = rank.zip(g.parse::<usize>().ok()).and_then(|(rank, g)| {
                let view = rank.view.read().unwrap();
                (g < view.grouping.groups.len()).then(|| group_stats_json(rank, &view.grouping, g))
            });
            match json {
                Some(json) => respond(&mut stream, "200 OK", JSON, json.as_bytes()),
                None => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
            }
        }
        (_, ["api", "rank", r, what]) => {
            let rank = r.parse::<u32>().ok().and_then(|r| ranks.iter().find(|x| x.model.rank == r));
            match (method.as_str(), rank, *what) {
                ("GET", Some(rank), "graph") => {
                    // "?max_group=N" regroups the rank for another size limit.
                    let wanted = query.split('&').find_map(|kv| kv.strip_prefix("max_group=")).and_then(|v| v.parse::<usize>().ok());
                    if let Some(max) = wanted.map(|m| m.max(analysis::MIN_MAX_GROUP)) {
                        if rank.view.read().unwrap().grouping.max_group != max {
                            let regrouped = rank.regroup(max);
                            *rank.view.write().unwrap() = regrouped;
                        }
                    }
                    respond(&mut stream, "200 OK", JSON, rank.view.read().unwrap().graph_json.as_bytes());
                }
                ("POST", Some(rank), "view") => {
                    let json = view_json(rank, &rank.view.read().unwrap().grouping, &String::from_utf8_lossy(&body));
                    respond(&mut stream, "200 OK", JSON, json.as_bytes());
                }
                ("GET", Some(rank), "full") => {
                    let json = full_json(&rank.model, &rank.crit, &rank.locs, &rank.loc_of);
                    respond(&mut stream, "200 OK", JSON, json.as_bytes());
                }
                ("POST", Some(rank), "summary") => {
                    let members = parse_indices(&String::from_utf8_lossy(&body), rank.model.tasks.len());
                    let json = group_summary_json(&analysis::summarize(&rank.model, &rank.crit, &members), &rank.loc_of);
                    respond(&mut stream, "200 OK", JSON, json.as_bytes());
                }
                ("POST", Some(rank), "timeline") => {
                    let json = timeline_json(rank, &String::from_utf8_lossy(&body));
                    respond(&mut stream, "200 OK", JSON, json.as_bytes());
                }
                ("POST", Some(rank), "timeline-select") => {
                    let json = timeline_select_json(rank, &String::from_utf8_lossy(&body));
                    respond(&mut stream, "200 OK", JSON, json.as_bytes());
                }
                ("POST", Some(rank), "region") => {
                    // Remember the region, so the timeline can mark its tasks.
                    let body = String::from_utf8_lossy(&body);
                    let mut selected = vec![false; rank.model.tasks.len()];
                    let members = parse_indices(&body, selected.len());
                    for &i in &members {
                        selected[i as usize] = true;
                    }
                    *rank.selection.write().unwrap() = if members.is_empty() { Vec::new() } else { selected };
                    let json = region_json(&rank.model, &body);
                    respond(&mut stream, "200 OK", JSON, json.as_bytes());
                }
                _ => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
            }
        }
        _ => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
    }
}

// ------------------------------------------------------------------ main

fn load(dir: &str, max_group: usize) -> Vec<Rank> {
    let mut files: Vec<_> = match std::fs::read_dir(dir) {
        Ok(d) => d.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "tmj")).collect(),
        Err(e) => {
            eprintln!("temanejo: cannot read trace directory {dir}: {e}");
            std::process::exit(1);
        }
    };
    files.sort();
    let mut ranks = Vec::new();
    for path in files {
        let trace = std::fs::File::open(&path).and_then(|f| tmj_trace::Trace::read(&mut BufReader::new(f)));
        let trace = match trace {
            Ok(t) => t,
            Err(e) => {
                eprintln!("temanejo: skipping {}: {e}", path.display());
                continue;
            }
        };
        if !trace.complete {
            eprintln!(
                "temanejo: {} has no end marker: the run of rank {} did not finish cleanly; showing the {} events recorded up to its last flush",
                path.display(),
                trace.rank,
                trace.events.len()
            );
        }
        if trace.events.is_empty() {
            eprintln!("temanejo: skipping {}: it contains no events", path.display());
            continue;
        }
        let model = Model::build(&trace);
        let crit = analysis::critical_path(&model, None);
        let locs = symbolize::symbolize(&trace.syms);
        let loc_of: HashMap<u64, usize> = locs.iter().enumerate().map(|(i, l)| (l.addr, i)).collect();
        let empty = Grouped { grouping: analysis::Grouping { max_group: 0, groups: Vec::new(), task_group: Vec::new() }, graph_json: String::new() };
        let rank = Rank { model, crit, locs, loc_of, view: std::sync::RwLock::new(empty), epoch_ns: trace.epoch_ns, complete: trace.complete, gaps: Default::default(), timeline: Default::default(), selection: Default::default() };
        let grouped = rank.regroup(max_group);
        *rank.view.write().unwrap() = grouped;
        ranks.push(rank);
    }
    ranks.sort_by_key(|r| r.model.rank);
    ranks
}

fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

fn main() {
    let mut dir = None;
    let mut port = 8700u16;
    let mut bind = "127.0.0.1".to_string();
    let mut summary_only = false;
    let mut max_group = analysis::DEFAULT_MAX_GROUP;
    const USAGE: &str = "usage: temanejo <trace-dir> [-p|--port PORT] [--bind ADDR] [--max-group N] [--summary] [-h|--help]";
    let usage_error = |msg: String| -> ! {
        eprintln!("temanejo: {msg}\n{USAGE}");
        std::process::exit(2);
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" | "-p" => {
                port = match args.next().map(|v| v.parse()) {
                    Some(Ok(p)) if p > 0 => p,
                    _ => usage_error(format!("{a} needs a port number (1-65535)")),
                }
            }
            "--bind" => bind = args.next().unwrap_or_else(|| usage_error("--bind needs an address".into())),
            "--max-group" => {
                max_group = match args.next().map(|v| v.parse::<usize>()) {
                    Some(Ok(n)) if n >= analysis::MIN_MAX_GROUP => n,
                    _ => usage_error(format!("--max-group needs a number of at least {}", analysis::MIN_MAX_GROUP)),
                }
            }
            "--summary" => summary_only = true,
            "--help" | "-h" => {
                println!("{USAGE}\n\nSee temanejo(1) for details.");
                return;
            }
            _ if a.starts_with('-') => usage_error(format!("unknown option {a}")),
            _ if dir.is_some() => usage_error("only one trace directory can be given".into()),
            _ => dir = Some(a),
        }
    }
    let Some(dir) = dir else { usage_error("no trace directory given".into()) };

    let ranks = load(&dir, max_group);
    if ranks.is_empty() {
        eprintln!("temanejo: no rank-*.tmj files in {dir}");
        std::process::exit(1);
    }
    println!("rank  host              tasks  threads   wall[ms]   work[ms]  crit.path[ms]  avg.parallelism");
    for r in &ranks {
        let m = &r.model;
        let work = m.total_work();
        println!(
            "{:>4}  {:<16} {:>6}  {:>7}  {:>9.2}  {:>9.2}  {:>13.2}  {:>15.2}",
            m.rank,
            m.host,
            m.tasks.len(),
            m.threads.len(),
            ms(m.wall),
            ms(work),
            ms(r.crit.length),
            work as f64 / r.crit.length.max(1) as f64
        );
    }
    if summary_only {
        return;
    }

    let listener = match TcpListener::bind((bind.as_str(), port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("temanejo: cannot listen on {bind}:{port}: {e}");
            std::process::exit(1);
        }
    };
    println!("\ntemanejo GUI: http://{bind}:{port}/   (Ctrl-C to stop)");
    let state = Arc::new((info_json(&dir, &ranks), ranks));
    for stream in listener.incoming().flatten() {
        let state = Arc::clone(&state);
        std::thread::spawn(move || handle(stream, &state.0, &state.1));
    }
}
