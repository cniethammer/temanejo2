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

pub struct Task {
    pub id: u64,
    pub kind: TaskKind,
    pub flags: u32,
    /// Index of the creating (or, for implicit tasks, encountering) task.
    pub parent: u32,
    /// Index into `Model::parallels` of the region the task belongs to.
    pub parallel: u32,
    /// Thread number within the team (implicit tasks only).
    pub team_index: u32,
    pub create_ts: u64,
    pub create_thread: u32,
    pub codeptr: u64,
    /// Created by a taskloop construct (the tool replaced the runtime-internal
    /// code address by the construct's address).
    pub taskloop: bool,
    pub end: u64,
    /// Exclusive work time: time executing, without time suspended or waiting.
    pub work: u64,
    pub segs: Vec<Seg>,
    /// `depend` clause items: (variable address, ompt dependence type).
    pub deps: Vec<(u64, u32)>,
    pub complete: bool,

    // --- reconstruction state
    open: Option<(u64, u32, u16, u16)>, // start ts, thread, cpu, numa
    waiting: bool,
    done: bool,
    last_mark: u64,
    first_part: u32,
    pub last_part: u32,
    pending_children: Vec<u32>,
    open_groups: Vec<u32>,
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

/// The acyclic graph of task parts used for path analysis.
#[derive(Default)]
pub struct Parts {
    /// Exclusive work time of each part in ns (0 for virtual join nodes).
    pub weight: Vec<u64>,
    /// Owning task index, or `NONE` for virtual join nodes.
    pub task: Vec<u32>,
    pub edges: Vec<(u32, u32)>,
}

impl Parts {
    fn node(&mut self, task: u32) -> u32 {
        self.weight.push(0);
        self.task.push(task);
        (self.weight.len() - 1) as u32
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
    pub edges: Vec<Edge>,
    pub parts: Parts,
    pub n_events: usize,
}

struct ThreadState {
    current: u32,
    stack: Vec<u32>,
}

struct Builder {
    tasks: Vec<Task>,
    task_idx: HashMap<u64, u32>,
    parallels: Vec<Parallel>,
    par_idx: HashMap<u64, u32>,
    threads: Vec<ThreadInfo>,
    tstate: HashMap<u32, ThreadState>,
    edges: Vec<Edge>,
    parts: Parts,
    groups: Vec<Vec<u32>>,
    deferred_deps: Vec<(u32, u32)>,
}

impl Builder {
    fn task(&self, id: u64) -> Option<u32> {
        self.task_idx.get(&id).copied()
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
            kind,
            flags,
            parent,
            parallel,
            team_index: 0,
            create_ts: e.ts,
            create_thread: e.thread,
            codeptr: 0,
            taskloop: false,
            end: 0,
            work: 0,
            segs: vec![],
            deps: vec![],
            complete: false,
            open: None,
            waiting: false,
            done: false,
            last_mark: 0,
            first_part: part,
            last_part: part,
            pending_children: vec![],
            open_groups: vec![],
            in_group: NONE,
            barriers: 0,
        });
        self.task_idx.insert(id, t);
        if parent != NONE {
            self.edges.push(Edge { from: parent, to: t, kind: EdgeKind::Create });
        }
        t
    }

    fn open_seg(&mut self, t: u32, e: &Event) {
        let task = &mut self.tasks[t as usize];
        if task.open.is_none() && !task.waiting && !task.done {
            task.open = Some((e.ts, e.thread, e.cpu, e.numa));
        }
    }

    fn close_seg(&mut self, t: u32, ts: u64, cpu: u16) {
        let task = &mut self.tasks[t as usize];
        if let Some((start, thread, cpu0, numa)) = task.open.take() {
            let end = ts.max(start);
            task.work += end - start;
            task.end = task.end.max(end);
            task.segs.push(Seg { thread, cpu: cpu0, cpu_end: cpu, numa, start, end });
        }
    }

    /// Work accumulated by task `t` up to time `ts`.
    fn work_until(&self, t: u32, ts: u64) -> u64 {
        let task = &self.tasks[t as usize];
        task.work + task.open.map_or(0, |(s, ..)| ts.saturating_sub(s))
    }

    /// Ends the current part of `t` at `ts`, assigning it the work done since the last cut.
    fn end_part(&mut self, t: u32, ts: u64) {
        let w = self.work_until(t, ts);
        let task = &mut self.tasks[t as usize];
        self.parts.weight[task.last_part as usize] += w - task.last_mark;
        task.last_mark = w;
    }

    /// Cuts task `t` at `ts`; returns (part before, part after).
    fn split(&mut self, t: u32, ts: u64) -> (u32, u32) {
        self.end_part(t, ts);
        let old = self.tasks[t as usize].last_part;
        let new = self.parts.node(t);
        self.parts.edges.push((old, new));
        self.tasks[t as usize].last_part = new;
        (old, new)
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
                    self.parts.edges.push((self.parallels[p as usize].end_node, cont));
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
                    self.parts.edges.push((fork, self.tasks[t as usize].first_part));
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
                self.tasks[t as usize].complete = true;
                self.tasks[t as usize].done = true;
                let p = self.tasks[t as usize].parallel;
                if let Some(par) = self.parallels.get(p as usize) {
                    self.parts.edges.push((self.tasks[t as usize].last_part, par.end_node));
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
                    self.parts.edges.push((before, self.tasks[t as usize].first_part));
                    let ptask = &mut self.tasks[p as usize];
                    ptask.pending_children.push(t);
                    let g = ptask.open_groups.last().copied().unwrap_or(ptask.in_group);
                    if g != NONE {
                        self.tasks[t as usize].in_group = g;
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
                        let task = &mut self.tasks[prior as usize];
                        task.complete = true;
                        task.done = true;
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
                    self.tasks[t as usize].deps.push((e.b, e.c as u32));
                }
            }
            kind::TASK_DEPENDENCE => {
                if let (Some(a), Some(b)) = (self.task(e.a), self.task(e.b)) {
                    self.edges.push(Edge { from: a, to: b, kind: EdgeKind::Depend });
                    self.deferred_deps.push((a, b));
                }
            }
            kind::SYNC_WAIT => {
                let Some(t) = self.task(e.b) else { return };
                if e.endpoint == ENDPOINT_BEGIN {
                    self.close_seg(t, e.ts, e.cpu);
                    self.tasks[t as usize].waiting = true;
                } else {
                    self.tasks[t as usize].waiting = false;
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
                    for c in std::mem::take(&mut self.tasks[t as usize].pending_children) {
                        self.parts.edges.push((self.tasks[c as usize].last_part, after));
                    }
                }
            }
            SYNC_TASKGROUP => {
                if begin {
                    self.groups.push(vec![]);
                    self.tasks[t as usize].open_groups.push((self.groups.len() - 1) as u32);
                } else if let Some(g) = self.tasks[t as usize].open_groups.pop() {
                    let (_, after) = self.split(t, e.ts);
                    for c in std::mem::take(&mut self.groups[g as usize]) {
                        self.parts.edges.push((self.tasks[c as usize].last_part, after));
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
                    let k = self.tasks[t as usize].barriers as usize;
                    self.tasks[t as usize].barriers += 1;
                    while self.parallels[p as usize].barrier_nodes.len() <= k {
                        let n = self.parts.node(NONE);
                        self.parallels[p as usize].barrier_nodes.push(n);
                        self.parallels[p as usize].barrier_released.push(false);
                    }
                    let join = self.parallels[p as usize].barrier_nodes[k];
                    let (before, after) = self.split(t, e.ts);
                    self.parts.edges.push((before, join));
                    self.parts.edges.push((join, after));
                    if e.a == SYNC_BARRIER_IMPLICIT_PARALLEL {
                        // libomp reports the end of the region-final barrier of
                        // worker threads only when they are reused; nothing
                        // executes in the implicit task after this point.
                        self.close_seg(t, e.ts, e.cpu);
                        self.tasks[t as usize].done = true;
                    }
                } else {
                    let k = (self.tasks[t as usize].barriers as usize).wrapping_sub(1);
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
                            self.parts.edges.push((self.tasks[c as usize].last_part, join));
                        }
                    }
                }
            }
        }
    }

    fn finish(&mut self, t_end: u64) {
        for t in 0..self.tasks.len() as u32 {
            if self.tasks[t as usize].open.is_some() {
                let cpu = self.tasks[t as usize].open.unwrap().2;
                self.close_seg(t, t_end, cpu);
            }
            if !self.tasks[t as usize].complete {
                self.end_part(t, t_end);
            }
        }
        for &(a, b) in &self.deferred_deps {
            self.parts.edges.push((self.tasks[a as usize].last_part, self.tasks[b as usize].first_part));
        }
        // Whatever was not joined at a barrier is joined by the end of its region.
        for par in &mut self.parallels {
            for c in std::mem::take(&mut par.unjoined) {
                self.parts.edges.push((self.tasks[c as usize].last_part, par.end_node));
            }
        }
    }
}

impl Model {
    pub fn build(trace: &Trace) -> Model {
        let mut events = trace.events.clone();
        // Per-thread streams are already ordered; a stable sort merges them.
        events.sort_by_key(|e| e.ts);
        let t0 = events.first().map_or(0, |e| e.ts);
        for e in &mut events {
            e.ts -= t0;
        }
        let wall = events.last().map_or(0, |e| e.ts);

        let mut b = Builder {
            tasks: vec![],
            task_idx: HashMap::new(),
            parallels: vec![],
            par_idx: HashMap::new(),
            threads: vec![],
            tstate: HashMap::new(),
            edges: vec![],
            parts: Parts::default(),
            groups: vec![],
            deferred_deps: vec![],
        };
        for e in &events {
            b.event(e);
        }
        b.finish(wall);
        b.threads.sort_by_key(|t| t.id);

        Model {
            rank: trace.rank,
            pid: trace.pid,
            host: trace.host.clone(),
            info: {
                // The runtime library is a header field of its own; show it with the rest.
                let mut info = trace.info.clone();
                if !trace.runtime_module.is_empty() {
                    let at = info.iter().position(|(k, _)| !k.starts_with("omp.") && !k.starts_with("tool.")).unwrap_or(info.len());
                    info.insert(at, ("omp.runtime_library".to_string(), trace.runtime_module.clone()));
                }
                info
            },
            wall,
            tasks: b.tasks,
            parallels: b.parallels,
            threads: b.threads,
            edges: b.edges,
            parts: b.parts,
            n_events: events.len(),
        }
    }

    pub fn total_work(&self) -> u64 {
        self.tasks.iter().map(|t| t.work).sum()
    }
}

impl Default for ThreadState {
    fn default() -> Self {
        ThreadState { current: NONE, stack: vec![] }
    }
}
