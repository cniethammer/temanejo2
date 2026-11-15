How the numbers are computed
============================

All times are measured with ``CLOCK_MONOTONIC`` inside the OMPT callbacks and
shown relative to the first event of the process.

Execution segments and work time
--------------------------------

A task runs in one or more *execution segments*: intervals during which it is
the task a thread executes. A segment starts when the task is scheduled on a
thread (``task_schedule`` with the task as *next*, or the begin of an implicit
task) and ends when the thread switches away, when the task completes, or when
it starts waiting in a ``taskwait``, at the end of a ``taskgroup`` or in a
barrier (``sync_region_wait``). It resumes when the wait ends.

The **work time** of a task is the sum of its segment lengths. It is
exclusive: time spent executing other tasks on the same thread in between,
and time spent waiting, is not counted.

For each segment the CPU and NUMA node at its start and the CPU at its end are
recorded, so the GUI can show

* *stealing*: an explicit task whose first segment is on a different thread
  than the one that created it;
* *migration between threads*: an untied task with segments on several
  threads;
* *migration between CPUs*: the operating system moving a thread, visible as
  a change of CPU within or between segments.

Time spent inside MPI calls is not distinguished: it counts as work of the
task that made the call.

The part graph
--------------

To find the critical path, the analysis needs a graph whose edges say "this
work can only start after that work has finished", and whose nodes carry the
measured work time. The task graph shown in the GUI is not such a graph.
Consider this code:

.. code-block:: c

   // task P
   x = prepare();            // work before the children
   #pragma omp task          // child A
   a = f(x);
   #pragma omp task          // child B
   b = g(x);
   #pragma omp taskwait
   result = combine(a, b);   // work after the taskwait

There are two orderings:

* A and B can only start after P has created them, so P comes before A and
  B.
* ``combine`` can only run after A and B have finished, so A and B come
  before P.

With one node per task, this needs the edges P → A and A → P, a cycle. A
cycle has no longest path, so no critical path can be computed. It would also
be wrong: the edges would say that *all* of P's work comes before A, and
*all* of it after A, although only ``prepare`` comes before and only
``combine`` after.

The analysis therefore splits P into **parts** at the points where something
happens that orders it against other tasks. Here these points are the two
task creations and the end of the ``taskwait``::

   P1 (prepare) --> P2 --> P3 --> P4 (combine)
     |              |              ^
     |              +--> B --------+
     +----------------> A ---------+

``P1`` holds the work up to creating A; ``P2`` the work between creating A
and creating B; ``P3`` the work after creating B until the ``taskwait``
begins; ``P4`` the work after the ``taskwait``. Now every edge points forward
in time, the graph has no cycle, and the longest path is correct: the
``combine`` part starts only after the longer of A and B, and only the work
of ``prepare`` lies before both children.

The same problem arises for every construct that makes a task wait for
others, so tasks are cut at each of these points:

* creation of a child task;
* begin and end of a parallel region the task encounters (the region's
  implicit tasks must run between them);
* end of a ``taskwait``, end of a ``taskgroup``, and each barrier.

Each part is weighted with the work time the task did between its two cuts.
Parts are connected by these edges:

=================== ==========================================================
Sequence            each part of a task to its next part
Creation            the creating part of the parent to the child's first part
Fork                the part before a parallel region to the first part of
                    every implicit task of its team
Dependence          the last part of a predecessor to the first part of its
                    successor (``depend`` clauses)
Taskwait            the last part of each child created since the previous
                    ``taskwait`` to the part after the ``taskwait``
Taskgroup           the last part of each task created in the group, including
                    descendants, to the part after the group ends
Barrier             through a weightless join node: the parts before the
                    *k*-th barrier of every implicit task of a team, and every
                    explicit task of the region completed when the barrier is
                    released, to the join; the join to the parts after the
                    barrier
Region end          every implicit task, and every explicit task of the region
                    not yet joined, to a join node; the join to the
                    encountering task's part after the region
=================== ==========================================================

The result is a directed acyclic graph. It is built in
``crates/temanejo/src/model.rs``.

Critical path
-------------

The critical path is the path through the part graph with the largest sum of
part weights, found with a topological sort in linear time
(``crates/temanejo/src/analysis.rs``). Its length is T\ :sub:`∞`: the shortest
time the program could take with unlimited threads and the measured task
durations. A task is *on the critical path* if one of its parts is; the GUI
reports how much of its work lies on the path.

T\ :sub:`1` is the total work time of all tasks, and **T₁ / T∞** is the
average parallelism: how many threads the graph could keep busy on average.
It is independent of the number of threads actually used.

Because the weights are measured, contention and data locality of the actual
run are included: a task slowed down by a remote NUMA access is longer on the
critical path too.

Region analysis
---------------

For a selected set of tasks the same longest path computation runs on the
parts of the selected tasks plus the join nodes; join nodes are kept so that
selected tasks remain ordered across barriers and region ends.

The measured concurrency is computed from the execution segments of the
selected tasks: a step function of how many of them run at each instant. The
GUI shows its maximum, its average over the wall-clock span of the selection
(T\ :sub:`1` divided by the span), and a chart averaged over 240 time buckets.

Comparing T\ :sub:`1`/T\ :sub:`∞` with the measured average concurrency tells
whether a region was limited by its dependence structure (both low) or by
scheduling, too few threads or overhead (T\ :sub:`1`/T\ :sub:`∞` high,
measured concurrency low).

Limitations
-----------

* Barrier joins assume that all threads of a team pass the same sequence of
  barriers, which does not hold after cancellation.
* ``detach`` clauses, target tasks and the pseudo tasks of
  ``taskwait depend(...)`` are recorded but not treated specially.
* Clocks of different ranks are not aligned, and MPI communication is not
  part of the graph: each rank is analysed on its own.
