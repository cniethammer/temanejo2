//! Critical path and degree-of-parallelism analysis on the part graph.

use crate::model::{Model, TaskKind, NONE};
use std::collections::HashSet;

pub struct PathResult {
    /// Length of the longest (work-weighted) path in ns: T_inf.
    pub length: u64,
    /// Critical work time of the tasks on the path, `(task index, ns)`,
    /// ascending by task. Paths are short compared with the graph, so this
    /// is not stored per task.
    pub tasks: Vec<(u32, u64)>,
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

    // Number of included predecessors of each included node.
    let mut indeg = vec![0u32; n];
    for u in (0..n).filter(|&u| included(u)) {
        for &v in parts.succ(u) {
            indeg[v as usize] += 1;
        }
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
        for &v in parts.succ(u) {
            let v = v as usize;
            if selected.is_some() && !included(v) {
                continue;
            }
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
    drop((indeg, queue, dist));

    let mut tasks: Vec<(u32, u64)> = Vec::new();
    let mut transitions = HashSet::new();
    let mut cur = best.1;
    let mut later_task = NONE; // task of the nearest real part after `cur` on the path
    while cur != NONE {
        let t = parts.task[cur as usize];
        if t != NONE {
            tasks.push((t, parts.weight[cur as usize]));
            if later_task != NONE && later_task != t {
                transitions.insert((t, later_task));
            }
            later_task = t;
        }
        cur = pred[cur as usize];
    }
    // One entry per task: a task can be on the path with several parts.
    tasks.sort_unstable_by_key(|x| x.0);
    tasks.dedup_by(|b, a| {
        let same = a.0 == b.0;
        if same {
            a.1 += b.1;
        }
        same
    });
    tasks.retain(|x| x.1 > 0);
    PathResult { length: best.0, tasks, transitions }
}

/// The steps of a path from one task to another that follow no edge of the
/// display graph forwards: a child returning to the parent that waits for it
/// (taskwait, taskgroup, barrier, end of a parallel region) and the step
/// between two tasks of a team at a barrier. All other steps go from a
/// parent to the child it creates or along a dependence edge. Sorted.
pub fn joins(m: &Model, transitions: &HashSet<(u32, u32)>) -> Vec<(u32, u32)> {
    let deps: HashSet<(u32, u32)> = m.depends().filter(|e| transitions.contains(e)).collect();
    let mut out: Vec<(u32, u32)> = transitions.iter().copied().filter(|&(a, b)| m.tasks[b as usize].parent != a && !deps.contains(&(a, b))).collect();
    out.sort_unstable();
    out
}

impl PathResult {
    /// Critical work time of task `t` (0 if it is not on the path).
    pub fn time(&self, t: u32) -> u64 {
        self.tasks.binary_search_by_key(&t, |x| x.0).map_or(0, |k| self.tasks[k].1)
    }
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
        for s in m.segs(i) {
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

/// Largest number of tasks executing at the same time in the whole run: the
/// `max_concurrency` of `analyze_region` for all tasks, without its other results.
pub fn peak_concurrency(m: &Model) -> u32 {
    let segs = || m.all_segs().map(|(_, s)| s).filter(|s| s.end > s.start);
    let mut starts: Vec<u64> = segs().map(|s| s.start).collect();
    let mut ends: Vec<u64> = segs().map(|s| s.end).collect();
    starts.sort_unstable();
    ends.sort_unstable();
    // Segments ending at a time are over before those starting at it begin.
    let (mut ended, mut max) = (0, 0);
    for (started, &t) in starts.iter().enumerate() {
        while ends[ended] <= t {
            ended += 1;
        }
        max = max.max(started + 1 - ended);
    }
    max as u32
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
        o.critical += crit.time(i as u32);
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

/// Classes of a dependence edge, as a bit mask (`dependence_class`).
/// Flow: the predecessor writes what the successor reads (read after write);
/// anti: the successor overwrites what the predecessor read (write after
/// read); output: both write (write after write). `DEP_UNKNOWN`: the two
/// depend clauses share no address, e.g. an edge the runtime adds for
/// `inoutset` or `mutexinoutset` bookkeeping.
///
/// Clang compiles `out` like `inout`, so libomp reports `out` items as
/// inout; with it, an out -> out ordering is classified as flow.
pub const DEP_FLOW: u8 = 1;
pub const DEP_ANTI: u8 = 2;
pub const DEP_OUTPUT: u8 = 4;
pub const DEP_UNKNOWN: u8 = 8;

/// ompt dependence types (`ompt_dependence_type_t`) that write / read the
/// variable. mutexinoutset and inoutset count as inout here.
fn dep_writes(kind: u32) -> bool {
    matches!(kind, 2 | 3 | 4 | 7 | 34 | 35)
}
fn dep_reads(kind: u32) -> bool {
    matches!(kind, 1 | 3 | 4 | 7 | 35)
}
/// `omp_all_memory`: matches every address.
fn dep_all_memory(kind: u32) -> bool {
    matches!(kind, 34 | 35)
}

/// A variable that orders two tasks: its address, the class of the ordering
/// (one `DEP_*` bit) and the dependence types in the predecessor's and the
/// successor's depend clause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepVar {
    pub addr: u64,
    pub class: u8,
    pub pred_kind: u32,
    pub succ_kind: u32,
}

/// The variables by which task `from` precedes task `to`: the addresses in
/// both depend clauses, classified. An inout followed by an in or inout is a
/// flow dependence (the write is what matters).
pub fn dependence_vars(m: &Model, from: u32, to: u32) -> Vec<DepVar> {
    let (pd, sd) = (m.deps(from as usize), m.deps(to as usize));
    let mut out = Vec::new();
    for p in pd {
        for s in sd {
            if p.addr != s.addr && !dep_all_memory(p.kind) && !dep_all_memory(s.kind) {
                continue;
            }
            let class = if dep_writes(p.kind) && dep_reads(s.kind) {
                DEP_FLOW
            } else if dep_writes(p.kind) && dep_writes(s.kind) {
                DEP_OUTPUT
            } else if dep_reads(p.kind) && dep_writes(s.kind) {
                DEP_ANTI
            } else {
                continue;
            };
            // For omp_all_memory, name the concrete address of the other side.
            let addr = if dep_all_memory(p.kind) { s.addr } else { p.addr };
            out.push(DepVar { addr, class, pred_kind: p.kind, succ_kind: s.kind });
        }
    }
    out
}

/// The classes of the edge `from -> to` as a mask of `DEP_*` bits.
pub fn dependence_class(m: &Model, from: u32, to: u32) -> u8 {
    match dependence_vars(m, from, to).iter().fold(0, |c, v| c | v.class) {
        0 => DEP_UNKNOWN,
        c => c,
    }
}

/// Dependence edges between the visible nodes for a set of expanded groups:
/// (from, to, on the critical path, `DEP_*` classes), merged per pair of
/// nodes (the classes of merged edges are or-ed).
pub fn visible_dependences(m: &Model, crit: &PathResult, grouping: &Grouping, expanded: &[bool]) -> Vec<(u32, u32, bool, u8)> {
    let node = grouping.visible(expanded);
    let mut merged: std::collections::BTreeMap<(u32, u32), (bool, u8)> = Default::default();
    for (from, to) in m.depends() {
        let (a, b) = (node[from as usize], node[to as usize]);
        if a != b {
            let e = merged.entry((a, b)).or_default();
            e.0 |= crit.transitions.contains(&(from, to));
            e.1 |= dependence_class(m, from, to);
        }
    }
    merged.into_iter().map(|((a, b), (c, k))| (a, b, c, k)).collect()
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
pub fn task_thread(m: &Model, i: usize) -> i64 {
    m.segs(i).first().map_or(m.tasks[i].create_thread as i64, |s| s.thread as i64)
}

/// Start of a task: begin of its first segment, or its creation if it never ran.
pub fn task_start(m: &Model, i: usize) -> u64 {
    m.segs(i).first().map_or(m.tasks[i].create_ts, |s| s.start)
}

pub fn summarize(m: &Model, crit: &PathResult, members: &[u32]) -> Summary {
    let mut selected = vec![false; m.tasks.len()];
    for &i in members {
        selected[i as usize] = true;
    }
    let region = analyze_region(m, &selected);
    let tasks = || members.iter().map(|&i| &m.tasks[i as usize]);
    let first_seg = |i: &u32| m.segs(*i as usize).first();
    let n = members.len().max(1) as u64;
    Summary {
        n: members.len(),
        work: region.work,
        critical: members.iter().map(|&i| crit.time(i)).sum(),
        min_work: tasks().map(|t| t.work).min().unwrap_or(0),
        max_work: tasks().map(|t| t.work).max().unwrap_or(0),
        t_begin: region.t_begin,
        t_end: region.t_end,
        threads: region.per_thread.len(),
        parallelism: Some((region.critical, region.max_concurrency)),
        thread: most_frequent(members.iter().map(|&i| task_thread(m, i as usize))).unwrap_or(-1),
        cpu: most_frequent(members.iter().map(|i| first_seg(i).map_or(-1, |s| s.cpu as i64))).unwrap_or(-1),
        numa: most_frequent(members.iter().map(|i| first_seg(i).map_or(-1, |s| s.numa as i64))).unwrap_or(-1),
        codeptr: most_frequent(tasks().map(|t| t.codeptr)).unwrap_or(0),
        parallel: most_frequent(tasks().map(|t| t.parallel)).unwrap_or(NONE),
        kind: most_frequent(tasks().map(|t| t.kind as u8)).unwrap_or(2),
        mean_work: region.work / n,
        mean_create: tasks().map(|t| t.create_ts).sum::<u64>() / n,
        mean_start: members.iter().map(|&i| task_start(m, i as usize)).sum::<u64>() / n,
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
    fn add_task(&mut self, m: &Model, i: usize, critical: u64) {
        let (t, segs) = (&m.tasks[i], m.segs(i));
        let seg = segs.first();
        let values = [task_thread(m, i), seg.map_or(-1, |s| s.cpu as i64), seg.map_or(-1, |s| s.numa as i64), t.codeptr as i64, t.parallel as i64, t.kind as i64];
        let mut one = Aggregate { n: 1, work: t.work, critical, min_work: t.work, max_work: t.work, t_begin: u64::MAX, create: t.create_ts as u128, start: task_start(m, i) as u128, ..Default::default() };
        for s in segs {
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
            a.add_task(m, i as usize, crit.time(i));
        }
        if grp.parent != NONE {
            agg[grp.parent as usize].merge(&a);
        }
        agg[g] = a;
    }
    agg.iter().map(Aggregate::summary).collect()
}

// ------------------------------------------------------------------ critical path gaps

/// What happens at a moment of the run with respect to the critical path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GapKind {
    /// A task with work on the critical path is executing.
    OnPath = 0,
    /// No critical task executes, and every available thread executes some
    /// other task: the run is limited by the amount of work.
    Busy = 1,
    /// No critical task executes, a thread is idle, and a critical task is
    /// ready (created, all dependence predecessors finished) but not started:
    /// a scheduling problem.
    Waiting = 2,
    /// No critical task executes, a thread is idle, and no critical task is
    /// ready: e.g. runtime overhead, or time spent in no task at all.
    Idle = 3,
}

/// Consecutive intervals covering the whole run, each of another kind than
/// the one before. A run has millions of them, so only the begin and kind
/// of each are stored; an interval ends where the next one begins.
pub struct CriticalGaps {
    begin: Vec<u64>,
    kind: Vec<GapKind>,
    /// End of the last interval.
    end: u64,
    /// Total time per kind, indexed by `GapKind as usize`.
    pub totals: [u64; 4],
}

impl CriticalGaps {
    pub fn len(&self) -> usize {
        self.begin.len()
    }

    /// Interval `i` as `(begin, end, kind)`.
    pub fn interval(&self, i: usize) -> (u64, u64, GapKind) {
        (self.begin[i], self.begin.get(i + 1).copied().unwrap_or(self.end), self.kind[i])
    }

    #[cfg(test)]
    pub fn intervals(&self) -> impl Iterator<Item = (u64, u64, GapKind)> + '_ {
        (0..self.len()).map(|i| self.interval(i))
    }

    /// Index range of the intervals that overlap the time window `(t0, t1)`.
    pub fn overlapping(&self, t0: f64, t1: f64) -> std::ops::Range<usize> {
        // Interval `i` ends at or before `t0` if the next one begins there.
        let a = self.begin.get(1..).map_or(0, |next| next.partition_point(|&b| (b as f64) <= t0));
        let a = if self.end as f64 <= t0 { self.len() } else { a };
        a..self.begin.partition_point(|&b| (b as f64) < t1).max(a)
    }
}

/// Classifies every moment of the run (0 to `m.wall`) by `GapKind`.
///
/// A thread is available from the start of its first to the end of its last
/// execution segment. A critical task is waiting from the moment it is
/// ready, the later of its creation and the completion of its dependence
/// predecessors, until its first segment starts.
pub fn critical_gaps(m: &Model, crit: &PathResult) -> CriticalGaps {
    // Changes of four counters: critical tasks running, tasks running,
    // threads available, critical tasks waiting. Packed into one word,
    // `time << 3 | counter << 1 | increment`, as there are several per segment.
    let mut changes: Vec<u64> = Vec::new();
    let change = |ts: u64, counter: u64, up: bool| ts << 3 | counter << 1 | up as u64;
    let mut ready = vec![0u64; m.tasks.len()];
    for (from, to) in m.depends() {
        let r = &mut ready[to as usize];
        *r = (*r).max(m.tasks[from as usize].end);
    }
    let mut span: std::collections::HashMap<u32, (u64, u64)> = Default::default();
    for (i, t) in m.tasks.iter().enumerate() {
        let critical = crit.time(i as u32) > 0;
        for s in m.segs(i).iter().filter(|s| s.end > s.start) {
            changes.push(change(s.start, 1, true));
            changes.push(change(s.end, 1, false));
            if critical {
                changes.push(change(s.start, 0, true));
                changes.push(change(s.end, 0, false));
            }
            let sp = span.entry(s.thread).or_insert((s.start, s.end));
            sp.0 = sp.0.min(s.start);
            sp.1 = sp.1.max(s.end);
        }
        if critical {
            if let Some(first) = m.segs(i).first() {
                let from = ready[i].max(t.create_ts);
                if from < first.start {
                    changes.push(change(from, 3, true));
                    changes.push(change(first.start, 3, false));
                }
            }
        }
    }
    for &(a, b) in span.values() {
        changes.push(change(a, 2, true));
        changes.push(change(b, 2, false));
    }
    // Only the time orders: all changes up to a time are applied together.
    changes.sort_unstable();

    let (mut begin, mut kinds): (Vec<u64>, Vec<GapKind>) = Default::default();
    let mut totals = [0u64; 4];
    let mut count = [0i32; 4];
    // Intervals are pushed in order without holes; one of the same kind as
    // the last extends it.
    let mut push = |a: u64, b: u64, kind: GapKind| {
        if b <= a {
            return;
        }
        totals[kind as usize] += b - a;
        if kinds.last() != Some(&kind) {
            begin.push(a);
            kinds.push(kind);
        }
    };
    let mut t = 0u64;
    let mut k = 0;
    while t < m.wall {
        while k < changes.len() && changes[k] >> 3 <= t {
            count[(changes[k] >> 1 & 3) as usize] += if changes[k] & 1 == 1 { 1 } else { -1 };
            k += 1;
        }
        let next = changes.get(k).map_or(m.wall, |c| (c >> 3).min(m.wall));
        let kind = if count[0] > 0 {
            GapKind::OnPath
        } else if count[2] > 0 && count[1] >= count[2] {
            GapKind::Busy
        } else if count[3] > 0 {
            GapKind::Waiting
        } else {
            GapKind::Idle
        };
        push(t, next, kind);
        t = next;
    }
    begin.shrink_to_fit();
    kinds.shrink_to_fit();
    CriticalGaps { begin, kind: kinds, end: m.wall, totals }
}
