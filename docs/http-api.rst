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
   The analysed task graph of one rank. Top-level fields: ``rank``,
   ``host``, ``pid``, ``wall``, ``work``, ``critical`` as above, and these
   arrays:

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
      ``x``    layout position, horizontal (grid units)
      ``y``    layout row
      ``segs`` execution segments: ``[thread, cpu, cpu_end, numa, start,
               end]``
      ``deps`` ``depend`` items: ``[address (hex string), type]``
      ======== ===========================================================

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
