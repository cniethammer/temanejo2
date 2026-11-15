The GUI
=======

Layout
------

The side panel on the right holds the summary, the task origins, the task
details and the region analysis. Drag the divider between graph and panel to
change its width (between 260 pixels and 70 % of the window); double-click
the divider to reset it. With the divider focused, the arrow keys change the
width in steps of 20 pixels (80 with Shift). The width is remembered by the
browser. On narrow windows the panel is placed below the graph instead.

Toolbar
-------

MPI rank
   Switches between the task graphs of the processes. Each entry shows rank,
   host and number of tasks.

Graph / Timeline
   Selects the view.

Colour by
   Thread, CPU (hardware thread), NUMA node, source location, parallel
   region, task kind, or work time (sequential, log scale). The first eight
   categories get distinct colours; further ones reuse them, and the task
   details always name the exact value. Thread, CPU and NUMA node refer to the
   task's first execution segment.

Critical path, Creation edges, Dependence edges
   Show or hide these overlays.

Task id, Fit
   Jump to a task by its id; fit the view to the window (key ``F``).

Graph view
----------

Each task is a node: circle for explicit tasks, square for implicit tasks,
diamond for the initial task. Light edges are task creation (parent to child,
including a parallel region's encountering task to the implicit tasks), dark
edges are ``depend`` dependences. Red marks the critical path.

The layout is layered: every edge points downwards. Layers with more tasks
than fit on a line are wrapped over several rows.

Drag to pan, use the wheel to zoom. Task ids are drawn once the zoom is large
enough.

Timeline view
-------------

One row per OpenMP thread, with the execution segments of all tasks over
time. A red strip below a segment marks a critical-path task. Drag to pan,
the wheel zooms the time axis.

Task origins
------------

A table of all source locations that create tasks, with, for each:

* the location (file and line, or function and library without debug
  information) and whether it is a ``task`` construct, a ``taskloop`` or a
  parallel region (whose implicit tasks are counted);
* the number of tasks created there;
* their total work time and its share of T\ :sub:`1`, also drawn as a bar;
* the part of that work on the critical path, and its share of the critical
  path length T\ :sub:`∞`.

The initial task, which holds the serial code outside parallel regions, has
its own row. The last row sums up all tasks. Click a column header to sort,
hover a row for the function name and the average work per task, and click a
row to select all its tasks for the region analysis. With *Colour by: Source
location*, each row shows the colour its tasks have in the graph.

Task details
------------

Clicking a task shows:

* kind and OMPT flags (``undeferred``, ``untied``, ``final``, ...), and badges
  for tasks created by a ``taskloop``, tasks on the critical path, tasks that started on a different thread
  than the one that created them (stolen), tasks that ran on several threads
  (untied tasks resumed elsewhere) and tasks the OS moved between CPUs;
* source file, line and function of the construct, with its address;
* exclusive work time, lifetime, and the part of the work on the critical
  path;
* creating task and thread, creation time and the time until the task first
  ran;
* parallel region, team size and, for implicit tasks, thread number;
* threads, CPUs and NUMA nodes used;
* number of children, predecessors and successors by dependence (clickable);
* the items of the ``depend`` clause;
* every execution segment: thread, CPU (start → end if it changed), NUMA
  node, start and duration.

*Select subtree* selects the task and all tasks created from it, transitively.
*Select parallel region* selects all tasks of the task's parallel region and
everything they create, including nested regions.

Region analysis
---------------

Shift+drag selects the tasks inside a rectangle of the graph view, or the
tasks executing within a time range of the timeline view. For the selection
the panel shows:

* average parallelism T\ :sub:`1` / T\ :sub:`∞`;
* number of tasks and the wall-clock span of their execution;
* work T\ :sub:`1` and critical path T\ :sub:`∞` of the selection;
* measured average and peak number of selected tasks running at the same
  time;
* a chart of that number over time (hover for values);
* the work done by each thread.

Tasks on the selection's own critical path get a thick outline. The
quantities are defined in :doc:`metrics`.

Links
-----

The state can be preset through the URL fragment, e.g.::

   http://127.0.0.1:8700/#rank=1&view=timeline&color=loc&task=1306&select=subtree

``rank``
   rank to show
``view``
   ``graph`` or ``timeline``
``color``
   ``thread``, ``cpu``, ``numa``, ``loc``, ``region``, ``kind`` or ``work``
``task``
   task id to select and centre
``select``
   ``subtree`` or ``region``: select relative to ``task``
