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
    graph_json: String,
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
        "{{\"rank\":{},\"host\":{},\"pid\":{},\"tasks\":{},\"threads\":{},\"events\":{},\"wall\":{},\"work\":{},\"critical\":{}}}",
        m.rank,
        esc(&m.host),
        m.pid,
        m.tasks.len(),
        m.threads.len(),
        m.n_events,
        m.wall,
        work,
        r.crit.length
    )
}

fn graph_json(m: &Model, crit: &PathResult, pos: &[(f32, f32)], locs: &[Loc]) -> String {
    let loc_of: HashMap<u64, usize> = locs.iter().enumerate().map(|(i, l)| (l.addr, i)).collect();
    let loc = |addr: u64| loc_of.get(&addr).map_or(-1, |&i| i as i64);
    let mut o = String::with_capacity(m.tasks.len() * 200);
    let _ = write!(
        o,
        "{{\"rank\":{},\"host\":{},\"pid\":{},\"wall\":{},\"work\":{},\"critical\":{},",
        m.rank,
        esc(&m.host),
        m.pid,
        m.wall,
        m.total_work(),
        crit.length
    );
    o.push_str("\"threads\":[");
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
    // Tasks: compact keys, see web/app.js for their meaning.
    o.push_str("],\"tasks\":[");
    for (i, t) in m.tasks.iter().enumerate() {
        let _ = write!(
            o,
            "{}{{\"id\":{},\"k\":{},\"fl\":{},\"par\":{},\"pr\":{},\"ix\":{},\"ct\":{},\"cth\":{},\"loc\":{},\"e\":{},\"w\":{},\"tl\":{},\"x\":{},\"y\":{},\"cr\":{},\"segs\":[",
            if i > 0 { "," } else { "" },
            t.id,
            t.kind as u8,
            t.flags,
            idx(t.parent),
            idx(t.parallel),
            t.team_index,
            t.create_ts,
            t.create_thread,
            loc(t.codeptr),
            t.end,
            t.work,
            t.taskloop as u8,
            pos[i].0,
            pos[i].1,
            crit.task_time[i]
        );
        for (j, s) in t.segs.iter().enumerate() {
            let _ = write!(
                o,
                "{}[{},{},{},{},{},{}]",
                if j > 0 { "," } else { "" },
                s.thread,
                s.cpu,
                s.cpu_end,
                s.numa,
                s.start,
                s.end
            );
        }
        o.push_str("],\"deps\":[");
        for (j, d) in t.deps.iter().enumerate() {
            let _ = write!(o, "{}[\"{:#x}\",{}]", if j > 0 { "," } else { "" }, d.0, d.1);
        }
        o.push_str("]}");
    }
    // Task origins; "k" is a bit set: 1 parallel region (implicit tasks),
    // 2 task construct, 4 taskloop.
    o.push_str("],\"origins\":[");
    for (i, g) in analysis::origins(m, crit).iter().enumerate() {
        let kinds = g.has_implicit as u8 | (g.has_explicit as u8) << 1 | (g.has_taskloop as u8) << 2;
        let _ = write!(
            o,
            "{}{{\"initial\":{},\"loc\":{},\"k\":{},\"n\":{},\"w\":{},\"cr\":{},\"tasks\":[",
            if i > 0 { "," } else { "" },
            g.initial as u8,
            if g.initial { -1 } else { loc(g.codeptr) },
            kinds,
            g.tasks.len(),
            g.work,
            g.critical
        );
        for (j, t) in g.tasks.iter().enumerate() {
            let _ = write!(o, "{}{}", if j > 0 { "," } else { "" }, t);
        }
        o.push_str("]}");
    }
    o.push_str("],\"edges\":[");
    for (i, e) in m.edges.iter().enumerate() {
        let on_path = crit.transitions.contains(&(e.from, e.to)) || crit.transitions.contains(&(e.to, e.from));
        let _ = write!(o, "{}[{},{},{},{}]", if i > 0 { "," } else { "" }, e.from, e.to, e.kind as u8, on_path as u8);
    }
    o.push_str("]}");
    o
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

fn handle(mut stream: TcpStream, ranks: &[Rank]) {
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
        ("GET", ["api", "ranks"]) => {
            let list: Vec<String> = ranks.iter().map(summary_json).collect();
            respond(&mut stream, "200 OK", JSON, format!("[{}]", list.join(",")).as_bytes());
        }
        (_, ["api", "rank", r, what]) => {
            let rank = r.parse::<u32>().ok().and_then(|r| ranks.iter().find(|x| x.model.rank == r));
            match (method.as_str(), rank, *what) {
                ("GET", Some(rank), "graph") => respond(&mut stream, "200 OK", JSON, rank.graph_json.as_bytes()),
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
        let model = Model::build(&trace);
        let crit = analysis::critical_path(&model, None);
        let pos = layout::layout(&model);
        let locs = symbolize::symbolize(&trace.syms);
        let graph_json = graph_json(&model, &crit, &pos, &locs);
        ranks.push(Rank { model, crit, graph_json });
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
    let ranks = Arc::new(ranks);
    for stream in listener.incoming().flatten() {
        let ranks = Arc::clone(&ranks);
        std::thread::spawn(move || handle(stream, &ranks));
    }
}
