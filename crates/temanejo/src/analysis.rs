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
/// Default and smallest allowed maximum group size.
pub const DEFAULT_MAX_GROUP: usize = 1000;
pub const MIN_MAX_GROUP: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GroupKind {
    /// The tasks one task created for a taskloop construct.
    Taskloop,
    /// Sibling tasks created by one task at the same code location.
    Fan,
    /// The descendants of a task (e.g. a recursion).
    Subtree,
}

/// A set of tasks the GUI shows as one node. Groups form a tree: expanding a
/// group reveals its direct `tasks` and its child groups.
#[derive(Debug)]
pub struct Group {
    pub kind: GroupKind,
    /// Creating task of the root tasks, or the root of the subtree (not a member).
    pub anchor: u32,
    pub codeptr: u64,
    /// Enclosing group, `NONE` at the top level.
    pub parent: u32,
    /// Tasks shown when the group is expanded, ascending.
    pub tasks: Vec<u32>,
    /// Groups shown when the group is expanded.
    pub children: Vec<u32>,
    /// Number of tasks in the group, including those in child groups.
    pub n: usize,
    /// First-created task of the group; stands for it in merged edges.
    pub rep: u32,
    /// For a part of a larger sibling group: (index of its first sibling,
    /// number of its siblings, siblings in the whole group).
    pub part: Option<(usize, usize, usize)>,
}

/// The groups of a graph and, for every task, the group it is a direct
/// member of (`NONE` for tasks outside groups).
pub struct Grouping {
    pub max_group: usize,
    pub groups: Vec<Group>,
    pub task_group: Vec<u32>,
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

type Bucket = (u64, bool, Vec<u32>);

/// Splits sibling tasks into buckets of at least `GROUP_MIN_SIBLINGS`
/// explicit tasks with the same code location (taskloop tasks separately),
/// and the remaining single tasks.
fn bucket_siblings(m: &Model, kids: &[u32]) -> (Vec<Bucket>, Vec<u32>) {
    if kids.len() < GROUP_MIN_SIBLINGS {
        return (Vec::new(), kids.to_vec());
    }
    let mut buckets: Vec<Bucket> = Vec::new();
    let mut singles = Vec::new();
    for &c in kids {
        let t = &m.tasks[c as usize];
        if t.kind != TaskKind::Explicit {
            singles.push(c);
            continue;
        }
        match buckets.iter_mut().find(|b| b.0 == t.codeptr && b.1 == t.taskloop) {
            Some(b) => b.2.push(c),
            None => buckets.push((t.codeptr, t.taskloop, vec![c])),
        }
    }
    let (fans, small): (Vec<Bucket>, Vec<Bucket>) = buckets.into_iter().partition(|b| b.2.len() >= GROUP_MIN_SIBLINGS);
    singles.extend(small.into_iter().flat_map(|b| b.2));
    singles.sort_unstable();
    (fans, singles)
}

/// Detects the task groups of a graph so that expanding a group never
/// reveals more than `max_group` nodes (tasks plus child groups).
///
/// **Top level.** Starting at the tasks without parent, every task is shown
/// and its children are examined:
/// * `GROUP_MIN_SIBLINGS` or more explicit children from one code location
///   form a group (taskloop tasks separately);
/// * an explicit task with at least `GROUP_MIN_SUBTREE` descendants and no
///   such sibling group below it gets one subtree group of its descendants;
/// * otherwise its children are shown, and examined in the same way.
///
/// **Inside a group** with root tasks `R` (the siblings, or the children of
/// the subtree's anchor):
/// * a group of at most `max_group` tasks shows all of them;
/// * otherwise consecutive roots are bundled while their subtrees together
///   hold at most `max_group` tasks. A bundle is shown directly if it is
///   small enough for the `max_group / 2` nodes shown so far, else it
///   becomes a child group; if there are too many bundles, several go into
///   one child group, which is split again when it is expanded;
/// * a single subtree larger than `max_group` has its root shown, and its
///   children are placed by the same rules (sibling buckets below it become
///   groups); once `max_group / 2` nodes are shown it becomes a child group
///   instead.
///
/// Child groups are therefore filled close to `max_group` tasks where the
/// graph allows, which keeps their number near (tasks / `max_group`).
pub fn group_tasks(m: &Model, max_group: usize) -> Grouping {
    let max_group = max_group.max(MIN_MAX_GROUP);
    let half = max_group / 2;
    let n = m.tasks.len();
    let ch = children(m);
    // Number of descendants; parents are created before their children, so
    // one backward sweep accumulates them.
    let mut size = vec![0usize; n];
    for i in (0..n).rev() {
        let p = m.tasks[i].parent;
        if p != NONE {
            size[p as usize] += size[i] + 1;
        }
    }
    let mut groups: Vec<Group> = Vec::new();
    // Root tasks of the groups whose content is still to be determined.
    let mut pending: Vec<(u32, Vec<u32>)> = Vec::new();
    let add = |groups: &mut Vec<Group>, pending: &mut Vec<(u32, Vec<u32>)>, kind, anchor: u32, codeptr, parent: u32, roots: Vec<u32>, part| {
        let id = groups.len() as u32;
        let members = roots.iter().map(|&r| size[r as usize] + 1).sum();
        groups.push(Group { kind, anchor, codeptr, parent, tasks: Vec::new(), children: Vec::new(), n: members, rep: roots[0], part });
        if parent != NONE {
            groups[parent as usize].children.push(id);
        }
        pending.push((id, roots));
    };
    let fan_kind = |taskloop: bool| if taskloop { GroupKind::Taskloop } else { GroupKind::Fan };

    // Top level.
    let mut stack: Vec<u32> = (0..n as u32).filter(|&i| m.tasks[i as usize].parent == NONE).collect();
    while let Some(i) = stack.pop() {
        let iu = i as usize;
        if ch[iu].is_empty() {
            continue;
        }
        let (fans, singles) = bucket_siblings(m, &ch[iu]);
        if fans.is_empty() && m.tasks[iu].kind == TaskKind::Explicit && size[iu] >= GROUP_MIN_SUBTREE {
            add(&mut groups, &mut pending, GroupKind::Subtree, i, m.tasks[iu].codeptr, NONE, ch[iu].clone(), None);
            continue;
        }
        for (codeptr, taskloop, sib) in fans {
            add(&mut groups, &mut pending, fan_kind(taskloop), i, codeptr, NONE, sib, None);
        }
        stack.extend(singles);
    }

    // Content of each group; this creates further groups.
    while let Some((g, roots)) = pending.pop() {
        let gu = g as usize;
        if groups[gu].n <= max_group {
            groups[gu].tasks = descendants(&ch, &roots);
            continue;
        }
        let own_kind = groups[gu].kind;
        let mut shown: Vec<u32> = Vec::new();
        // Tasks that will be shown and whose children are still to be placed.
        let mut queue: std::collections::VecDeque<u32> = Default::default();
        // Forests to place: (sibling tasks, are they the group's own roots).
        let mut forests: Vec<(Vec<u32>, bool)> = vec![(roots, true)];
        loop {
            while let Some((list, own)) = forests.pop() {
                // Consecutive siblings are bundled while their subtrees
                // together hold at most `max_group` tasks; a larger subtree
                // stands alone.
                let mut bundles: Vec<(usize, Vec<u32>)> = Vec::new(); // (tasks in the bundle, its roots)
                for &r in &list {
                    let tasks = size[r as usize] + 1;
                    match bundles.last_mut() {
                        Some((total, b)) if tasks <= max_group && *total + tasks <= max_group => {
                            *total += tasks;
                            b.push(r);
                        }
                        _ => bundles.push((tasks, vec![r])),
                    }
                }
                // A part of a sibling group keeps kind and location and is
                // labelled by its position; anything else is a subtree group
                // below the tasks' parent.
                let fan_like = own && own_kind != GroupKind::Subtree;
                let (base, total) = match groups[gu].part {
                    Some((first, _, total)) if own => (first, total),
                    _ => (0, list.len()),
                };
                let anchor = if fan_like { groups[gu].anchor } else { m.tasks[list[0] as usize].parent };
                let (kind, codeptr) = if fan_like { (own_kind, groups[gu].codeptr) } else { (GroupKind::Subtree, m.tasks[anchor as usize].codeptr) };
                let count = |shown: &Vec<u32>, queue: &std::collections::VecDeque<u32>, groups: &Vec<Group>| shown.len() + queue.len() + groups[gu].children.len();
                // Nodes the group may still reveal. A queued task takes two:
                // itself and one group for what is below it.
                let left = |shown: &Vec<u32>, queue: &std::collections::VecDeque<u32>, groups: &Vec<Group>| {
                    max_group.saturating_sub(shown.len() + 2 * queue.len() + groups[gu].children.len())
                };

                let room = half.saturating_sub(count(&shown, &queue, &groups)).min(left(&shown, &queue, &groups)).max(1);
                if bundles.len() > 1 && bundles.len() > room {
                    // Too many bundles for this group: several of them per child group.
                    let per_group = bundles.len().div_ceil(room);
                    let mut offset = 0;
                    for chunk in bundles.chunks(per_group) {
                        let sub: Vec<u32> = chunk.iter().flat_map(|b| b.1.iter().copied()).collect();
                        let len = sub.len();
                        add(&mut groups, &mut pending, kind, anchor, codeptr, g, sub, Some((base + offset, len, total)));
                        offset += len;
                    }
                    continue;
                }
                let split = bundles.len() > 1;
                let mut offset = 0;
                let mut after = bundles.len(); // bundles still to place, each needs at least one node
                for (tasks, b) in bundles {
                    after -= 1;
                    let len = b.len();
                    let part = split.then_some((base + offset, len, total));
                    offset += len;
                    let now = count(&shown, &queue, &groups);
                    let free = left(&shown, &queue, &groups).saturating_sub(after);
                    if tasks > max_group {
                        // One subtree larger than the limit: show its root if
                        // there is room, else it becomes a group of its own.
                        if now < half && free >= 2 {
                            queue.push_back(b[0]);
                        } else {
                            add(&mut groups, &mut pending, kind, anchor, codeptr, g, b, part);
                        }
                    } else if tasks == 1 || (now + tasks <= half && tasks <= free) {
                        shown.extend(descendants(&ch, &b));
                    } else {
                        add(&mut groups, &mut pending, kind, anchor, codeptr, g, b, part);
                    }
                }
            }
            let Some(t) = queue.pop_front() else { break };
            shown.push(t);
            let tu = t as usize;
            if ch[tu].is_empty() {
                continue;
            }
            let (fans, singles) = bucket_siblings(m, &ch[tu]);
            // The children of `t` have at least the one node reserved for
            // them; if its sibling groups do not fit, all its children go
            // into one group.
            let free = max_group.saturating_sub(shown.len() + 2 * queue.len() + groups[gu].children.len());
            if fans.len() + usize::from(!singles.is_empty()) > free {
                add(&mut groups, &mut pending, GroupKind::Subtree, t, m.tasks[tu].codeptr, g, ch[tu].clone(), None);
                continue;
            }
            for (codeptr, taskloop, sib) in fans {
                add(&mut groups, &mut pending, fan_kind(taskloop), t, codeptr, g, sib, None);
            }
            if !singles.is_empty() {
                forests.push((singles, false));
            }
        }
        shown.sort_unstable();
        groups[gu].tasks = shown;
    }

    let mut task_group = vec![NONE; n];
    for (g, grp) in groups.iter().enumerate() {
        for &i in &grp.tasks {
            task_group[i as usize] = g as u32;
        }
    }
    Grouping { max_group, groups, task_group }
}

impl Grouping {
    /// All tasks of a group, including those of its child groups, ascending.
    pub fn members(&self, g: usize) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.groups[g].n);
        let mut stack = vec![g];
        while let Some(x) = stack.pop() {
            out.extend_from_slice(&self.groups[x].tasks);
            stack.extend(self.groups[x].children.iter().map(|&c| c as usize));
        }
        out.sort_unstable();
        out
    }

    /// For every task the node that stands for it when the groups flagged in
    /// `expanded` are open: the task itself if all groups around it are
    /// expanded, else the representative of the outermost collapsed one.
    pub fn visible(&self, expanded: &[bool]) -> Vec<u32> {
        // Outermost collapsed group around each group (or the group itself);
        // parents precede their children.
        let mut hidden_in = vec![NONE; self.groups.len()];
        for (g, grp) in self.groups.iter().enumerate() {
            hidden_in[g] = match grp.parent {
                p if p != NONE && hidden_in[p as usize] != NONE => hidden_in[p as usize],
                _ if !expanded.get(g).copied().unwrap_or(false) => g as u32,
                _ => NONE,
            };
        }
        self.task_group
            .iter()
            .enumerate()
            .map(|(i, &g)| match g {
                NONE => i as u32,
                g => match hidden_in[g as usize] {
                    NONE => i as u32,
                    h => self.groups[h as usize].rep,
                },
            })
            .collect()
    }
}

/// Dependence edges between the visible nodes for a set of expanded groups:
/// (from, to, on the critical path), merged per pair of nodes.
pub fn visible_dependences(m: &Model, crit: &PathResult, grouping: &Grouping, expanded: &[bool]) -> Vec<(u32, u32, bool)> {
    let node = grouping.visible(expanded);
    let mut merged: std::collections::BTreeMap<(u32, u32), bool> = Default::default();
    for e in m.edges.iter().filter(|e| e.kind == crate::model::EdgeKind::Depend) {
        let (a, b) = (node[e.from as usize], node[e.to as usize]);
        if a != b {
            *merged.entry((a, b)).or_default() |= crit.transitions.contains(&(e.from, e.to)) || crit.transitions.contains(&(e.to, e.from));
        }
    }
    merged.into_iter().map(|((a, b), c)| (a, b, c)).collect()
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
    /// Critical path through these tasks alone (`work / own_critical` is
    /// their parallelism) and their peak concurrency. `None` in the group
    /// summaries of `summarize_groups`, which does not run a path analysis.
    pub parallelism: Option<(u64, u32)>,
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
        parallelism: Some((region.critical, region.max_concurrency)),
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

/// Sums over a set of tasks from which a `Summary` (without parallelism) is made.
#[derive(Default, Clone)]
struct Aggregate {
    n: usize,
    work: u64,
    critical: u64,
    min_work: u64,
    max_work: u64,
    t_begin: u64,
    t_end: u64,
    threads: std::collections::BTreeSet<u32>,
    create: u128,
    start: u128,
    /// Occurrences of thread, CPU, NUMA node, code location, region and kind.
    counts: [std::collections::HashMap<i64, usize>; 6],
}

impl Aggregate {
    fn add_task(&mut self, t: &crate::model::Task, critical: u64) {
        let seg = t.segs.first();
        let values = [task_thread(t), seg.map_or(-1, |s| s.cpu as i64), seg.map_or(-1, |s| s.numa as i64), t.codeptr as i64, t.parallel as i64, t.kind as i64];
        let mut one = Aggregate { n: 1, work: t.work, critical, min_work: t.work, max_work: t.work, t_begin: u64::MAX, create: t.create_ts as u128, start: task_start(t) as u128, ..Default::default() };
        for s in &t.segs {
            one.t_begin = one.t_begin.min(s.start);
            one.t_end = one.t_end.max(s.end);
            one.threads.insert(s.thread);
        }
        for (c, v) in one.counts.iter_mut().zip(values) {
            c.insert(v, 1);
        }
        self.merge(&one);
    }

    fn merge(&mut self, o: &Aggregate) {
        if o.n == 0 {
            return;
        }
        if self.n == 0 {
            self.min_work = u64::MAX;
            self.t_begin = u64::MAX;
        }
        self.n += o.n;
        self.work += o.work;
        self.critical += o.critical;
        self.min_work = self.min_work.min(o.min_work);
        self.max_work = self.max_work.max(o.max_work);
        self.t_begin = self.t_begin.min(o.t_begin);
        self.t_end = self.t_end.max(o.t_end);
        self.threads.extend(&o.threads);
        self.create += o.create;
        self.start += o.start;
        for (c, oc) in self.counts.iter_mut().zip(&o.counts) {
            for (&v, &k) in oc {
                *c.entry(v).or_default() += k;
            }
        }
    }

    fn summary(&self) -> Summary {
        // Most frequent value; ties go to the smaller one.
        let top = |c: &std::collections::HashMap<i64, usize>| c.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))).map_or(-1, |(&v, _)| v);
        let n = self.n.max(1) as u128;
        let ran = self.t_begin != u64::MAX && self.t_end > 0;
        Summary {
            n: self.n,
            work: self.work,
            critical: self.critical,
            min_work: if self.n == 0 { 0 } else { self.min_work },
            max_work: self.max_work,
            t_begin: if ran { self.t_begin } else { 0 },
            t_end: self.t_end,
            threads: self.threads.len(),
            parallelism: None,
            thread: top(&self.counts[0]),
            cpu: top(&self.counts[1]),
            numa: top(&self.counts[2]),
            codeptr: top(&self.counts[3]) as u64,
            parallel: top(&self.counts[4]) as u32,
            kind: top(&self.counts[5]) as u8,
            mean_work: (self.work as u128 / n) as u64,
            mean_create: (self.create / n) as u64,
            mean_start: (self.start / n) as u64,
        }
    }
}

/// Summaries of all groups in one pass over the tasks: each group's direct
/// tasks are added up, then the sums are passed to the enclosing groups.
/// The parallelism is left out (see `Summary::parallelism`).
pub fn summarize_groups(m: &Model, crit: &PathResult, grouping: &Grouping) -> Vec<Summary> {
    let mut agg: Vec<Aggregate> = vec![Aggregate::default(); grouping.groups.len()];
    // Child groups have larger indices than their parents.
    for g in (0..grouping.groups.len()).rev() {
        let grp = &grouping.groups[g];
        let mut a = std::mem::take(&mut agg[g]);
        for &i in &grp.tasks {
            a.add_task(&m.tasks[i as usize], crit.task_time[i as usize]);
        }
        if grp.parent != NONE {
            agg[grp.parent as usize].merge(&a);
        }
        agg[g] = a;
    }
    agg.iter().map(Aggregate::summary).collect()
}
