HTTP interface
==============

The server answers the following requests; the GUI uses nothing else, so
other front ends or scripts can use the same data. All responses are JSON
except the GUI files. Times are integers in nanoseconds, relative to the
first event of the rank. Task, region and thread references are **indices**
into the arrays of the same response, -1 meaning none, unless a field is
called ``id``.

The server has no authentication and binds to ``127.0.0.1`` by default.

``GET /``, ``/app.js``, ``/style.css``
   The GUI.

``GET /api/info``
   The loaded run: ``dir`` (absolute path of the trace directory), ``name``
   (its last component), ``recorded_ms`` (when the tool started in the
   earliest rank, milliseconds since the Unix epoch, 0 if unknown) and
   ``ranks`` (number of loaded ranks).

``GET /api/ranks``
   One summary object per rank:

   ============ ======================================================
   ``rank``     MPI rank
   ``host``     host name
   ``pid``      process id
   ``tasks``    number of tasks
   ``threads``  number of threads
   ``events``   number of events in the trace
   ``wall``     time from first to last event
   ``work``     total work time T\ :sub:`1`
   ``critical`` critical path length T\ :sub:`∞`
   ============ ======================================================

``GET /api/rank/<rank>/graph``
   The analysed task graph of one rank **with all groups collapsed**; this
   is what the GUI loads first. The groups are described in :doc:`gui`,
   section "Collapsing groups". Top-level fields: ``rank``, ``host``,
   ``pid``, ``wall``, ``work``, ``critical`` as above, ``peak`` (the largest
   number of tasks executing at the same time), ``threads``, ``locs``,
   ``parallels`` and ``origins`` as in the full graph below, and:

   ``info``
      Optional information about the run: ``[key, value]`` pairs from the
      trace header (see :doc:`trace-format`, "Run information"), plus
      ``omp.runtime_library``, the path of the OpenMP runtime library. May
      be empty; also part of ``/full``.
   ``n``
      Number of tasks. Task indices run from 0 to ``n`` − 1.
   ``ids``, ``par``
      For every task its id and the index of its creating task (-1 for
      none). Creation edges are exactly this parent relation and are not
      listed as edges.
   ``crit``
      ``[task, work on the critical path]`` for the tasks on it.
   ``cc``
      Tasks whose creation edge the critical path follows from the parent to
      the task.
   ``cj``
      ``[from, to]`` task pairs: the steps of the critical path that follow
      no creation or dependence edge forwards, *joins*: a task returning to
      the parent waiting for it (``taskwait``, ``taskgroup``, barrier, end of
      a parallel region) and the step between two tasks of a team at a
      barrier. Also in the full graph document.
   ``cats``
      The values occurring in each colour category over all tasks:
      ``thread``, ``cpu``, ``numa`` (of the first execution segment; thread:
      the creating thread if the task never ran, CPU and NUMA: -1), ``loc``,
      ``region``, ``kind``.
   ``ranges``
      ``[min, max]`` over all tasks of ``work``, ``create``, ``start`` and
      ``wait`` (start − create).
   ``tasks``
      Records of the tasks outside groups, as in the full graph, without
      ``x``/``y`` and with ``i``, the task's index, the dependence lists
      ``dp`` and ``ds`` and their variables ``dpv`` and ``dsv`` (see
      ``/group`` below).
   ``max_group``
      The size limit the groups were made for.
   ``groups``
      The tree of groups: ``{kind, anchor, loc, parent, rep, part, sum,
      tasks, sub}``. ``kind`` is ``taskloop``, ``fan`` (sibling tasks from
      one source location) or ``subtree``; ``anchor`` the creating task or
      subtree root; ``parent`` the enclosing group (-1 at the top level);
      ``rep`` the first-created task of the group, which stands for it in
      merged edges; ``part`` is ``[first, count, total]`` for a part of a
      larger sibling group, else null; ``tasks`` the tasks shown when the
      group is expanded and ``sub`` its child groups, together at most
      ``max_group``; ``sum`` a summary as returned by ``/summary`` below,
      over all tasks of the group including its child groups, with
      ``own_cr`` and ``peak`` null. The index in this array is the group id.
      Every task outside ``tasks`` at the top level is in the ``tasks`` of
      exactly one group.

   With the query ``?max_group=N`` (N ≥ 16) the server first regroups the
   rank for that limit; group ids change, and the new grouping stays in
   effect for all later requests.

``GET /api/rank/<rank>/group/<group>``
   ``tasks``: the records of the tasks the group shows when expanded (its
   direct tasks, with ``i`` and the dependence lists ``dp``, tasks it
   depends on, and ``ds``, tasks depending on it). ``dpv`` and ``dsv`` give
   for each entry of ``dp`` and ``ds`` the variables that order the two
   tasks, as a list of ``[address (hex string), class, predecessor's type,
   successor's type]``. ``class`` is one bit: 1 read after write (RAW), 2
   write after read (WAR), 4 write after write (WAW); the types are OMPT
   dependence types (``ompt_dependence_type_t``). The list is empty if the
   two ``depend`` clauses share no address.

``GET /api/rank/<rank>/group/<group>/stats``
   ``own_cr``, the critical path through all tasks of the group alone, and
   ``peak``, the largest number of them executing at the same time. Computed
   on request with a pass over the graph.

``POST /api/rank/<rank>/timeline``
   What the timeline shows for a time range. Request body:
   ``t0=<ns>&t1=<ns>&cols=<pixel columns>&mode=<colour mode>``, the mode
   being one of the GUI's colour modes (``thread``, ``cpu``, ``numa``,
   ``loc``, ``region``, ``kind``, ``work``, ``create``, ``start``,
   ``wait``). The response echoes ``t0``, ``t1`` and ``cols`` and has one of
   two forms:

   ``kind: "segs"``
      At most 4,000 segments touch the range. ``items`` lists them as
      ``[thread, start, end, task, value, flags]``.
   ``kind: "bins"``
      More segments (``segments`` gives their number). ``rows`` has per
      thread ``{thread, bins}``, with ``[column, busy, task, value, flags]``
      for every column in which the thread executed anything: ``busy`` is
      the fraction of the column's time, ``task`` the task taking most of
      it.

   ``value`` is the task's value in the colour mode (a category value as in
   ``cats`` of the graph, or a time in ns). ``flags``: 1 the task is on the
   critical path, 2 it is in the region analysed last (see ``/region``); for
   a bin, the flags of all tasks in the column combined.

   ``gaps`` is the critical path strip for the same range (see
   :doc:`metrics`, "Critical path gaps"): ``total``, the time per class over
   the whole run (class 0 on the critical path, 1 gap with all threads busy,
   2 gap with a critical task waiting, 3 gap with threads idle), and either
   ``iv``, the intervals in the range as ``[begin, end, class]`` if there
   are at most 4,000, or ``cls``, the class taking most of each pixel
   column (-1 outside the run).

``POST /api/rank/<rank>/timeline-select``
   The tasks with an execution segment overlapping a time range. Request
   body: ``t0=<ns>&t1=<ns>``. Response: ``tasks``, their indices in
   ascending order.

``POST /api/rank/<rank>/view``
   The dependence edges between the nodes that are visible when certain
   groups are expanded. Request body: the ids of the expanded groups,
   separated by commas (empty: all collapsed). Response: ``deps``, a list of
   ``[from, to, critical, classes]``; ``classes`` is a bit set of the
   dependence types (1 RAW, 2 WAR, 4 WAW, 8 no shared address) of all edges
   merged into this one. A task whose enclosing groups are all expanded
   is its own node; any other task is represented by the ``rep`` of the
   outermost collapsed group around it. Edges between the same two nodes
   are merged, edges inside one node dropped.

``POST /api/rank/<rank>/summary``
   Summary of a set of tasks. Request body: task indices separated by
   commas. Response: ``n`` tasks, ``w`` total work, ``cr`` work on the
   critical path, ``min``/``max`` task work, ``t0``/``t1`` first segment
   start and last segment end, ``threads`` used, ``own_cr`` critical path
   through these tasks alone, ``peak`` concurrency, and ``typ``, a typical
   task with the keys of a task record (most frequent kind, location,
   region, thread, CPU and NUMA node; mean creation time, start and work).

``GET /api/rank/<rank>/full``
   The complete graph of one rank in one document, with a layout computed
   by the server. It is built on request and can be large (about 230 bytes
   per task). Top-level fields: ``rank``, ``host``, ``pid``, ``wall``,
   ``work``, ``critical``, ``peak``, and these arrays:

   ``threads``
      ``{id, tid, type}``: tool thread id, OS thread id, OMPT thread type.

   ``locs``
      Source locations: ``{addr, module, func, file, line}``. ``addr`` is a
      hexadecimal string; ``file`` is empty and ``line`` 0 without debug
      information.

   ``parallels``
      ``{id, parent, requested, actual, loc, begin, end}``: region id,
      encountering task (index), requested and actual team size, source
      location (index into ``locs``), begin and end time.

   ``tasks``
      One object per task. Index = position in this array; tasks appear in
      creation order.

      ======== ===========================================================
      ``i``    index of the task
      ``id``   tool task id (as in the trace)
      ``k``    kind: 0 initial, 1 implicit, 2 explicit
      ``fl``   OMPT task flags
      ``par``  creating (or encountering) task
      ``pr``   parallel region
      ``ix``   thread number in the team (implicit tasks)
      ``ct``   creation time
      ``cth``  thread that created the task
      ``loc``  source location
      ``e``    completion time
      ``w``    exclusive work time
      ``cr``   work time on the critical path (0 if not on it)
      ``tl``   1 if the task was created by a ``taskloop``, else 0
      ``x``    layout position, horizontal (grid units; ``/full`` only)
      ``y``    layout row (``/full`` only)
      ``segs`` execution segments: ``[thread, cpu, cpu_end, numa, start,
               end]``
      ``deps`` ``depend`` items: ``[address (hex string), type]``
      ======== ===========================================================

   ``origins``
      Task origins, as in the GUI's task origins table, sorted by work:
      ``{initial, loc, k, n, w, cr, own_cr, peak, tasks}``. ``initial`` is 1 for the group
      of the initial task (``loc`` is then -1); otherwise ``loc`` is the
      source location of the creating construct. ``k`` is a bit set of the
      kinds in the group: 1 implicit tasks of a parallel region, 2 tasks of
      a ``task`` construct, 4 tasks of a ``taskloop``. ``n`` is the number
      of tasks, ``w`` their total work, ``cr`` their work on the critical
      path, ``own_cr`` the critical path through these tasks alone
      (``w / own_cr`` is their average parallelism), ``peak`` the largest
      number of them executing at the same time, ``tasks`` their indices. The groups partition the tasks, so the
      sums of ``w`` and ``cr`` are ``work`` and ``critical``.

   ``edges``
      ``[from, to, kind, critical]``: task indices; kind 0 creation, 1
      dependence; critical 1 if the critical path follows the edge from
      ``from`` to ``to`` (a return of the path to a waiting task is listed
      in ``cj``, not here). Dependence edges have a fifth element, the variables that
      order the two tasks, in the format of ``dpv`` (see ``/group``).

``POST /api/rank/<rank>/region``
   (The server remembers the task set of the last request per rank, to
   flag its tasks in ``/timeline`` answers; an empty body clears it.)

   Region analysis. Request body: task indices separated by commas, e.g.
   ``0,5,6,7``. Response:

   ============== ======================================================
   ``n``          number of tasks
   ``work``       T\ :sub:`1` of the selection
   ``critical``   T\ :sub:`∞` of the selection
   ``t_begin``    start of the first execution segment
   ``t_end``      end of the last execution segment
   ``max``        peak number of selected tasks running concurrently
   ``profile``    ``[time, average concurrency]`` for 240 equal buckets
   ``bucket``     bucket width
   ``per_thread`` ``[thread, work time]``
   ``crit_tasks`` ``[task, work on the selection's critical path]``
   ``crit_edges`` ``[from, to]`` pairs of tasks the selection's critical
                  path passes between along a creation or dependence
                  edge
   ``crit_joins`` ``[from, to]`` its other steps (joins, see ``cj``)
   ============== ======================================================

Unknown paths and ranks return status 404.
