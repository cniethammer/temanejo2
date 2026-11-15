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
      Tasks whose creation edge is on the critical path.
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
      ``x``/``y`` and with ``i``, the task's index.
   ``groups``
      ``{kind, anchor, loc, top, sum, members}``: ``kind`` is ``taskloop``,
      ``fan`` (sibling tasks from one source location) or ``subtree``;
      ``anchor`` the creating task or subtree root; ``top`` is 1 for a group
      not nested in another; ``members`` the task indices in ascending
      order; ``sum`` a summary as returned by ``/summary`` below. The index
      in this array is the group id.
   ``edges``
      Dependence edges ``[from, to, 1, critical]`` between tasks outside
      groups.
   ``pre``
      Dependence edges into, out of and between outermost groups, merged
      per pair of end points: ``[from, to, 1, critical, from group, to
      group]``. A group is represented by its first member; the group ids
      are -1 where the end point is a task outside groups.

``GET /api/rank/<rank>/group/<group>``
   The content of a group: ``tasks``, the records of its members (with
   ``i``), and ``edges``, all dependence edges ``[from, to, 1, critical]``
   with at least one end point in the group. An edge between two groups is
   part of both groups' answers.

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
      dependence; critical 1 if the critical path passes from one task to
      the other.

``POST /api/rank/<rank>/region``
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
   ``crit_edges`` ``[task, task]`` pairs the selection's critical path
                  passes between
   ============== ======================================================

Unknown paths and ranks return status 404.
