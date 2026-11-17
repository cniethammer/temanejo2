//! Reconstructs the task graph of one process from its event trace.
//!
//! Two graphs are built:
//!
//! * the **display graph**: one node per task, edges for task creation and
//!   for `depend` clause dependences. This is what the GUI draws.
//! * the **part graph**: every task is cut into *parts* at the points where
//!   it creates a child, forks/joins a parallel region or passes a
//!   synchronisation point (taskwait, taskgroup end, barrier). Parts carry
//!   the measured exclusive work time and are connected by all ordering
//!   constraints, including the joins child -> waiting parent. The part
//!   graph is acyclic and is what critical path and parallelism are
//!   computed on (see `analysis.rs`).
//!
//! The model is built by feeding the events in time order to a
//! `ModelBuilder`, so a trace need not be in memory. Everything kept per
//! task, segment or part is stored in flat arrays: a trace has millions of
//! them, and the model stays in memory while the server runs.

use std::collections::HashMap;
use tmj_trace::{kind, Event, Trace, ENDPOINT_BEGIN};

pub const NONE: u32 = u32::MAX;

// ompt_task_flag_t
pub const TASK_INITIAL: u32 = 0x1;
// ompt_task_status_t
const STATUS_COMPLETE: u64 = 1;
const STATUS_CANCEL: u64 = 3;
const STATUS_DETACH: u64 = 4;
const STATUS_EARLY_FULFILL: u64 = 5;
const STATUS_LATE_FULFILL: u64 = 6;
// ompt_sync_region_t
const SYNC_TASKWAIT: u64 = 5;
const SYNC_TASKGROUP: u64 = 6;
const SYNC_REDUCTION: u64 = 7;
const SYNC_BARRIER_IMPLICIT_PARALLEL: u64 = 9;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Initial = 0,
    Implicit = 1,
    Explicit = 2,
}

/// A stretch of uninterrupted work of one task on one thread.
#[derive(Clone, Copy)]
pub struct Seg {
    pub thread: u32,
    pub cpu: u16,
    /// CPU at the end of the segment; differs from `cpu` if the OS migrated the thread.
    pub cpu_end: u16,
    pub numa: u16,
    pub start: u64,
    pub end: u64,
}

/// A task. Its execution segments and `depend` clause items are
/// `Model::segs` and `Model::deps`.
pub struct Task {
    pub id: u64,
    pub create_ts: u64,
    pub codeptr: u64,
    pub end: u64,
    /// Exclusive work time: time executing, without time suspended or waiting.
    pub work: u64,
    /// Index of the creating (or, for implicit tasks, encountering) task.
    pub parent: u32,
    /// Index into `Model::parallels` of the region the task belongs to.
    pub parallel: u32,
    /// Thread number within the team (implicit tasks only).
    pub team_index: u32,
    pub create_thread: u32,
    pub flags: u32,
    pub kind: TaskKind,
    /// Created by a taskloop construct (the tool replaced the runtime-internal
    /// code address by the construct's address).
    pub taskloop: bool,
    pub complete: bool,
}

/// An item of a task's `depend` clause.
#[derive(Clone, Copy)]
pub struct DepItem {
    task: u32,
    /// ompt dependence type.
    pub kind: u32,
    /// Address of the variable.
    pub addr: u64,
}

/// What is known about a task only while the graph is reconstructed.
struct TaskState {
    /// Start of the segment the task is executing (if `running`), and where.
    open_start: u64,
    open_thread: u32,
    open_cpu: u16,
    open_numa: u16,
    running: bool,
    waiting: bool,
    done: bool,
    /// Work of the task up to the last cut into parts.
    last_mark: u64,
    first_part: u32,
    last_part: u32,
    /// Innermost taskgroup the task was created in.
    in_group: u32,
    barriers: u32,
}

pub struct Parallel {
    pub id: u64,
    pub parent_task: u32,
    pub requested: u32,
    pub actual: u32,
    pub codeptr: u64,
    pub begin: u64,
    pub end: u64,
    fork_part: u32,
    end_node: u32,
    barrier_nodes: Vec<u32>,
    barrier_released: Vec<bool>,
    unjoined: Vec<u32>,
}

pub struct ThreadInfo {
    pub id: u32,
    pub os_tid: u64,
    pub thread_type: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Create = 0,
    Depend = 1,
}

pub struct Edge {
    pub from: u32,
    pub to: u32,
    pub kind: EdgeKind,
}

/// A dependence edge between two tasks.
struct Depend {
    from: u32,
    to: u32,
    /// Number of tasks that existed when the dependence was recorded; gives
    /// its place among the creation edges in `Model::edges`.
    tasks_before: u32,
}

/// The acyclic graph of task parts used for path analysis.
#[derive(Default)]
pub struct Parts {
    /// Exclusive work time of each part in ns (0 for virtual join nodes).
    pub weight: Vec<u64>,
    /// Owning task index, or `NONE` for virtual join nodes.
    pub task: Vec<u32>,
    /// Successors of part `p`: `succ[start[p]..start[p + 1]]`.
    start: Vec<u32>,
    succ: Vec<u32>,
}

impl Parts {
    fn node(&mut self, task: u32) -> u32 {
        self.weight.push(0);
        self.task.push(task);
        (self.weight.len() - 1) as u32
    }

    /// The parts that can only start after part `p`.
    pub fn succ(&self, p: usize) -> &[u32] {
        &self.succ[self.start[p] as usize..self.start[p + 1] as usize]
    }

    /// Stores the edges as successor lists, each in the order of `edges`.
    fn set_edges(&mut self, edges: Vec<(u32, u32)>) {
        let n = self.weight.len();
        let mut start = vec![0u32; n + 1];
        for &(a, _) in &edges {
            start[a as usize + 1] += 1;
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let mut succ = vec![0u32; edges.len()];
        // `start[p]` is moved to the end of the list of `p` while filling ...
        for &(a, b) in &edges {
            succ[start[a as usize] as usize] = b;
            start[a as usize] += 1;
        }
        // ... which is the start of the next list: shift back by one.
        start.copy_within(0..n, 1);
        start[0] = 0;
        self.start = start;
        self.succ = succ;
    }
}

pub struct Model {
    pub rank: u32,
    pub pid: u32,
    pub host: String,
    /// Optional information about the run from the trace header.
    pub info: Vec<(String, String)>,
    /// Duration covered by the trace in ns; all timestamps are relative to its start.
    pub wall: u64,
    pub tasks: Vec<Task>,
    pub parallels: Vec<Parallel>,
    pub threads: Vec<ThreadInfo>,
    pub parts: Parts,
    pub n_events: usize,
    /// Execution segments of task `i`: `segs[seg_start[i]..seg_start[i + 1]]`.
    segs: Vec<Seg>,
    seg_start: Vec<u32>,
    /// `depend` clause items, ordered by task.
    dep_items: Vec<DepItem>,
    /// Dependence edges in the order they were recorded. Creation edges are
    /// not stored: they are the `parent` of each task.
    depends: Vec<Depend>,
}

struct ThreadState {
    current: u32,
    stack: Vec<u32>,
}

/// The index of each task by its id in the trace. The tool numbers tasks
/// consecutively, so the ids index a table; ids far beyond the number of
/// tasks seen (a trace from elsewhere) go to a hash map instead.
#[derive(Default)]
struct IdMap {
    table: Vec<u32>,
    other: HashMap<u64, u32>,
    len: usize,
}

impl IdMap {
    fn get(&self, id: u64) -> Option<u32> {
        match self.table.get(id as usize) {
            Some(&t) if id <= usize::MAX as u64 && t != NONE => Some(t),
            _ if self.other.is_empty() => None,
            _ => self.other.get(&id).copied(),
        }
    }

    fn insert(&mut self, id: u64, task: u32) {
        self.len += 1;
        if id < (2 * self.len + 4096) as u64 {
            if self.table.len() <= id as usize {
                self.table.resize(id as usize + 1, NONE);
            }
            self.table[id as usize] = task;
        } else {
            self.other.insert(id, task);
        }
    }
}

/// Reconstructs a `Model` from the events of a trace, passed one at a time
/// in time order (`push`).
#[derive(Default)]
pub struct ModelBuilder {
    /// Time of the first event; all times of the model are relative to it.
    t0: Option<u64>,
    wall: u64,
    n_events: usize,
    tasks: Vec<Task>,
    /// Reconstruction state of each task.
    st: Vec<TaskState>,
    task_idx: IdMap,
    parallels: Vec<Parallel>,
    par_idx: HashMap<u64, u32>,
    threads: Vec<ThreadInfo>,
    tstate: HashMap<u32, ThreadState>,
    /// Segments in the order they ended, and the task of each.
    segs: Vec<Seg>,
    seg_task: Vec<u32>,
    dep_items: Vec<DepItem>,
    depends: Vec<Depend>,
    parts: Parts,
    part_edges: Vec<(u32, u32)>,
    /// Children a task created and has not waited for; dropped when it completes.
    pending_children: HashMap<u32, Vec<u32>>,
    /// Taskgroups a task has open, innermost last (indices into `groups`).
    open_groups: HashMap<u32, Vec<u32>>,
    groups: Vec<Vec<u32>>,
}

impl ModelBuilder {
    pub fn new() -> ModelBuilder {
        ModelBuilder::default()
    }

    /// Processes the next event of the trace. Events must be passed in time
    /// order; events with the same time in the order of the trace file.
    pub fn push(&mut self, e: &Event) {
        let t0 = *self.t0.get_or_insert(e.ts);
        let e = Event { ts: e.ts.saturating_sub(t0), ..*e };
        self.wall = self.wall.max(e.ts);
        self.n_events += 1;
        self.event(&e);
    }

    /// Completes the model; `head` is the trace the events came from (its
    /// events are not used).
    pub fn finish(mut self, head: &Trace) -> Model {
        self.close(self.wall);
        self.threads.sort_by_key(|t| t.id);
        // The reconstruction state is large; free it before the final arrays are made.
        drop(std::mem::take(&mut self.st));
        drop(std::mem::take(&mut self.task_idx));

        // Segments and `depend` items, grouped by task in their order of recording.
        let n = self.tasks.len();
        let mut seg_start = vec![0u32; n + 1];
        for &t in &self.seg_task {
            seg_start[t as usize + 1] += 1;
        }
        for i in 0..n {
            seg_start[i + 1] += seg_start[i];
        }
        // The place of each segment in the final order, then the segments
        // are moved there in place (cycle by cycle) instead of being copied.
        let mut next = seg_start.clone();
        let mut dest = std::mem::take(&mut self.seg_task);
        for t in dest.iter_mut() {
            let d = next[*t as usize];
            next[*t as usize] += 1;
            *t = d;
        }
        drop(next);
        let mut segs = std::mem::take(&mut self.segs);
        for k in 0..segs.len() {
            while dest[k] as usize != k {
                let d = dest[k] as usize;
                segs.swap(k, d);
                dest.swap(k, d);
            }
        }
        drop(dest);
        segs.shrink_to_fit();
        self.dep_items.sort_by_key(|d| d.task);
        self.dep_items.shrink_to_fit();
        self.depends.shrink_to_fit();
        self.tasks.shrink_to_fit();
        self.parts.weight.shrink_to_fit();
        self.parts.task.shrink_to_fit();
        self.parts.set_edges(std::mem::take(&mut self.part_edges));

        Model {
            rank: head.rank,
            pid: head.pid,
            host: head.host.clone(),
            info: {
                // The runtime library is a header field of its own; show it with the rest.
                let mut info = head.info.clone();
                if !head.runtime_module.is_empty() {
                    let at = info.iter().position(|(k, _)| !k.starts_with("omp.") && !k.starts_with("tool.")).unwrap_or(info.len());
                    info.insert(at, ("omp.runtime_library".to_string(), head.runtime_module.clone()));
                }
                info
            },
            wall: self.wall,
            tasks: self.tasks,
            parallels: self.parallels,
            threads: self.threads,
            parts: self.parts,
            n_events: self.n_events,
            segs,
            seg_start,
            dep_items: self.dep_items,
            depends: self.depends,
        }
    }

    fn task(&self, id: u64) -> Option<u32> {
        self.task_idx.get(id)
    }

    fn parallel(&mut self, id: u64, ts: u64) -> u32 {
        if id == 0 {
            return NONE;
        }
        if let Some(&p) = self.par_idx.get(&id) {
            return p;
        }
        let end_node = self.parts.node(NONE);
        self.parallels.push(Parallel {
            id,
            parent_task: NONE,
            requested: 0,
            actual: 0,
            codeptr: 0,
            begin: ts,
            end: 0,
            fork_part: NONE,
            end_node,
            barrier_nodes: vec![],
            barrier_released: vec![],
            unjoined: vec![],
        });
        let p = (self.parallels.len() - 1) as u32;
        self.par_idx.insert(id, p);
        p
    }

    fn new_task(&mut self, id: u64, kind: TaskKind, flags: u32, parent: u32, parallel: u32, e: &Event) -> u32 {
        let t = self.tasks.len() as u32;
        let part = self.parts.node(t);
        self.tasks.push(Task {
            id,
            create_ts: e.ts,
            codeptr: 0,
            end: 0,
            work: 0,
            parent,
            parallel,
            team_index: 0,
            create_thread: e.thread,
            flags,
            kind,
            taskloop: false,
            complete: false,
        });
        self.st.push(TaskState {
            open_start: 0,
            open_thread: 0,
            open_cpu: 0,
            open_numa: 0,
            running: false,
            waiting: false,
            done: false,
            last_mark: 0,
            first_part: part,
            last_part: part,
            in_group: NONE,
            barriers: 0,
        });
        self.task_idx.insert(id, t);
        t
    }

    fn open_seg(&mut self, t: u32, e: &Event) {
        let st = &mut self.st[t as usize];
        if !st.running && !st.waiting && !st.done {
            st.running = true;
            (st.open_start, st.open_thread, st.open_cpu, st.open_numa) = (e.ts, e.thread, e.cpu, e.numa);
        }
    }

    fn close_seg(&mut self, t: u32, ts: u64, cpu: u16) {
        let (task, st) = (&mut self.tasks[t as usize], &mut self.st[t as usize]);
        if st.running {
            st.running = false;
            let (start, end) = (st.open_start, ts.max(st.open_start));
            task.work += end - start;
            task.end = task.end.max(end);
            self.segs.push(Seg { thread: st.open_thread, cpu: st.open_cpu, cpu_end: cpu, numa: st.open_numa, start, end });
            self.seg_task.push(t);
        }
    }

    /// Work accumulated by task `t` up to time `ts`.
    fn work_until(&self, t: u32, ts: u64) -> u64 {
        let st = &self.st[t as usize];
        self.tasks[t as usize].work + if st.running { ts.saturating_sub(st.open_start) } else { 0 }
    }

    /// Ends the current part of `t` at `ts`, assigning it the work done since the last cut.
    fn end_part(&mut self, t: u32, ts: u64) {
        let w = self.work_until(t, ts);
        let st = &mut self.st[t as usize];
        self.parts.weight[st.last_part as usize] += w - st.last_mark;
        st.last_mark = w;
    }

    /// Cuts task `t` at `ts`; returns (part before, part after).
    fn split(&mut self, t: u32, ts: u64) -> (u32, u32) {
        self.end_part(t, ts);
        let old = self.st[t as usize].last_part;
        let new = self.parts.node(t);
        self.part_edges.push((old, new));
        self.st[t as usize].last_part = new;
        (old, new)
    }

    /// Marks task `t` as finished; what was kept for its further execution is dropped.
    fn complete(&mut self, t: u32) {
        self.tasks[t as usize].complete = true;
        self.st[t as usize].done = true;
        self.pending_children.remove(&t);
        self.open_groups.remove(&t);
    }

    fn event(&mut self, e: &Event) {
        match e.kind {
            kind::THREAD_BEGIN => {
                self.threads.push(ThreadInfo { id: e.thread, os_tid: e.b, thread_type: e.a as u32 });
            }
            kind::PARALLEL_BEGIN => {
                let p = self.parallel(e.a, e.ts);
                let enc = self.task(e.b);
                let fork = enc.map(|t| self.split(t, e.ts).0);
                let par = &mut self.parallels[p as usize];
                par.parent_task = enc.unwrap_or(NONE);
                par.requested = e.c as u32;
                par.codeptr = e.d;
                par.begin = e.ts;
                par.fork_part = fork.unwrap_or(NONE);
            }
            kind::PARALLEL_END => {
                let Some(&p) = self.par_idx.get(&e.a) else { return };
                self.parallels[p as usize].end = e.ts;
                if let Some(t) = self.task(e.b) {
                    let (_, cont) = self.split(t, e.ts);
                    self.part_edges.push((self.parallels[p as usize].end_node, cont));
                }
            }
            kind::IMPLICIT_TASK if e.endpoint == ENDPOINT_BEGIN => {
                let p = self.parallel(e.b, e.ts);
                let initial = e.flags & TASK_INITIAL != 0;
                let (parent, fork, codeptr) = match self.parallels.get(p as usize) {
                    Some(par) if !initial => (par.parent_task, par.fork_part, par.codeptr),
                    _ => (NONE, NONE, 0),
                };
                let t = match self.task(e.a) {
                    Some(t) => t, // initial task already announced through task_create
                    None => {
                        let k = if initial { TaskKind::Initial } else { TaskKind::Implicit };
                        self.new_task(e.a, k, e.flags, parent, p, e)
                    }
                };
                self.tasks[t as usize].team_index = e.c as u32;
                self.tasks[t as usize].codeptr = codeptr;
                if let Some(par) = self.parallels.get_mut(p as usize) {
                    par.actual = par.actual.max(e.d as u32);
                }
                if fork != NONE {
                    self.part_edges.push((fork, self.st[t as usize].first_part));
                }
                // The task that was running on this thread (the encountering
                // task on the primary thread) is suspended for the region.
                let prev = self.tstate.entry(e.thread).or_insert_with(|| ThreadState { current: NONE, stack: vec![] }).current;
                if prev != NONE {
                    self.close_seg(prev, e.ts, e.cpu);
                }
                let st = self.tstate.get_mut(&e.thread).unwrap();
                st.stack.push(prev);
                st.current = t;
                self.open_seg(t, e);
            }
            kind::IMPLICIT_TASK => {
                let Some(t) = self.task(e.a) else { return };
                self.close_seg(t, e.ts, e.cpu);
                self.end_part(t, e.ts);
                self.complete(t);
                let p = self.tasks[t as usize].parallel;
                if let Some(par) = self.parallels.get(p as usize) {
                    self.part_edges.push((self.st[t as usize].last_part, par.end_node));
                }
                let st = self.tstate.entry(e.thread).or_default();
                let prev = st.stack.pop().unwrap_or(NONE);
                st.current = prev;
                if prev != NONE {
                    self.open_seg(prev, e);
                }
            }
            kind::TASK_CREATE => {
                if self.task(e.a).is_some() {
                    return;
                }
                if e.flags & TASK_INITIAL != 0 {
                    self.new_task(e.a, TaskKind::Initial, e.flags, NONE, NONE, e);
                    return;
                }
                let enc = self.task(e.b);
                let (parent, parallel) = match enc {
                    Some(p) => (p, self.tasks[p as usize].parallel),
                    None => (NONE, NONE),
                };
                let t = self.new_task(e.a, TaskKind::Explicit, e.flags, parent, parallel, e);
                self.tasks[t as usize].codeptr = e.d;
                self.tasks[t as usize].taskloop = e.c != 0;
                if let Some(p) = enc {
                    let (before, _) = self.split(p, e.ts);
                    self.part_edges.push((before, self.st[t as usize].first_part));
                    self.pending_children.entry(p).or_default().push(t);
                    let g = self.open_groups.get(&p).and_then(|open| open.last().copied()).unwrap_or(self.st[p as usize].in_group);
                    if g != NONE {
                        self.st[t as usize].in_group = g;
                        self.groups[g as usize].push(t);
                    }
                }
                if let Some(par) = self.parallels.get_mut(parallel as usize) {
                    par.unjoined.push(t);
                }
            }
            kind::TASK_SCHEDULE => {
                if e.b == STATUS_EARLY_FULFILL || e.b == STATUS_LATE_FULFILL {
                    return; // no task switch happens
                }
                if let Some(prior) = self.task(e.a) {
                    self.close_seg(prior, e.ts, e.cpu);
                    if matches!(e.b, STATUS_COMPLETE | STATUS_CANCEL | STATUS_DETACH) {
                        self.end_part(prior, e.ts);
                        self.complete(prior);
                        let task = &mut self.tasks[prior as usize];
                        task.end = task.end.max(e.ts);
                    }
                }
                let next = self.task(e.c).unwrap_or(NONE);
                self.tstate.entry(e.thread).or_default().current = next;
                if next != NONE {
                    self.open_seg(next, e);
                }
            }
            kind::DEPENDENCE_VAR => {
                if let Some(t) = self.task(e.a) {
                    self.dep_items.push(DepItem { task: t, kind: e.c as u32, addr: e.b });
                }
            }
            kind::TASK_DEPENDENCE => {
                if let (Some(a), Some(b)) = (self.task(e.a), self.task(e.b)) {
                    self.depends.push(Depend { from: a, to: b, tasks_before: self.tasks.len() as u32 });
                }
            }
            kind::SYNC_WAIT => {
                let Some(t) = self.task(e.b) else { return };
                if e.endpoint == ENDPOINT_BEGIN {
                    self.close_seg(t, e.ts, e.cpu);
                    self.st[t as usize].waiting = true;
                } else {
                    self.st[t as usize].waiting = false;
                    if self.tstate.get(&e.thread).map(|s| s.current) == Some(t) {
                        self.open_seg(t, e);
                    }
                }
            }
            kind::SYNC_REGION => self.sync_region(e),
            _ => {}
        }
    }

    fn sync_region(&mut self, e: &Event) {
        let Some(t) = self.task(e.b) else { return };
        let begin = e.endpoint == ENDPOINT_BEGIN;
        match e.a {
            SYNC_TASKWAIT => {
                if !begin {
                    let (_, after) = self.split(t, e.ts);
                    for c in self.pending_children.remove(&t).unwrap_or_default() {
                        self.part_edges.push((self.st[c as usize].last_part, after));
                    }
                }
            }
            SYNC_TASKGROUP => {
                if begin {
                    self.groups.push(vec![]);
                    self.open_groups.entry(t).or_default().push((self.groups.len() - 1) as u32);
                } else if let Some(g) = self.open_groups.get_mut(&t).and_then(|open| open.pop()) {
                    let (_, after) = self.split(t, e.ts);
                    for c in std::mem::take(&mut self.groups[g as usize]) {
                        self.part_edges.push((self.st[c as usize].last_part, after));
                    }
                }
            }
            SYNC_REDUCTION => {}
            _ => {
                // Barriers. The k-th barrier of every implicit task of a team
                // is the same barrier; a virtual node joins the team there.
                let p = self.tasks[t as usize].parallel;
                if p == NONE {
                    return;
                }
                if begin {
                    let k = self.st[t as usize].barriers as usize;
                    self.st[t as usize].barriers += 1;
                    while self.parallels[p as usize].barrier_nodes.len() <= k {
                        let n = self.parts.node(NONE);
                        self.parallels[p as usize].barrier_nodes.push(n);
                        self.parallels[p as usize].barrier_released.push(false);
                    }
                    let join = self.parallels[p as usize].barrier_nodes[k];
                    let (before, after) = self.split(t, e.ts);
                    self.part_edges.push((before, join));
                    self.part_edges.push((join, after));
                    if e.a == SYNC_BARRIER_IMPLICIT_PARALLEL {
                        // libomp reports the end of the region-final barrier of
                        // worker threads only when they are reused; nothing
                        // executes in the implicit task after this point.
                        self.close_seg(t, e.ts, e.cpu);
                        self.st[t as usize].done = true;
                    }
                } else {
                    let k = (self.st[t as usize].barriers as usize).wrapping_sub(1);
                    let par = &mut self.parallels[p as usize];
                    if k < par.barrier_nodes.len() && !par.barrier_released[k] {
                        // First thread leaving the barrier: every explicit task
                        // of the region that has completed was awaited by it.
                        par.barrier_released[k] = true;
                        let join = par.barrier_nodes[k];
                        let (done, rest): (Vec<u32>, Vec<u32>) =
                            std::mem::take(&mut par.unjoined).into_iter().partition(|&c| self.tasks[c as usize].complete);
                        self.parallels[p as usize].unjoined = rest;
                        for c in done {
                            self.part_edges.push((self.st[c as usize].last_part, join));
                        }
                    }
                }
            }
        }
    }

    /// Ends what is still open at the end of the trace, `t_end`.
    fn close(&mut self, t_end: u64) {
        for t in 0..self.tasks.len() as u32 {
            if self.st[t as usize].running {
                let cpu = self.st[t as usize].open_cpu;
                self.close_seg(t, t_end, cpu);
            }
            if !self.tasks[t as usize].complete {
                self.end_part(t, t_end);
            }
        }
        for d in &self.depends {
            self.part_edges.push((self.st[d.from as usize].last_part, self.st[d.to as usize].first_part));
        }
        // Whatever was not joined at a barrier is joined by the end of its region.
        for par in &mut self.parallels {
            for c in std::mem::take(&mut par.unjoined) {
                self.part_edges.push((self.st[c as usize].last_part, par.end_node));
            }
        }
    }
}

impl Model {
    /// Builds the model of a trace held in memory.
    #[cfg(test)]
    pub fn build(trace: &Trace) -> Model {
        let mut events = trace.events.clone();
        // Per-thread streams are already ordered; a stable sort merges them.
        events.sort_by_key(|e| e.ts);
        let mut b = ModelBuilder::new();
        for e in &events {
            b.push(e);
        }
        b.finish(trace)
    }

    pub fn total_work(&self) -> u64 {
        self.tasks.iter().map(|t| t.work).sum()
    }

    /// Execution segments of task `i`, in time order.
    pub fn segs(&self, i: usize) -> &[Seg] {
        &self.segs[self.seg_start[i] as usize..self.seg_start[i + 1] as usize]
    }

    /// Number of execution segments of all tasks.
    pub fn seg_count(&self) -> usize {
        self.segs.len()
    }

    /// Execution segment `k` of all (`all_segs` order).
    pub fn seg(&self, k: u32) -> &Seg {
        &self.segs[k as usize]
    }

    /// The task of execution segment `k`.
    pub fn seg_task(&self, k: u32) -> u32 {
        (self.seg_start.partition_point(|&s| s <= k) - 1) as u32
    }

    /// Execution segments of all tasks with the index of their task, ordered by task.
    pub fn all_segs(&self) -> impl Iterator<Item = (u32, &Seg)> {
        (0..self.tasks.len()).flat_map(move |i| self.segs(i).iter().map(move |s| (i as u32, s)))
    }

    /// `depend` clause items of task `i`.
    pub fn deps(&self, i: usize) -> &[DepItem] {
        let a = self.dep_items.partition_point(|d| (d.task as usize) < i);
        let n = self.dep_items[a..].partition_point(|d| d.task as usize == i);
        &self.dep_items[a..a + n]
    }

    /// Dependence edges `(from, to)` in the order they were recorded.
    pub fn depends(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.depends.iter().map(|d| (d.from, d.to))
    }

    /// All edges of the display graph, creation edges (parent to task) and
    /// dependence edges, in the order they came about.
    pub fn edges(&self) -> impl Iterator<Item = Edge> + '_ {
        let create = move |range: std::ops::Range<u32>| {
            range.filter(move |&t| self.tasks[t as usize].parent != NONE).map(move |t| Edge { from: self.tasks[t as usize].parent, to: t, kind: EdgeKind::Create })
        };
        let n = self.tasks.len() as u32;
        let mut done = 0;
        self.depends
            .iter()
            .map(Some)
            .chain(std::iter::once(None))
            .flat_map(move |d| {
                // The tasks created since the previous dependence, then the dependence.
                let upto = d.map_or(n, |d| d.tasks_before);
                let range = done..upto;
                done = upto;
                create(range).chain(d.map(|d| Edge { from: d.from, to: d.to, kind: EdgeKind::Depend }))
            })
    }
}

impl Default for ThreadState {
    fn default() -> Self {
        ThreadState { current: NONE, stack: vec![] }
    }
}
