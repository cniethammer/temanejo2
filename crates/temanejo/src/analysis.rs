//! Critical path and degree-of-parallelism analysis on the part graph.

use crate::model::{Model, TaskKind, NONE};
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

/// Tasks grouped by the code location that created them.
#[derive(Debug, Default)]
pub struct Origin {
    /// The group of the initial task (serial code); `codeptr` is then 0.
    pub initial: bool,
    /// Code address of the creating construct (`task`, `taskloop` or
    /// `parallel`), 0 if unknown.
    pub codeptr: u64,
    /// Task indices, in creation order.
    pub tasks: Vec<u32>,
    /// Total exclusive work of the tasks (ns).
    pub work: u64,
    /// Part of that work on the critical path (ns).
    pub critical: u64,
    /// Critical path through these tasks alone (ns), as in `analyze_region`;
    /// `work / own_critical` is their average parallelism.
    pub own_critical: u64,
    /// Largest number of these tasks executing at the same time.
    pub peak: u32,
    pub has_implicit: bool,
    pub has_explicit: bool,
    pub has_taskloop: bool,
}

/// Groups all tasks by origin: the initial task forms its own group, every
/// other task is grouped by the code address of the construct that created
/// it. Every task is in exactly one group, so work and critical times of the
/// groups add up to T_1 and T_inf. Sorted by work, largest first.
pub fn origins(m: &Model, crit: &PathResult) -> Vec<Origin> {
    let mut groups: std::collections::HashMap<(bool, u64), Origin> = Default::default();
    for (i, t) in m.tasks.iter().enumerate() {
        let initial = t.kind == TaskKind::Initial;
        let codeptr = if initial { 0 } else { t.codeptr };
        let o = groups.entry((initial, codeptr)).or_insert_with(|| Origin { initial, codeptr, ..Default::default() });
        o.tasks.push(i as u32);
        o.work += t.work;
        o.critical += crit.task_time[i];
        match t.kind {
            TaskKind::Implicit => o.has_implicit = true,
            TaskKind::Explicit if t.taskloop => o.has_taskloop = true,
            TaskKind::Explicit => o.has_explicit = true,
            TaskKind::Initial => {}
        }
    }
    let mut v: Vec<Origin> = groups.into_values().collect();
    let mut sel = vec![false; m.tasks.len()];
    for o in &mut v {
        for &t in &o.tasks {
            sel[t as usize] = true;
        }
        let r = analyze_region(m, &sel);
        o.own_critical = r.critical;
        o.peak = r.max_concurrency;
        for &t in &o.tasks {
            sel[t as usize] = false;
        }
    }
    v.sort_by(|a, b| b.work.cmp(&a.work).then(a.initial.cmp(&b.initial)).then(a.codeptr.cmp(&b.codeptr)));
    v
}

// ------------------------------------------------------------------ groups

/// Minimum number of sibling tasks from one location that form a group.
pub const GROUP_MIN_SIBLINGS: usize = 8;
/// Minimum number of descendants for which an explicit task's subtree
/// becomes a group.
pub const GROUP_MIN_SUBTREE: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GroupKind {
    /// The tasks one task created for a taskloop construct.
    Taskloop,
    /// Sibling tasks created by one task at the same code location.
    Fan,
    /// All descendants of an explicit task (e.g. a recursion).
    Subtree,
}

/// A set of tasks the GUI can show as one node and load on demand.
#[derive(Debug)]
pub struct Group {
    pub kind: GroupKind,
    /// Creating task of the siblings, or the root of the subtree (not a member).
    pub anchor: u32,
    pub codeptr: u64,
    /// Task indices in ascending (creation) order.
    pub members: Vec<u32>,
}

/// Children of every task, in creation order.
pub fn children(m: &Model) -> Vec<Vec<u32>> {
    let mut ch = vec![Vec::new(); m.tasks.len()];
    for (i, t) in m.tasks.iter().enumerate() {
        if t.parent != NONE {
            ch[t.parent as usize].push(i as u32);
        }
    }
    ch
}

fn descendants(children: &[Vec<u32>], roots: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    let mut stack = roots.to_vec();
    while let Some(i) = stack.pop() {
        out.push(i);
        stack.extend_from_slice(&children[i as usize]);
    }
    out.sort_unstable();
    out
}

/// Detects the task groups of a graph:
///
/// 1. For every task, its explicit children are bucketed by code location
///    (taskloop tasks separately); a bucket of at least
///    `GROUP_MIN_SIBLINGS` tasks forms a group together with everything
///    these tasks create.
/// 2. Of the explicit tasks outside those groups, the topmost ones with at
///    least `GROUP_MIN_SUBTREE` descendants get a subtree group of their
///    descendants, unless a group of step 1 already hangs below them.
///
/// Groups may nest (a group inside another one is a subset of it); they
/// never overlap partially.
pub fn groups(m: &Model) -> Vec<Group> {
    let n = m.tasks.len();
    let ch = children(m);
    let mut groups = Vec::new();
    let mut in_group = vec![false; n];
    let mut has_fan = vec![false; n];
    for (p, kids) in ch.iter().enumerate() {
        if kids.len() < GROUP_MIN_SIBLINGS {
            continue;
        }
        let mut buckets: Vec<((u64, bool), Vec<u32>)> = Vec::new();
        for &c in kids {
            let t = &m.tasks[c as usize];
            if t.kind != TaskKind::Explicit {
                continue;
            }
            let key = (t.codeptr, t.taskloop);
            match buckets.iter_mut().find(|b| b.0 == key) {
                Some(b) => b.1.push(c),
                None => buckets.push((key, vec![c])),
            }
        }
        for ((codeptr, taskloop), sib) in buckets {
            if sib.len() < GROUP_MIN_SIBLINGS {
                continue;
            }
            let members = descendants(&ch, &sib);
            for &i in &members {
                in_group[i as usize] = true;
            }
            has_fan[p] = true;
            let kind = if taskloop { GroupKind::Taskloop } else { GroupKind::Fan };
            groups.push(Group { kind, anchor: p as u32, codeptr, members });
        }
    }
    // Subtree sizes: parents are created before their children, so one
    // backward sweep accumulates them.
    let mut size = vec![0usize; n];
    for i in (0..n).rev() {
        let p = m.tasks[i].parent;
        if p != NONE {
            size[p as usize] += size[i] + 1;
        }
    }
    let mut stack: Vec<u32> = (0..n as u32).filter(|&i| m.tasks[i as usize].parent == NONE).collect();
    while let Some(i) = stack.pop() {
        let iu = i as usize;
        if in_group[iu] {
            continue;
        }
        if m.tasks[iu].kind == TaskKind::Explicit && size[iu] >= GROUP_MIN_SUBTREE && !has_fan[iu] {
            groups.push(Group { kind: GroupKind::Subtree, anchor: i, codeptr: m.tasks[iu].codeptr, members: descendants(&ch, &ch[iu]) });
        } else {
            stack.extend_from_slice(&ch[iu]);
        }
    }
    groups
}

/// For every task the outermost group containing it (`NONE` if none).
pub fn top_group(n_tasks: usize, groups: &[Group]) -> Vec<u32> {
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by_key(|&g| std::cmp::Reverse(groups[g].members.len()));
    let mut top = vec![NONE; n_tasks];
    for g in order {
        for &i in &groups[g].members {
            if top[i as usize] == NONE {
                top[i as usize] = g as u32;
            }
        }
    }
    top
}

/// What the GUI shows for a set of tasks without having their records.
#[derive(Debug, Default)]
pub struct Summary {
    pub n: usize,
    pub work: u64,
    /// Work on the critical path of the whole graph.
    pub critical: u64,
    pub min_work: u64,
    pub max_work: u64,
    /// Start of the first and end of the last execution segment.
    pub t_begin: u64,
    pub t_end: u64,
    pub threads: usize,
    /// Critical path through these tasks alone; `work / own_critical` is their parallelism.
    pub own_critical: u64,
    pub peak: u32,
    // A typical task, used to colour the group node: the most frequent
    // value of each category and the mean of each time.
    pub thread: i64,
    pub cpu: i64,
    pub numa: i64,
    pub codeptr: u64,
    pub parallel: u32,
    pub kind: u8,
    pub mean_work: u64,
    pub mean_create: u64,
    pub mean_start: u64,
}

fn most_frequent<T: Copy + Ord + std::hash::Hash>(values: impl Iterator<Item = T>) -> Option<T> {
    let mut count: std::collections::HashMap<T, usize> = Default::default();
    for v in values {
        *count.entry(v).or_default() += 1;
    }
    // Ties go to the smaller value, so the result does not depend on hash order.
    count.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0))).map(|(v, _)| v)
}

/// Thread a task counts as running on: that of its first segment, or the
/// creating thread if it never ran. (The GUI uses the same rule.)
pub fn task_thread(t: &crate::model::Task) -> i64 {
    t.segs.first().map_or(t.create_thread as i64, |s| s.thread as i64)
}

/// Start of a task: begin of its first segment, or its creation if it never ran.
pub fn task_start(t: &crate::model::Task) -> u64 {
    t.segs.first().map_or(t.create_ts, |s| s.start)
}

pub fn summarize(m: &Model, crit: &PathResult, members: &[u32]) -> Summary {
    let mut selected = vec![false; m.tasks.len()];
    for &i in members {
        selected[i as usize] = true;
    }
    let region = analyze_region(m, &selected);
    let tasks = || members.iter().map(|&i| &m.tasks[i as usize]);
    let n = members.len().max(1) as u64;
    Summary {
        n: members.len(),
        work: region.work,
        critical: members.iter().map(|&i| crit.task_time[i as usize]).sum(),
        min_work: tasks().map(|t| t.work).min().unwrap_or(0),
        max_work: tasks().map(|t| t.work).max().unwrap_or(0),
        t_begin: region.t_begin,
        t_end: region.t_end,
        threads: region.per_thread.len(),
        own_critical: region.critical,
        peak: region.max_concurrency,
        thread: most_frequent(tasks().map(task_thread)).unwrap_or(-1),
        cpu: most_frequent(tasks().map(|t| t.segs.first().map_or(-1, |s| s.cpu as i64))).unwrap_or(-1),
        numa: most_frequent(tasks().map(|t| t.segs.first().map_or(-1, |s| s.numa as i64))).unwrap_or(-1),
        codeptr: most_frequent(tasks().map(|t| t.codeptr)).unwrap_or(0),
        parallel: most_frequent(tasks().map(|t| t.parallel)).unwrap_or(NONE),
        kind: most_frequent(tasks().map(|t| t.kind as u8)).unwrap_or(2),
        mean_work: region.work / n,
        mean_create: tasks().map(|t| t.create_ts).sum::<u64>() / n,
        mean_start: tasks().map(task_start).sum::<u64>() / n,
    }
}
