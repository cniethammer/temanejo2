//! Tests of graph reconstruction and analysis on small synthetic traces.
//!
//! The traces are written by hand following the OMPT callback semantics of
//! the OpenMP specification (no runtime-specific behaviour), with timings
//! chosen so that work, critical path and origin shares can be computed by
//! hand. Every scenario documents its expected values next to the trace.

use crate::analysis::{self, PathResult};
use crate::model::{EdgeKind, Model, TaskKind};
use std::collections::BTreeMap;
use tmj_trace::{kind, Event, Trace, ENDPOINT_BEGIN, ENDPOINT_END};

// OMPT constants (omp-tools.h)
const FLAG_INITIAL: u32 = 0x1;
const FLAG_IMPLICIT: u32 = 0x2;
const FLAG_EXPLICIT: u32 = 0x4;
const STATUS_COMPLETE: u64 = 1;
const STATUS_SWITCH: u64 = 7;
const SYNC_TASKWAIT: u64 = 5;
const SYNC_BARRIER_IMPLICIT_PARALLEL: u64 = 9;
const THREAD_INITIAL: u64 = 1;
const THREAD_WORKER: u64 = 2;

/// Writes events per thread, like the tool does, and concatenates the
/// per-thread streams into a trace (so the trace is not globally sorted).
#[derive(Default)]
struct TraceBuilder {
    threads: BTreeMap<u32, Vec<Event>>,
}

impl TraceBuilder {
    #[allow(clippy::too_many_arguments)]
    fn ev(&mut self, thread: u32, ts: u64, kind: u8, endpoint: u8, flags: u32, a: u64, b: u64, c: u64, d: u64) -> &mut Self {
        let events = self.threads.entry(thread).or_default();
        assert!(events.last().is_none_or(|e| e.ts <= ts), "events of a thread must be in time order");
        events.push(Event { kind, endpoint, cpu: thread as u16, numa: 0, thread, flags, ts, a, b, c, d });
        self
    }

    fn thread_begin(&mut self, th: u32, ts: u64, initial: bool) -> &mut Self {
        let ty = if initial { THREAD_INITIAL } else { THREAD_WORKER };
        self.ev(th, ts, kind::THREAD_BEGIN, 0, 0, ty, 1000 + th as u64, 0, 0)
    }

    /// Begin and end of the initial task, reported as implicit task of the
    /// implicit parallel region `par` (OpenMP 5.x).
    fn initial_begin(&mut self, th: u32, ts: u64, task: u64, par: u64) -> &mut Self {
        self.ev(th, ts, kind::IMPLICIT_TASK, ENDPOINT_BEGIN, FLAG_INITIAL, task, par, 0, 1)
    }

    fn initial_end(&mut self, th: u32, ts: u64, task: u64, par: u64) -> &mut Self {
        self.ev(th, ts, kind::IMPLICIT_TASK, ENDPOINT_END, FLAG_INITIAL, task, par, 0, 1)
    }

    fn parallel_begin(&mut self, th: u32, ts: u64, par: u64, encountering: u64, n: u64, codeptr: u64) -> &mut Self {
        self.ev(th, ts, kind::PARALLEL_BEGIN, 0, 0x1, par, encountering, n, codeptr)
    }

    fn parallel_end(&mut self, th: u32, ts: u64, par: u64, encountering: u64) -> &mut Self {
        self.ev(th, ts, kind::PARALLEL_END, 0, 0x1, par, encountering, 0, 0)
    }

    fn implicit_begin(&mut self, th: u32, ts: u64, task: u64, par: u64, index: u64, n: u64) -> &mut Self {
        self.ev(th, ts, kind::IMPLICIT_TASK, ENDPOINT_BEGIN, FLAG_IMPLICIT, task, par, index, n)
    }

    fn implicit_end(&mut self, th: u32, ts: u64, task: u64, par: u64) -> &mut Self {
        self.ev(th, ts, kind::IMPLICIT_TASK, ENDPOINT_END, FLAG_IMPLICIT, task, par, 0, 0)
    }

    fn create(&mut self, th: u32, ts: u64, task: u64, parent: u64, codeptr: u64) -> &mut Self {
        self.ev(th, ts, kind::TASK_CREATE, 0, FLAG_EXPLICIT, task, parent, 0, codeptr)
    }

    fn schedule(&mut self, th: u32, ts: u64, prior: u64, status: u64, next: u64) -> &mut Self {
        self.ev(th, ts, kind::TASK_SCHEDULE, 0, 0, prior, status, next, 0)
    }

    fn dependence(&mut self, th: u32, ts: u64, pred: u64, succ: u64) -> &mut Self {
        self.ev(th, ts, kind::TASK_DEPENDENCE, 0, 0, pred, succ, 0, 0)
    }

    /// Begin of a synchronisation region and of waiting in it.
    fn sync_begin(&mut self, th: u32, ts: u64, sync: u64, task: u64, par: u64) -> &mut Self {
        self.ev(th, ts, kind::SYNC_REGION, ENDPOINT_BEGIN, 0, sync, task, par, 0xdead);
        self.ev(th, ts, kind::SYNC_WAIT, ENDPOINT_BEGIN, 0, sync, task, par, 0xdead)
    }

    /// End of waiting and of the synchronisation region.
    fn sync_end(&mut self, th: u32, ts: u64, sync: u64, task: u64, par: u64) -> &mut Self {
        self.ev(th, ts, kind::SYNC_WAIT, ENDPOINT_END, 0, sync, task, par, 0xdead);
        self.ev(th, ts, kind::SYNC_REGION, ENDPOINT_END, 0, sync, task, par, 0xdead)
    }

    fn build(&mut self) -> Trace {
        let events = self.threads.values().flatten().copied().collect();
        Trace { rank: 0, pid: 1, epoch_ns: 0, host: "test".into(), events, complete: true, ..Default::default() }
    }
}

fn analyse(trace: &Trace) -> (Model, PathResult) {
    let m = Model::build(trace);
    let crit = analysis::critical_path(&m, None);
    (m, crit)
}

/// Index of the task with OMPT-side id `id`.
fn task(m: &Model, id: u64) -> usize {
    m.tasks.iter().position(|t| t.id == id).unwrap_or_else(|| panic!("task {id} not in model"))
}

fn work(m: &Model, id: u64) -> u64 {
    m.tasks[task(m, id)].work
}

fn crit_time(m: &Model, crit: &PathResult, id: u64) -> u64 {
    crit.task_time[task(m, id)]
}

/// Checks the invariants that must hold for every trace: per-task critical
/// time never exceeds work, the critical path is at most T_1, and origins
/// partition the tasks so their work and critical times sum to T_1 and T_inf.
fn check_invariants(m: &Model, crit: &PathResult) {
    for (i, t) in m.tasks.iter().enumerate() {
        assert!(crit.task_time[i] <= t.work, "task {}: critical time {} > work {}", t.id, crit.task_time[i], t.work);
    }
    assert_eq!(crit.task_time.iter().sum::<u64>(), crit.length, "critical times must add up to T_inf");
    assert!(crit.length <= m.total_work(), "T_inf must not exceed T_1");

    let origins = analysis::origins(m, crit);
    let mut seen = vec![false; m.tasks.len()];
    for o in &origins {
        for &t in &o.tasks {
            assert!(!seen[t as usize], "task {} in two origins", m.tasks[t as usize].id);
            seen[t as usize] = true;
        }
    }
    assert!(seen.iter().all(|&s| s), "every task must belong to an origin");
    assert_eq!(origins.iter().map(|o| o.tasks.len()).sum::<usize>(), m.tasks.len());
    assert_eq!(origins.iter().map(|o| o.work).sum::<u64>(), m.total_work(), "origin work must add up to T_1");
    assert_eq!(origins.iter().map(|o| o.critical).sum::<u64>(), crit.length, "origin critical times must add up to T_inf");
    for o in &origins {
        assert!(o.own_critical <= o.work, "an origin's own critical path cannot exceed its work");
        assert!(o.peak as usize <= m.threads.len(), "peak concurrency cannot exceed the thread count");
        assert_eq!(o.peak == 0, o.work == 0, "tasks that worked were running at some point");
    }
}

// ------------------------------------------------------------------ scenarios

/// Only the initial task, working from 0 to 100.
#[test]
fn serial_program() {
    let mut b = TraceBuilder::default();
    b.thread_begin(0, 0, true).initial_begin(0, 0, 1, 1).initial_end(0, 100, 1, 1);
    let (m, crit) = analyse(&b.build());

    assert_eq!(m.tasks.len(), 1);
    assert!(m.tasks[0].kind == TaskKind::Initial);
    assert_eq!(m.total_work(), 100);
    assert_eq!(crit.length, 100);
    assert_eq!(crit_time(&m, &crit, 1), 100);
    check_invariants(&m, &crit);
}

/// One thread. The initial task I creates A (codeptr 0xA) and B (0xB),
/// waits in a taskwait while A and B run, then continues.
///
/// ```text
/// I: work 0-10, create A, work 10-12, create B, work 12-20, taskwait 20-102,
///    work 102-132
/// A: 21-51 (30)    B: 51-101 (50)
/// ```
/// Parts of I: P1 = 10, P2 = 2, P3 = 8, P4 = 30.
/// Paths: P1+P2+B+P4 = 92 (longest), P1+A+P4 = 70, P1+P2+P3+P4 = 50.
fn taskwait_trace(codeptr_a: u64, codeptr_b: u64) -> Trace {
    let mut b = TraceBuilder::default();
    b.thread_begin(0, 0, true)
        .initial_begin(0, 0, 1, 1)
        .create(0, 10, 2, 1, codeptr_a)
        .create(0, 12, 3, 1, codeptr_b)
        .sync_begin(0, 20, SYNC_TASKWAIT, 1, 1)
        .schedule(0, 21, 1, STATUS_SWITCH, 2)
        .schedule(0, 51, 2, STATUS_COMPLETE, 3)
        .schedule(0, 101, 3, STATUS_COMPLETE, 1)
        .sync_end(0, 102, SYNC_TASKWAIT, 1, 1)
        .initial_end(0, 132, 1, 1);
    b.build()
}

#[test]
fn taskwait_counts_and_work() {
    let (m, crit) = analyse(&taskwait_trace(0xA, 0xB));
    assert_eq!(m.tasks.len(), 3);
    assert_eq!(m.tasks.iter().filter(|t| t.kind == TaskKind::Explicit).count(), 2);
    assert_eq!(work(&m, 1), 10 + 2 + 8 + 30);
    assert_eq!(work(&m, 2), 30);
    assert_eq!(work(&m, 3), 50);
    assert_eq!(m.total_work(), 130);
    let creates = m.edges.iter().filter(|e| e.kind == EdgeKind::Create).count();
    assert_eq!(creates, 2);
    check_invariants(&m, &crit);
}

#[test]
fn taskwait_critical_path() {
    let (m, crit) = analyse(&taskwait_trace(0xA, 0xB));
    assert_eq!(crit.length, 92);
    assert_eq!(crit_time(&m, &crit, 1), 10 + 2 + 30, "I before B and after the taskwait");
    assert_eq!(crit_time(&m, &crit, 2), 0, "A is shorter than B");
    assert_eq!(crit_time(&m, &crit, 3), 50);
    let (i, b) = (task(&m, 1) as u32, task(&m, 3) as u32);
    assert!(crit.transitions.contains(&(i, b)) && crit.transitions.contains(&(b, i)));
}

#[test]
fn origins_separate_locations() {
    let (m, crit) = analyse(&taskwait_trace(0xA, 0xB));
    let origins = analysis::origins(&m, &crit);
    assert_eq!(origins.len(), 3);
    let by = |initial: bool, codeptr: u64| origins.iter().find(|o| o.initial == initial && o.codeptr == codeptr).unwrap();
    let (init, a, b) = (by(true, 0), by(false, 0xA), by(false, 0xB));
    assert_eq!((init.tasks.len(), init.work, init.critical), (1, 50, 42));
    assert_eq!((a.tasks.len(), a.work, a.critical), (1, 30, 0));
    assert_eq!((b.tasks.len(), b.work, b.critical), (1, 50, 50));
    // A single task: its own critical path is its work, peak 1.
    assert_eq!((init.own_critical, init.peak), (50, 1));
    assert_eq!((a.own_critical, a.peak), (30, 1));
    assert!(a.has_explicit && !a.has_implicit && !a.has_taskloop);
    // Sorted by work, largest first.
    assert!(origins.windows(2).all(|w| w[0].work >= w[1].work));
    check_invariants(&m, &crit);
}

#[test]
fn origins_merge_same_location() {
    // A and B created at the same code location form one origin.
    let (m, crit) = analyse(&taskwait_trace(0xC, 0xC));
    let origins = analysis::origins(&m, &crit);
    assert_eq!(origins.len(), 2);
    let c = origins.iter().find(|o| o.codeptr == 0xC).unwrap();
    assert_eq!((c.tasks.len(), c.work, c.critical), (2, 80, 50));
    // A and B are independent: own critical path max(30, 50) = 50, so the
    // average parallelism is 80 / 50; on one thread they never overlap.
    assert_eq!((c.own_critical, c.peak), (50, 1));
    check_invariants(&m, &crit);
}

/// One thread. I creates A, B (B depends on A) and C, then waits.
///
/// ```text
/// I: work 0-5, create A, 5-6 create B (+ dependence A -> B), 6-7 create C,
///    7-8, taskwait 8-44, work 44-50
/// A: 8-18 (10)   C: 18-23 (5)   B: 23-43 (20)
/// ```
/// Parts of I: P1 = 5, P2 = 1, P3 = 1, P4 = 1, P5 = 6.
/// Longest path: P1 + A + B + P5 = 41.
fn dependence_trace() -> Trace {
    let mut b = TraceBuilder::default();
    b.thread_begin(0, 0, true)
        .initial_begin(0, 0, 1, 1)
        .create(0, 5, 2, 1, 0xA)
        .create(0, 6, 3, 1, 0xB)
        .dependence(0, 6, 2, 3)
        .create(0, 7, 4, 1, 0xC)
        .sync_begin(0, 8, SYNC_TASKWAIT, 1, 1)
        .schedule(0, 8, 1, STATUS_SWITCH, 2)
        .schedule(0, 18, 2, STATUS_COMPLETE, 4)
        .schedule(0, 23, 4, STATUS_COMPLETE, 3)
        .schedule(0, 43, 3, STATUS_COMPLETE, 1)
        .sync_end(0, 44, SYNC_TASKWAIT, 1, 1)
        .initial_end(0, 50, 1, 1);
    b.build()
}

#[test]
fn dependence_critical_path() {
    let (m, crit) = analyse(&dependence_trace());
    assert_eq!(m.tasks.len(), 4);
    assert_eq!(m.total_work(), 14 + 10 + 20 + 5);
    let deps: Vec<_> = m.edges.iter().filter(|e| e.kind == EdgeKind::Depend).collect();
    assert_eq!(deps.len(), 1);
    assert_eq!((deps[0].from as usize, deps[0].to as usize), (task(&m, 2), task(&m, 3)));

    assert_eq!(crit.length, 41);
    assert_eq!(crit_time(&m, &crit, 1), 5 + 6);
    assert_eq!(crit_time(&m, &crit, 2), 10);
    assert_eq!(crit_time(&m, &crit, 3), 20);
    assert_eq!(crit_time(&m, &crit, 4), 0);
    check_invariants(&m, &crit);
}

#[test]
fn without_dependence_the_path_is_shorter() {
    // Same trace without the A -> B dependence: P1 + P2 + B + P5 = 32.
    let mut trace = dependence_trace();
    trace.events.retain(|e| e.kind != kind::TASK_DEPENDENCE);
    let (m, crit) = analyse(&trace);
    assert_eq!(crit.length, 32);
    assert_eq!(crit_time(&m, &crit, 2), 0);
    check_invariants(&m, &crit);
}

/// Two threads. I forks a team of two (implicit tasks 10 on thread 0 and 11
/// on thread 1). Task 10 creates explicit task 20, which thread 1 executes
/// while waiting in the region's final barrier. After the join I continues.
///
/// ```text
/// thread 0: I 0-10 | fork | 10: 10-15 create 20, 15-16, barrier 16-58 | join | I 59-70
/// thread 1:          11: 10-13, barrier 13-58, executes 20: 17-57 (40)
/// ```
/// Work: I = 10 + 11, task 10 = 6, task 11 = 3, task 20 = 40, T_1 = 70.
/// Longest path: I before fork (10) + task 10 up to the creation (5) + 20 (40)
/// + barrier + region end + I after the join (11) = 66.
fn parallel_trace() -> Trace {
    let mut b = TraceBuilder::default();
    b.thread_begin(0, 0, true)
        .initial_begin(0, 0, 1, 1)
        .parallel_begin(0, 10, 2, 1, 2, 0x100)
        .implicit_begin(0, 10, 10, 2, 0, 2)
        .create(0, 15, 20, 10, 0x200)
        .sync_begin(0, 16, SYNC_BARRIER_IMPLICIT_PARALLEL, 10, 2)
        .sync_end(0, 58, SYNC_BARRIER_IMPLICIT_PARALLEL, 10, 2)
        .implicit_end(0, 59, 10, 2)
        .parallel_end(0, 59, 2, 1)
        .initial_end(0, 70, 1, 1);
    b.thread_begin(1, 9, false)
        .implicit_begin(1, 10, 11, 2, 1, 2)
        .sync_begin(1, 13, SYNC_BARRIER_IMPLICIT_PARALLEL, 11, 2)
        .schedule(1, 17, 11, STATUS_SWITCH, 20)
        .schedule(1, 57, 20, STATUS_COMPLETE, 11)
        .sync_end(1, 58, SYNC_BARRIER_IMPLICIT_PARALLEL, 11, 2)
        .implicit_end(1, 59, 11, 2);
    b.build()
}

#[test]
fn parallel_region_counts() {
    let (m, crit) = analyse(&parallel_trace());
    assert_eq!(m.tasks.len(), 4);
    let count = |k: TaskKind| m.tasks.iter().filter(|t| t.kind == k).count();
    assert_eq!((count(TaskKind::Initial), count(TaskKind::Implicit), count(TaskKind::Explicit)), (1, 2, 1));
    assert_eq!(m.threads.len(), 2);
    let region = m.parallels.iter().find(|p| p.id == 2).expect("parallel region 2");
    assert_eq!((region.requested, region.actual), (2, 2));
    assert_eq!(region.parent_task as usize, task(&m, 1));
    // Implicit tasks are children of the encountering task.
    for id in [10, 11] {
        assert_eq!(m.tasks[task(&m, id)].parent as usize, task(&m, 1));
    }
    check_invariants(&m, &crit);
}

#[test]
fn parallel_region_work_and_stealing() {
    let (m, _) = analyse(&parallel_trace());
    assert_eq!(work(&m, 1), 10 + 11);
    assert_eq!(work(&m, 10), 6);
    assert_eq!(work(&m, 11), 3);
    assert_eq!(work(&m, 20), 40);
    assert_eq!(m.total_work(), 70);
    // Task 20 was created on thread 0 and executed on thread 1.
    let t20 = &m.tasks[task(&m, 20)];
    assert_eq!(t20.create_thread, 0);
    assert_eq!(t20.segs.len(), 1);
    assert_eq!((t20.segs[0].thread, t20.segs[0].start, t20.segs[0].end), (1, 17, 57));
}

#[test]
fn parallel_region_critical_path() {
    let (m, crit) = analyse(&parallel_trace());
    assert_eq!(crit.length, 66);
    assert_eq!(crit_time(&m, &crit, 1), 21);
    assert_eq!(crit_time(&m, &crit, 10), 5);
    assert_eq!(crit_time(&m, &crit, 11), 0);
    assert_eq!(crit_time(&m, &crit, 20), 40);

    let origins = analysis::origins(&m, &crit);
    let region = origins.iter().find(|o| o.codeptr == 0x100).unwrap();
    assert!(region.has_implicit && !region.has_explicit);
    assert_eq!((region.tasks.len(), region.work, region.critical), (2, 9, 5));
    // Implicit tasks 10 (5 + 1 before the barrier) and 11 (3) run side by side
    // on two threads: own critical path 6, peak 2.
    assert_eq!((region.own_critical, region.peak), (6, 2));
    check_invariants(&m, &crit);
}

#[test]
fn trace_order_does_not_matter() {
    // The model must not depend on the order in which per-thread streams are
    // stored in the file.
    let mut trace = parallel_trace();
    let (m1, c1) = analyse(&trace);
    // Thread 1's stream first; the order within a thread is kept, as in a file.
    trace.events.sort_by_key(|e| e.thread == 0);
    let (m2, c2) = analyse(&trace);
    assert_eq!(m1.total_work(), m2.total_work());
    assert_eq!(c1.length, c2.length);
    assert_eq!(m1.tasks.len(), m2.tasks.len());
}

// ------------------------------------------------------------------ regions

fn select(m: &Model, ids: &[u64]) -> Vec<bool> {
    let mut sel = vec![false; m.tasks.len()];
    for &id in ids {
        sel[task(m, id)] = true;
    }
    sel
}

#[test]
fn region_with_dependence_chain() {
    // A -> B is a chain: T_1 = T_inf = 30, no parallelism.
    let m = Model::build(&dependence_trace());
    let r = analysis::analyze_region(&m, &select(&m, &[2, 3]));
    assert_eq!(r.n_tasks, 2);
    assert_eq!(r.work, 30);
    assert_eq!(r.critical, 30);
    assert_eq!(r.max_concurrency, 1);
    assert_eq!((r.t_begin, r.t_end), (8, 43));
}

#[test]
fn region_of_independent_tasks() {
    // A and C are independent: T_1 = 15, T_inf = max(10, 5) = 10.
    let m = Model::build(&dependence_trace());
    let r = analysis::analyze_region(&m, &select(&m, &[2, 4]));
    assert_eq!((r.work, r.critical), (15, 10));
    assert_eq!(r.path.task_time[task(&m, 2)], 10);
    assert_eq!(r.path.task_time[task(&m, 4)], 0);
}

#[test]
fn region_concurrency_on_two_threads() {
    // Implicit tasks 10 (10-16) and 11 (10-13) run at the same time; 20 runs
    // alone (17-57).
    let m = Model::build(&parallel_trace());
    let r = analysis::analyze_region(&m, &select(&m, &[10, 11, 20]));
    assert_eq!(r.n_tasks, 3);
    assert_eq!(r.work, 49);
    assert_eq!(r.max_concurrency, 2);
    assert_eq!((r.t_begin, r.t_end), (10, 57));
    assert_eq!(r.per_thread, vec![(0, 6), (1, 3 + 40)]);
    // Without the initial task the longest path is task 10 up to the
    // creation of 20 (5), then 20 (40).
    assert_eq!(r.critical, 45);
}

#[test]
fn downsample_preserves_area() {
    // Step function: 2 tasks from 0 to 50, 1 task from 50 to 100.
    let profile = [(0, 2), (50, 1), (100, 0)];
    let samples = analysis::downsample(&profile, 0, 100, 10);
    assert_eq!(samples.len(), 10);
    let area: f64 = samples.iter().map(|s| s.1 * 10.0).sum();
    assert!((area - 150.0).abs() < 1e-9, "area {area}");
    assert!((samples[0].1 - 2.0).abs() < 1e-9 && (samples[9].1 - 1.0).abs() < 1e-9);
}

// ------------------------------------------------------------------ groups

/// The initial task (1) creates explicit tasks; `spec` lists (id, parent id,
/// code location). The tasks never run, which grouping does not need.
fn creation_trace(spec: &[(u64, u64, u64)]) -> Trace {
    let mut b = TraceBuilder::default();
    b.thread_begin(0, 0, true).initial_begin(0, 0, 1, 1);
    for (k, &(id, parent, codeptr)) in spec.iter().enumerate() {
        b.create(0, 10 + k as u64, id, parent, codeptr);
    }
    b.initial_end(0, 10_000, 1, 1);
    b.build()
}

fn ids(m: &Model, tasks: &[u32]) -> Vec<u64> {
    tasks.iter().map(|&i| m.tasks[i as usize].id).collect()
}

/// A binary recursion below task 1001: node j of a heap has the id 1000 + j
/// and is created by node j / 2. The two children of a node have different
/// code locations, so no sibling group exists.
fn recursion(descendants: u64) -> Vec<(u64, u64, u64)> {
    let mut spec = vec![(1001, 1, 0x1)];
    spec.extend((2..2 + descendants).map(|j| (1000 + j, 1000 + j / 2, 0x100 + (j % 2))));
    spec
}

/// Properties every grouping must have: each task is a direct member of at
/// most one group, group sizes add up over the tree, no expansion reveals
/// more than the limit, and a group is represented by its first task.
fn check_grouping(m: &Model, gr: &analysis::Grouping) {
    use crate::model::NONE;
    let mut direct = vec![0usize; m.tasks.len()];
    for (g, grp) in gr.groups.iter().enumerate() {
        assert!(grp.tasks.windows(2).all(|w| w[0] < w[1]), "direct tasks are sorted");
        for &i in &grp.tasks {
            direct[i as usize] += 1;
            assert_eq!(gr.task_group[i as usize], g as u32);
        }
        let below: usize = grp.children.iter().map(|&c| gr.groups[c as usize].n).sum();
        assert_eq!(grp.n, grp.tasks.len() + below, "group {g}: size adds up over its children");
        assert!(grp.n >= 1 && grp.tasks.len() + grp.children.len() >= 1);
        assert!(grp.tasks.len() + grp.children.len() <= gr.max_group, "group {g} reveals {} nodes", grp.tasks.len() + grp.children.len());
        for &c in &grp.children {
            assert!(c as usize > g, "child groups come after their parent");
            assert_eq!(gr.groups[c as usize].parent, g as u32);
            assert!(gr.groups[c as usize].n < grp.n, "a child group is smaller than its parent");
        }
        let members = gr.members(g);
        assert_eq!(members.len(), grp.n);
        assert_eq!(grp.rep, members[0], "the representative is the first-created member");
        assert!(!members.contains(&grp.anchor), "the anchor is outside the group");
    }
    assert!(direct.iter().all(|&c| c <= 1), "a task is a direct member of at most one group");
    for (i, &g) in gr.task_group.iter().enumerate() {
        assert_eq!(g == NONE, direct[i] == 0);
    }
}

#[test]
fn siblings_from_one_location_form_a_group() {
    use analysis::{GroupKind, GROUP_MIN_SIBLINGS};
    // 8 tasks at 0xA (a group), 7 at 0xB (one too few), all children of task 1.
    let mut spec: Vec<(u64, u64, u64)> = (0..GROUP_MIN_SIBLINGS as u64).map(|k| (10 + k, 1, 0xA)).collect();
    spec.extend((0..GROUP_MIN_SIBLINGS as u64 - 1).map(|k| (100 + k, 1, 0xB)));
    let m = Model::build(&creation_trace(&spec));
    let gr = analysis::group_tasks(&m, 1000);
    assert_eq!(gr.groups.len(), 1);
    let g = &gr.groups[0];
    assert!(g.kind == GroupKind::Fan && g.parent == crate::model::NONE && g.part.is_none());
    assert_eq!(g.anchor as usize, task(&m, 1));
    assert_eq!(g.codeptr, 0xA);
    assert_eq!(ids(&m, &g.tasks), (10..18).collect::<Vec<_>>());
    assert_eq!(gr.task_group[task(&m, 100)], crate::model::NONE, "tasks outside groups have none");
    check_grouping(&m, &gr);
}

#[test]
fn small_group_shows_its_descendants() {
    // 8 siblings at 0xA; sibling 10 itself creates 8 tasks at 0xB. All 16
    // fit into one group, so the inner tasks are shown with it.
    let mut spec: Vec<(u64, u64, u64)> = (0..8).map(|k| (10 + k, 1, 0xA)).collect();
    spec.extend((0..8).map(|k| (100 + k, 10, 0xB)));
    let m = Model::build(&creation_trace(&spec));
    let gr = analysis::group_tasks(&m, 1000);
    assert_eq!(gr.groups.len(), 1);
    assert_eq!((gr.groups[0].n, gr.groups[0].tasks.len()), (16, 16));
    check_grouping(&m, &gr);
}

#[test]
fn deep_task_tree_forms_a_subtree_group() {
    use analysis::{GroupKind, GROUP_MIN_SUBTREE};
    let n = GROUP_MIN_SUBTREE as u64;
    let m = Model::build(&creation_trace(&recursion(n)));
    let gr = analysis::group_tasks(&m, 1000);
    assert_eq!(gr.groups.len(), 1);
    assert!(gr.groups[0].kind == GroupKind::Subtree);
    assert_eq!(gr.groups[0].anchor as usize, task(&m, 1001), "the topmost explicit task is the root");
    assert_eq!(gr.groups[0].n as u64, n);
    check_grouping(&m, &gr);
    // One task fewer than the minimum: no group.
    assert!(analysis::group_tasks(&Model::build(&creation_trace(&recursion(n - 1))), 1000).groups.is_empty());
}

#[test]
fn large_sibling_group_is_split_into_parts() {
    // 100 leaf tasks from one location with a limit of 16: parts of 16
    // siblings each (the last one smaller), labelled by their position.
    let spec: Vec<(u64, u64, u64)> = (0..100).map(|k| (10 + k, 1, 0xA)).collect();
    let m = Model::build(&creation_trace(&spec));
    let gr = analysis::group_tasks(&m, 16);
    check_grouping(&m, &gr);
    let top: Vec<_> = gr.groups.iter().filter(|g| g.parent == crate::model::NONE).collect();
    assert_eq!(top.len(), 1);
    assert_eq!(top[0].n, 100);
    let leaves: Vec<_> = gr.groups.iter().filter(|g| g.children.is_empty()).collect();
    assert!(leaves.iter().all(|g| g.kind == analysis::GroupKind::Fan && g.codeptr == 0xA && g.anchor == top[0].anchor));
    assert!(leaves.iter().all(|g| g.n <= 16));
    assert_eq!(leaves.iter().map(|g| g.n).sum::<usize>() + gr.groups.iter().filter(|g| !g.children.is_empty()).map(|g| g.tasks.len()).sum::<usize>(), 100);
    // Parts cover the siblings 0..100 without gaps, in creation order.
    let mut parts: Vec<(usize, usize, usize)> = leaves.iter().filter_map(|g| g.part).collect();
    parts.sort();
    assert!(parts.iter().all(|p| p.2 == 100));
    let mut next = 0;
    for (first, count, _) in parts {
        if first == next {
            next += count;
        }
    }
    let shown: usize = gr.groups.iter().filter(|g| !g.children.is_empty()).map(|g| g.tasks.len()).sum();
    assert_eq!(next + shown, 100, "parts and directly shown siblings cover all siblings");
}

#[test]
fn large_recursion_is_nested() {
    // 3000 tasks in a binary recursion, limits of 64 and 500.
    let m = Model::build(&creation_trace(&recursion(3000)));
    for max in [16, 64, 500] {
        let gr = analysis::group_tasks(&m, max);
        check_grouping(&m, &gr);
        let top: Vec<_> = gr.groups.iter().filter(|g| g.parent == crate::model::NONE).collect();
        assert_eq!((top.len(), top[0].n), (1, 3000));
        assert!(top[0].children.len() > 1, "limit {max}: the recursion is split");
        // Leaf groups are filled reasonably: far fewer groups than tasks.
        assert!(gr.groups.len() <= 3000 * 4 / max + 4, "limit {max}: {} groups", gr.groups.len());
    }
    // A limit above the size gives one group showing everything.
    let gr = analysis::group_tasks(&m, 5000);
    assert_eq!((gr.groups.len(), gr.groups[0].tasks.len()), (1, 3000));
}

#[test]
fn grouping_properties_hold_for_mixed_graphs() {
    // Fans with descendants, nested fans, recursion below fan members.
    let mut spec: Vec<(u64, u64, u64)> = (0..40).map(|k| (10 + k, 1, 0xA)).collect();
    spec.extend((0..200).map(|k| (100 + k, 10 + k % 5, 0xB + k % 2)));
    spec.extend((0..300).map(|k| (1000 + k, 100 + k / 3, 0xD)));
    for trace in [creation_trace(&spec), taskwait_trace(0xA, 0xB), parallel_trace()] {
        let m = Model::build(&trace);
        for max in [16, 40, 1000] {
            check_grouping(&m, &analysis::group_tasks(&m, max));
        }
    }
}

#[test]
fn grouping_properties_hold_for_random_trees() {
    // Random task trees from a fixed pseudo-random sequence: every new task
    // picks its parent among the tasks so far (biased towards recent ones or
    // towards a few hubs) and one of a few code locations.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = |bound: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % bound.max(1)
    };
    for shape in 0..12 {
        let n = 200 + 150 * shape as u64;
        let mut spec: Vec<(u64, u64, u64)> = Vec::new();
        for k in 0..n {
            let parent = match shape % 4 {
                0 => 1 + next(k + 1),                           // uniform: bushy
                1 => (k + 1).saturating_sub(next(3)).max(1),    // recent: deep chains
                2 => 1 + next((k + 1).min(6)),                  // hubs: wide fans
                _ => if next(4) == 0 { 1 + next(k + 1) } else { (k + 1).saturating_sub(next(40)).max(1) },
            };
            spec.push((k + 2, parent, 0xA0 + next(1 + shape as u64 % 5)));
        }
        let m = Model::build(&creation_trace(&spec));
        assert_eq!(m.tasks.len() as u64, n + 1);
        for max in [16, 17, 30, 100, 1000] {
            check_grouping(&m, &analysis::group_tasks(&m, max));
        }
    }
}

#[test]
fn minimum_limit_is_enforced() {
    let m = Model::build(&creation_trace(&recursion(200)));
    assert_eq!(analysis::group_tasks(&m, 1).max_group, analysis::MIN_MAX_GROUP);
}

#[test]
fn summary_of_a_task_set() {
    // A (30) and B (50) of the taskwait scenario, both on thread 0.
    let (m, crit) = analyse(&taskwait_trace(0xC, 0xC));
    let s = analysis::summarize(&m, &crit, &[task(&m, 2) as u32, task(&m, 3) as u32]);
    assert_eq!((s.n, s.work, s.min_work, s.max_work, s.mean_work), (2, 80, 30, 50, 40));
    assert_eq!(s.critical, 50, "only B is on the critical path of the graph");
    assert_eq!((s.parallelism, s.threads), (Some((50, 1)), 1));
    assert_eq!((s.t_begin, s.t_end), (21, 101));
    assert_eq!((s.thread, s.codeptr, s.kind), (0, 0xC, 2));
    assert_eq!(s.mean_create, (10 + 12) / 2);
    assert_eq!(s.mean_start, (21 + 51) / 2);
}

/// One thread: task 1 creates 10 tasks at 0xA (ids 10..19, a group) and task
/// X (id 50, 0xB). Each runs for 5; 10 -> 11 and 12 -> X depend on each other.
fn fan_with_dependences() -> Trace {
    let mut b = TraceBuilder::default();
    b.thread_begin(0, 0, true).initial_begin(0, 0, 1, 1);
    for k in 0..10 {
        b.create(0, 1 + k, 10 + k, 1, 0xA);
    }
    b.create(0, 11, 50, 1, 0xB);
    b.dependence(0, 11, 10, 11).dependence(0, 11, 12, 50).dependence(0, 11, 13, 50);
    b.sync_begin(0, 20, SYNC_TASKWAIT, 1, 1);
    let mut prev = (1, STATUS_SWITCH);
    for (k, id) in (10..20).chain([50]).enumerate() {
        b.schedule(0, 20 + 5 * k as u64, prev.0, prev.1, id);
        prev = (id, STATUS_COMPLETE);
    }
    b.schedule(0, 75, 50, STATUS_COMPLETE, 1);
    b.sync_end(0, 76, SYNC_TASKWAIT, 1, 1).initial_end(0, 80, 1, 1);
    b.build()
}

#[test]
fn group_summaries_match_summarize() {
    // The one-pass summaries equal a direct summary of each group's members
    // (which also computes the parallelism), also for nested groups.
    let cases = [(fan_with_dependences(), 1000), (fan_with_dependences(), 16), (creation_trace(&recursion(300)), 16)];
    for (trace, max) in cases {
        let (m, crit) = analyse(&trace);
        let gr = analysis::group_tasks(&m, max);
        let sums = analysis::summarize_groups(&m, &crit, &gr);
        assert_eq!(sums.len(), gr.groups.len());
        for (g, s) in sums.iter().enumerate() {
            let d = analysis::summarize(&m, &crit, &gr.members(g));
            assert!(s.parallelism.is_none() && d.parallelism.is_some());
            assert_eq!((s.n, s.work, s.critical, s.min_work, s.max_work, s.threads), (d.n, d.work, d.critical, d.min_work, d.max_work, d.threads), "group {g}");
            assert_eq!((s.t_begin, s.t_end), (d.t_begin, d.t_end), "group {g}");
            assert_eq!((s.thread, s.cpu, s.numa, s.codeptr, s.parallel, s.kind), (d.thread, d.cpu, d.numa, d.codeptr, d.parallel, d.kind), "group {g}");
            assert_eq!((s.mean_work, s.mean_create, s.mean_start), (d.mean_work, d.mean_create, d.mean_start), "group {g}");
        }
    }
    // Values of the fan group: 10 tasks of 5 each, run from 20 to 70.
    let (m, crit) = analyse(&fan_with_dependences());
    let gr = analysis::group_tasks(&m, 1000);
    let s = &analysis::summarize_groups(&m, &crit, &gr)[0];
    assert_eq!((s.n, s.work, s.min_work, s.max_work, s.t_begin, s.t_end, s.threads), (10, 50, 5, 5, 20, 70, 1));
}

#[test]
fn dependences_follow_the_expanded_groups() {
    let (m, crit) = analyse(&fan_with_dependences());
    let gr = analysis::group_tasks(&m, 1000);
    assert_eq!(gr.groups.len(), 1);
    let (rep, x) = (gr.groups[0].rep, task(&m, 50) as u32);
    assert_eq!(rep as usize, task(&m, 10));
    let pairs = |expanded: &[bool]| analysis::visible_dependences(&m, &crit, &gr, expanded).iter().map(|e| (e.0, e.1)).collect::<Vec<_>>();
    // Collapsed: the edge inside the group disappears, the two edges to X merge.
    assert_eq!(pairs(&[false]), [(rep, x)]);
    // Expanded: the three dependences between the tasks themselves.
    let t = |id| task(&m, id) as u32;
    assert_eq!(pairs(&[true]), [(t(10), t(11)), (t(12), x), (t(13), x)]);
    // Every task is its own node when expanded, the representative otherwise.
    assert_eq!(gr.visible(&[true])[task(&m, 15)], t(15));
    assert_eq!(gr.visible(&[false])[task(&m, 15)], rep);
    assert_eq!(gr.visible(&[false])[x as usize], x);
}

#[test]
fn nested_groups_are_visible_one_level_at_a_time() {
    let m = Model::build(&creation_trace(&recursion(3000)));
    let gr = analysis::group_tasks(&m, 64);
    let top = gr.groups.iter().position(|g| g.parent == crate::model::NONE).unwrap();
    let mut expanded = vec![false; gr.groups.len()];
    // All collapsed: every member is represented by the top group.
    let all = gr.visible(&expanded);
    assert!(gr.members(top).iter().all(|&i| all[i as usize] == gr.groups[top].rep));
    // Top group expanded: its direct tasks are themselves, every other member
    // is represented by the child group it is in.
    expanded[top] = true;
    let v = gr.visible(&expanded);
    assert!(gr.groups[top].tasks.iter().all(|&i| v[i as usize] == i));
    for &c in &gr.groups[top].children {
        assert!(gr.members(c as usize).iter().all(|&i| v[i as usize] == gr.groups[c as usize].rep));
    }
    // Expanding only a child changes nothing while its parent is collapsed.
    let mut inner = vec![false; gr.groups.len()];
    inner[gr.groups[top].children[0] as usize] = true;
    assert_eq!(gr.visible(&inner), all);
}
