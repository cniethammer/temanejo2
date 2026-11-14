//! Critical path and degree-of-parallelism analysis on the part graph.

use crate::model::{Model, NONE};
use std::collections::HashSet;

pub struct PathResult {
    /// Length of the longest (work-weighted) path in ns: T_inf.
    pub length: u64,
    /// Critical work time per task index (0 if the task is not on the path).
    pub task_time: Vec<u64>,
    /// Ordered pairs of task indices between which the path changes task.
    pub transitions: HashSet<(u32, u32)>,
}

/// Longest path through the part graph, optionally restricted to the parts
/// of the tasks flagged in `selected` (virtual join nodes always take part,
/// so two selected tasks stay ordered across a barrier or region end).
pub fn critical_path(m: &Model, selected: Option<&[bool]>) -> PathResult {
    let parts = &m.parts;
    let n = parts.weight.len();
    let included = |p: usize| match (selected, parts.task[p]) {
        (Some(sel), t) if t != NONE => sel[t as usize],
        _ => true,
    };

    // CSR adjacency over included nodes.
    let mut indeg = vec![0u32; n];
    let mut start = vec![0u32; n + 1];
    let edges: Vec<(u32, u32)> =
        parts.edges.iter().copied().filter(|&(a, b)| included(a as usize) && included(b as usize)).collect();
    for &(a, b) in &edges {
        start[a as usize + 1] += 1;
        indeg[b as usize] += 1;
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut adj = vec![0u32; edges.len()];
    for &(a, b) in &edges {
        adj[fill[a as usize] as usize] = b;
        fill[a as usize] += 1;
    }

    // Kahn's algorithm with longest-distance relaxation. Nodes on a cycle
    // (only possible with a damaged trace) are never released and ignored.
    let mut dist = vec![0u64; n];
    let mut pred = vec![NONE; n];
    let mut queue: Vec<u32> = (0..n as u32).filter(|&p| included(p as usize) && indeg[p as usize] == 0).collect();
    let mut best = (0u64, NONE);
    let mut head = 0;
    while head < queue.len() {
        let u = queue[head] as usize;
        head += 1;
        dist[u] += parts.weight[u];
        if dist[u] > best.0 || best.1 == NONE {
            best = (dist[u], u as u32);
        }
        for &v in &adj[start[u] as usize..start[u + 1] as usize] {
            let v = v as usize;
            if dist[u] > dist[v] || pred[v] == NONE {
                dist[v] = dist[u];
                pred[v] = u as u32;
            }
            indeg[v] -= 1;
            if indeg[v] == 0 {
                queue.push(v as u32);
            }
        }
    }

    let mut task_time = vec![0u64; m.tasks.len()];
    let mut transitions = HashSet::new();
    let mut cur = best.1;
    let mut later_task = NONE; // task of the nearest real part after `cur` on the path
    while cur != NONE {
        let t = parts.task[cur as usize];
        if t != NONE {
            task_time[t as usize] += parts.weight[cur as usize];
            if later_task != NONE && later_task != t {
                transitions.insert((t, later_task));
            }
            later_task = t;
        }
        cur = pred[cur as usize];
    }
    PathResult { length: best.0, task_time, transitions }
}

pub struct Region {
    pub n_tasks: usize,
    /// Total work of the selected tasks (T_1).
    pub work: u64,
    /// Critical path length inside the selection (T_inf).
    pub critical: u64,
    pub path: PathResult,
    /// Wall-clock interval covered by the selected tasks' execution.
    pub t_begin: u64,
    pub t_end: u64,
    pub max_concurrency: u32,
    /// Measured concurrency: (time, number of selected tasks executing from then on).
    pub profile: Vec<(u64, u32)>,
    /// Work time per thread id.
    pub per_thread: Vec<(u32, u64)>,
}

pub fn analyze_region(m: &Model, selected: &[bool]) -> Region {
    let path = critical_path(m, Some(selected));
    let mut work = 0u64;
    let mut n_tasks = 0;
    let mut steps: Vec<(u64, i32)> = Vec::new();
    let mut per_thread: std::collections::BTreeMap<u32, u64> = Default::default();
    for (i, t) in m.tasks.iter().enumerate() {
        if !selected[i] {
            continue;
        }
        n_tasks += 1;
        work += t.work;
        for s in &t.segs {
            if s.end > s.start {
                steps.push((s.start, 1));
                steps.push((s.end, -1));
                *per_thread.entry(s.thread).or_default() += s.end - s.start;
            }
        }
    }
    // Ends sort before starts at equal times so back-to-back segments do not overlap.
    steps.sort_unstable();
    let mut profile: Vec<(u64, u32)> = Vec::new();
    let (mut level, mut max) = (0i32, 0i32);
    for &(ts, d) in &steps {
        level += d;
        max = max.max(level);
        match profile.last_mut() {
            Some(last) if last.0 == ts => last.1 = level as u32,
            _ => profile.push((ts, level as u32)),
        }
    }
    Region {
        n_tasks,
        work,
        critical: path.length,
        path,
        t_begin: steps.first().map_or(0, |s| s.0),
        t_end: steps.last().map_or(0, |s| s.0),
        max_concurrency: max as u32,
        profile,
        per_thread: per_thread.into_iter().collect(),
    }
}

/// Reduces a concurrency step function to at most `buckets` time-averaged samples.
pub fn downsample(profile: &[(u64, u32)], t_begin: u64, t_end: u64, buckets: usize) -> Vec<(u64, f64)> {
    if profile.is_empty() || t_end <= t_begin {
        return vec![];
    }
    let span = (t_end - t_begin) as f64;
    let mut acc = vec![0f64; buckets];
    for w in profile.windows(2) {
        let (a, level) = w[0];
        let b = w[1].0;
        if level == 0 {
            continue;
        }
        let fa = (a - t_begin) as f64 / span * buckets as f64;
        let fb = (b - t_begin) as f64 / span * buckets as f64;
        let (ia, ib) = (fa as usize, (fb as usize).min(buckets - 1));
        for (i, slot) in acc.iter_mut().enumerate().take(ib + 1).skip(ia) {
            let lo = fa.max(i as f64);
            let hi = fb.min(i as f64 + 1.0);
            if hi > lo {
                *slot += (hi - lo) * level as f64;
            }
        }
    }
    acc.iter().enumerate().map(|(i, &v)| (t_begin + (span * i as f64 / buckets as f64) as u64, v)).collect()
}
