Metrics reference
=================

This chapter defines every number the summary table and the GUI show, and how
it is computed from the trace. The graph model behind the critical path is
described in :doc:`analysis`.

Time base and notation
----------------------

* Every event carries a ``CLOCK_MONOTONIC`` timestamp in nanoseconds, taken
  inside the OMPT callback. All times are shown relative to the first event
  of the rank, so the first event is at 0.
* *Segment*: an interval [*s*, *e*) in which a task is the task a thread
  executes (see :doc:`analysis`). A task has one or more segments.
* *w(t)*: work time of task *t*.
* Σ runs over all tasks of the rank, or of the selection for region metrics.
* T\ :sub:`1`, T\ :sub:`∞`: work and critical path length, following the
  usual notation of the work–span model.

Per-task metrics
----------------

Shown in the *Task* panel of the GUI.

Work time
   *w(t)* = Σ (*e* − *s*) over the task's segments.

   Exclusive: time in which the thread executes another task (a child that
   runs immediately, a task picked up at a scheduling point) is not counted,
   nor is time waiting in ``taskwait``, at the end of a ``taskgroup`` or in a
   barrier. It does include time the task spends in non-OpenMP calls, such as
   MPI, I/O or library calls.

   A task that never completes, for instance the initial task when the
   runtime shuts down, is closed at the last event of the rank.

Segments
   The number of segments. More than one means the task was suspended and
   resumed: it created an undeferred or included child, waited in a
   synchronisation construct, or was switched out at a scheduling point.

Lifetime
   From the start of the first segment to the completion time. The
   completion time is the time of the ``task_schedule`` event with status
   *complete* (or the end of the implicit task), or the end of the last
   segment if that is later. Lifetime − work time is the time the task was
   suspended or waiting.

Waited (explicit tasks)
   Start of the first segment − creation time: how long the task was queued
   before a thread picked it up. Large values with idle threads point to a
   dependence, scheduling or task-throttling problem.

On critical path
   The part of *w(t)* that lies on the critical path: the sum of the weights
   of the task's parts that are on the path. Also given as a percentage of
   T\ :sub:`∞`. A task can be partly on the path, e.g. a parent whose work up
   to a ``taskwait`` is critical but whose work after it is not.

Created
   Creation time, creating task and the thread it ran on, from the
   ``task_create`` event. For implicit tasks the "creating" task is the task
   that encountered the parallel construct.

Threads, CPUs, NUMA
   The distinct tool thread ids, logical CPUs and NUMA nodes of all
   segments. The CPU is sampled with ``getcpu(2)`` at the start and at the
   end of each segment; the NUMA node at the start.

Badges
   ``ran on another thread than its creator``
      Explicit task whose first segment is on a different thread than the
      one that created it: the task was stolen, or taken from a shared queue.
   ``migrated between threads``
      Segments on more than one thread; only possible for untied tasks.
   ``moved between CPUs``
      Segments with more than one distinct CPU: the operating system
      migrated the executing thread, which happens when threads are not
      pinned (``OMP_PROC_BIND``, ``OMP_PLACES``).

Per-origin metrics
------------------

Shown in the *Task origins* table. Tasks are grouped by their origin: the
code address of the construct that created them, i.e. the ``codeptr_ra`` of
``task_create`` for explicit tasks and of ``parallel_begin`` for the implicit
tasks of a region. The initial task forms a group of its own. For a group
*G*:

Tasks
   \|\ *G*\ \|, the number of tasks in the group.

Work
   Σ\ :sub:`t∈G` *w(t)*, and its share of T\ :sub:`1` in percent.

Crit.
   Σ\ :sub:`t∈G` of each task's work on the critical path, and its share of
   T\ :sub:`∞` in percent. The column sums to T\ :sub:`∞` (100 %).

Avg.
   Average work per task: Work / Tasks. A small average with many tasks
   points to tasks too fine-grained for the overhead of creating and
   scheduling them.

Par.
   Average parallelism of the group's tasks on their own:
   Work / T\ :sub:`∞,G`, where T\ :sub:`∞,G` is the critical path through the
   group's tasks only, computed as for a selected region (see `Region
   metrics`_). It answers how many threads these tasks alone could keep
   busy.

Peak
   The largest number of the group's tasks that were executing at the same
   time, computed as the measured peak concurrency of a region.

   Compare *Par.* and *Peak*: an average parallelism far above the peak means the
   tasks would allow more parallelism than the run achieved, e.g. because
   there were not more threads (the peak cannot exceed the thread count).
   An average parallelism close to 1 means the tasks form a chain.
   The *all tasks* row shows T\ :sub:`1` / T\ :sub:`∞` and the peak of the
   whole graph.

Every task belongs to exactly one group, so the rows add up to the totals of
the process.

The location shown is only as good as the address the runtime reports.
LLVM ``libomp`` 23 reports an address inside the runtime for tasks created by
``taskloop``; the tool replaces it with the address of the ``taskloop``
construct, so these tasks appear under the construct's line with the kind
*taskloop* (their row includes internal helper tasks of the runtime).
Constructs whose runtime call was compiled as a tail call, e.g. a parallel
region that ends the body of another one, are reported with an address in
the runtime (``__kmp_invoke_microtask (libomp.so)``); all such tasks are
grouped into one row, even if they come from different constructs, unless
the program is built with ``-fno-optimize-sibling-calls``. Details are in
:doc:`runtime-notes`.

Per-process metrics
-------------------

Shown in the summary table of ``temanejo`` and the *Process* panel.

Wall time
   Timestamp of the last event − timestamp of the first event of the rank.
   This covers the OpenMP part of the program, from the runtime's
   initialisation to its shutdown, not the full process lifetime.

Work T\ :sub:`1`
   T\ :sub:`1` = Σ *w(t)* over all tasks, including the initial task and the
   implicit tasks. It is the time a single thread would need to run all
   tasks at the measured speed, without runtime overhead.

Critical path T\ :sub:`∞`
   Length of the longest path through the part graph, with each part
   weighted by the work it did (see :doc:`analysis`). It is the shortest
   possible run time with unlimited threads, measured task durations and no
   runtime overhead. Serial code in the initial task outside parallel
   regions is always on it.

Average parallelism
   T\ :sub:`1` / T\ :sub:`∞`. The number of threads the task graph can keep
   busy on average, and an upper bound on the speedup over a serial run
   with the same task durations. It is a property of the graph, independent
   of how many threads were used.

Measured average concurrency
   T\ :sub:`1` / wall time: the number of threads that, on average, were
   executing a task. At most the number of threads.

Tasks, threads
   Number of tasks of all three kinds; number of threads that recorded
   events, including threads of nested teams.

Region metrics
--------------

Computed by the server for a selection of tasks (*Selected region* panel).

What is selected
   *Graph view*: the tasks whose node centres lie in the rectangle.
   *Timeline view*: the tasks with at least one segment overlapping the time
   range. Whole tasks are selected; their work outside the range is
   included in all region metrics.
   *Select subtree*: the task and all tasks created from it, transitively.
   *Select parallel region*: all tasks belonging to the region and
   everything they create, including nested regions.

Work T\ :sub:`1`
   Σ *w(t)* over the selected tasks.

Critical path T\ :sub:`∞`
   Longest path through the part graph restricted to the parts of the
   selected tasks and the weightless join nodes. Join nodes are kept so that
   selected tasks stay ordered across barriers and region ends even if the
   tasks connecting them are not selected. Tasks on this path get a thick
   outline in the graph.

Average parallelism
   T\ :sub:`1` / T\ :sub:`∞` of the selection.

Wall-clock span
   From the earliest segment start to the latest segment end among the
   selected tasks.

Measured average concurrency
   T\ :sub:`1` / span.

Measured peak concurrency
   The largest number of selected tasks executing at the same instant.
   Computed by sorting all segment starts (+1) and ends (−1) and taking the
   maximum of the running sum. At equal timestamps ends are processed before
   starts, so back-to-back segments on one thread do not count twice.

Concurrency over time (chart)
   The same step function *c(τ)*, number of selected tasks executing at time
   *τ*, averaged over 240 equal buckets of the span:
   value of bucket *i* = (1 / bucket width) · ∫ *c(τ)* d\ *τ* over the bucket.
   Short bursts are therefore smoothed; the peak in the chart can be below
   the measured peak.

Work per thread
   Σ (*e* − *s*) of the selected tasks' segments on each thread, and its
   share of the selection's total. An uneven distribution with low
   concurrency indicates load imbalance; an even one with low concurrency
   indicates threads that were idle in turns.

Critical path gaps
------------------

The critical path covers only part of the wall time; in between, the
threads execute other tasks or wait. The strip in the timeline classifies
every moment of the run (from the first to the last event):

*on the critical path*
   A task with work on the critical path executes.
*gap, threads busy*
   No critical task executes, and the number of executing tasks is at
   least the number of available threads. A thread is available from the
   start of its first to the end of its last execution segment.
*gap, critical task waiting*
   No critical task executes, fewer tasks than available threads execute,
   and at least one critical task is *ready*: it was created, and all its
   dependence predecessors have completed, but it has not started yet.
*gap, idle*
   No critical task executes, fewer tasks than threads execute, and no
   critical task is ready. This covers runtime overhead between tasks, and
   time in which the next critical task cannot start yet for a reason the
   trace does not record as a task, e.g. a thread finishing a ``taskwait``
   or a barrier.

The legend shows the share of each class in the wall time. In the example
trace (rank 1, 4 threads) the critical path runs 36.9 % of the time; the
gaps are 54.0 % busy, 1.4 % waiting and 7.6 % idle, so the run is mainly
limited by the amount of work, not by scheduling.

Limitations: readiness only considers creation and ``depend`` clauses, not
locks or other synchronisation inside tasks; threads of nested teams count
as available over their whole lifetime, even while their team does not
exist.

Reading the numbers together
----------------------------

With *p* threads, the work–span model bounds the run time T\ :sub:`p` by

   max(T\ :sub:`1` / *p*, T\ :sub:`∞`)  ≤  T\ :sub:`p`

and a greedy scheduler achieves T\ :sub:`p` ≤ T\ :sub:`1` / *p* +
T\ :sub:`∞`. Comparing the measured wall time with this bound shows where
time is lost:

* **T₁ / T∞ below p**: the graph does not have enough parallelism for the
  thread count. Look at the critical path: long tasks on it, or chains of
  dependences.
* **T₁ / T∞ well above p, but measured concurrency well below p**: the
  parallelism is there but not used: threads idle in barriers or
  ``taskwait``, tasks waiting long before they start, scheduling overhead,
  or serial code between parallel regions. Use the timeline and the
  *waited* times.
* **Measured concurrency close to p**: threads are busy; further gains must
  come from making tasks faster (locality, NUMA placement) or reducing work.

Example, rank 0 of the example program with 4 threads::

   tasks   wall[ms]   work[ms]   crit.path[ms]   avg.parallelism
   12037     412.34     704.42           78.92              8.93

* Average parallelism 8.93 > 4: the graph could keep more than four threads
  busy.
* Measured concurrency 704.42 / 412.34 = 1.71 of 4 threads: 43 % of the
  thread time was spent executing tasks.
* Lower bound for the run time: max(704.42 / 4, 78.92) = 176.1 ms against
  412.3 ms measured, so most of the lost time is in scheduling, waiting and
  serial sections, not in the structure of the graph.

Accuracy and caveats
--------------------

* Time between tasks that belongs to no task (the runtime looking for work,
  idle threads, task creation inside the runtime before the creating task
  resumes) is not counted as work. It lowers the measured concurrency but
  not T\ :sub:`1` or T\ :sub:`∞`.
* Every event costs the tool a ``clock_gettime`` and a ``getcpu`` call plus
  a memory write. This has not been measured yet; both calls are normally
  served by the vDSO and expected to take tens of nanoseconds. For tasks of a
  few microseconds the overhead can be a noticeable part of the measured
  work.
* Time blocked in MPI calls counts as work of the calling task, and
  therefore can be on the critical path. Serial MPI calls such as
  ``MPI_Finalize`` belong to the initial task and can dominate T\ :sub:`∞`;
  see :ref:`mpi-time`.
* Time in which the operating system switched a thread out also counts as
  work of the task it was executing. If more threads run than cores are
  available, e.g. with Open MPI's default binding of one core per process,
  work times and T\ :sub:`∞` are inflated; see :ref:`binding`.
* T\ :sub:`∞` uses the measured durations of this run. With another thread
  count, placement or input the durations, and so the critical path, can
  change.
* Per-rank metrics only: ranks are analysed independently, and clocks of
  different ranks are not aligned.
