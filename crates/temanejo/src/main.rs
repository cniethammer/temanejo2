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
    groups: Vec<analysis::Group>,
    /// What the GUI loads first: the graph with all groups collapsed.
    graph_json: String,
    /// Wall-clock time (ns since the Unix epoch) when the tool started in this process.
    epoch_ns: u64,
    /// False if the trace has no end marker: the run did not finish cleanly.
    complete: bool,
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

/// Writes one task record. Compact keys, see web/app.js for their meaning.
/// `i` is the task's index; `x`, `y` are only written with a layout position.
fn task_json(o: &mut String, m: &Model, crit: &PathResult, loc_of: &HashMap<u64, usize>, i: usize, pos: Option<(f32, f32)>) {
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
        task_json(&mut o, m, crit, loc_of, i, Some(pos[i]));
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

/// Summary of a set of tasks: statistics and a "typical task" record (`typ`,
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
        s.own_critical,
        s.peak,
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
/// * `groups`: members and summary of every group;
/// * `edges`: dependence edges between tasks outside groups;
///   `pre`: dependence edges into, out of and between outermost groups,
///   merged per pair of end points, with a group standing in by its first
///   task: `[from, to, 1, critical, from group, to group]` (-1: a task).
///
/// The records and dependence edges of a group are served by `group_json`.
fn graph_json(m: &Model, crit: &PathResult, locs: &[Loc], loc_of: &HashMap<u64, usize>, groups: &[analysis::Group], top: &[u32]) -> String {
    use std::collections::{BTreeMap, BTreeSet};
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

    o.push_str("},\"tasks\":[");
    let mut first = true;
    for i in (0..n).filter(|&i| top[i] == NONE) {
        if !first {
            o.push(',');
        }
        first = false;
        task_json(&mut o, m, crit, loc_of, i, None);
    }
    o.push_str("],\"groups\":[");
    for (g, grp) in groups.iter().enumerate() {
        let kind = match grp.kind {
            analysis::GroupKind::Taskloop => "taskloop",
            analysis::GroupKind::Fan => "fan",
            analysis::GroupKind::Subtree => "subtree",
        };
        let _ = write!(
            o,
            "{}{{\"kind\":\"{kind}\",\"anchor\":{},\"loc\":{},\"top\":{},\"sum\":{},\"members\":",
            if g > 0 { "," } else { "" },
            grp.anchor,
            loc(grp.codeptr),
            (top[grp.members[0] as usize] == g as u32) as u8,
            group_summary_json(&analysis::summarize(m, crit, &grp.members), loc_of)
        );
        list(&mut o, grp.members.iter());
        o.push('}');
    }

    // Dependence edges; a task inside a group is replaced by the first task
    // of its outermost group, edges inside one group are dropped.
    let end = |i: u32| match top[i as usize] {
        NONE => (i, -1i64),
        g => (groups[g as usize].members[0], g as i64),
    };
    let mut plain = Vec::new();
    let mut merged: BTreeMap<(u32, u32), (bool, i64, i64)> = BTreeMap::new();
    for e in m.edges.iter().filter(|e| e.kind == model::EdgeKind::Depend) {
        let ((a, ga), (b, gb)) = (end(e.from), end(e.to));
        let critical = on_path(e.from, e.to);
        if ga < 0 && gb < 0 {
            plain.push(format!("[{a},{b},1,{}]", critical as u8));
        } else if ga != gb {
            merged.entry((a, b)).or_insert((false, ga, gb)).0 |= critical;
        }
    }
    o.push_str("],\"edges\":");
    list(&mut o, plain.iter());
    o.push_str(",\"pre\":");
    list(&mut o, merged.iter().map(|(&(a, b), &(c, ga, gb))| format!("[{a},{b},1,{},{ga},{gb}]", c as u8)));
    let _ = write!(o, ",\"origins\":{}}}", origins_json(m, crit, loc_of));
    o
}

/// The task records of a group and all dependence edges touching its tasks.
fn group_json(r: &Rank, g: usize) -> String {
    let (m, grp) = (&r.model, &r.groups[g]);
    let mut member = vec![false; m.tasks.len()];
    let mut o = String::from("{\"tasks\":[");
    for (j, &i) in grp.members.iter().enumerate() {
        member[i as usize] = true;
        if j > 0 {
            o.push(',');
        }
        task_json(&mut o, m, &r.crit, &r.loc_of, i as usize, None);
    }
    o.push_str("],\"edges\":");
    let touching = m.edges.iter().filter(|e| e.kind == model::EdgeKind::Depend && (member[e.from as usize] || member[e.to as usize]));
    list(
        &mut o,
        touching.map(|e| {
            let on_path = r.crit.transitions.contains(&(e.from, e.to)) || r.crit.transitions.contains(&(e.to, e.from));
            format!("[{},{},1,{}]", e.from, e.to, on_path as u8)
        }),
    );
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
    let path = path.split('?').next().unwrap_or("").to_string();
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
            match (rank, g.parse::<usize>()) {
                (Some(rank), Ok(g)) if g < rank.groups.len() => respond(&mut stream, "200 OK", JSON, group_json(rank, g).as_bytes()),
                _ => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
            }
        }
        (_, ["api", "rank", r, what]) => {
            let rank = r.parse::<u32>().ok().and_then(|r| ranks.iter().find(|x| x.model.rank == r));
            match (method.as_str(), rank, *what) {
                ("GET", Some(rank), "graph") => respond(&mut stream, "200 OK", JSON, rank.graph_json.as_bytes()),
                ("GET", Some(rank), "full") => {
                    let json = full_json(&rank.model, &rank.crit, &rank.locs, &rank.loc_of);
                    respond(&mut stream, "200 OK", JSON, json.as_bytes());
                }
                ("POST", Some(rank), "summary") => {
                    let members = parse_indices(&String::from_utf8_lossy(&body), rank.model.tasks.len());
                    let json = group_summary_json(&analysis::summarize(&rank.model, &rank.crit, &members), &rank.loc_of);
                    respond(&mut stream, "200 OK", JSON, json.as_bytes());
                }
                ("POST", Some(rank), "region") => {
                    let json = region_json(&rank.model, &String::from_utf8_lossy(&body));
                    respond(&mut stream, "200 OK", JSON, json.as_bytes());
                }
                _ => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
            }
        }
        _ => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
    }
}

// ------------------------------------------------------------------ main

fn load(dir: &str) -> Vec<Rank> {
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
        let groups = analysis::groups(&model);
        let top = analysis::top_group(model.tasks.len(), &groups);
        let graph_json = graph_json(&model, &crit, &locs, &loc_of, &groups, &top);
        ranks.push(Rank { model, crit, locs, loc_of, groups, graph_json, epoch_ns: trace.epoch_ns, complete: trace.complete });
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
    const USAGE: &str = "usage: temanejo <trace-dir> [-p|--port PORT] [--bind ADDR] [--summary] [-h|--help]";
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

    let ranks = load(&dir);
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
