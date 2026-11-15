The GUI
=======

Layout
------

The side panel on the right holds the summary, the task origins, the task
details and the region analysis. Drag the divider between graph and panel to
change its width (between 260 pixels and 70 % of the window); double-click
the divider to reset it to the default of 400 pixels. With the divider focused, the arrow keys change the
width in steps of 20 pixels (80 with Shift). The width is remembered by the
browser. On narrow windows the panel is placed below the graph instead.

Keyboard
--------

:kbd:`F`
   Fit the view to the window.
:kbd:`Esc`
   Clear the region selection and the selected task; while drawing a
   selection rectangle, cancel it.
:kbd:`P`
   Show or hide the frame time display, to find out which settings make the
   GUI slow on a given machine. It shows the measured time between frames
   for *full redraws* (with the JavaScript part of the last one in
   brackets; the rest is the browser turning the drawing into pixels) and
   for *move/overlay* frames. The values are meaningful while the view
   changes continuously, e.g. during panning; for hovering they are an
   upper bound.
Arrow keys
   With the divider between graph and side panel focused: change the panel
   width.

Toolbar
-------

Trace name
   Next to the title: the name of the trace directory and when the run was
   recorded (the earliest start of the tool among the ranks, in the
   browser's local time). Hovering it shows the full path and the number of
   ranks. The directory name is also the title of the browser tab.

MPI rank
   Switches between the task graphs of the processes. Each entry shows rank,
   host and number of tasks.

Graph / Timeline
   Selects the view.

Colour by
   Thread, CPU (hardware thread), NUMA node, source location, parallel
   region or task kind (categorical colours), or one of these sequential
   scales (light = small or early, dark = large or late):

   * *Work time*: exclusive work time, log scale.
   * *Create time*: when the task was created, linear from the first to the
     last creation.
   * *Start time*: when the task started, linear. A task's start time is the
     begin of its first execution segment, or its creation time if it never
     ran.
   * *Wait time*: start time − create time, how long the task waited before
     a thread picked it up, log scale. Implicit tasks and undeferred tasks
     start right away. Long waits next to idle threads point to dependences
     or scheduling holding tasks back.

   Of the categorical colours, the first eight categories get distinct
   colours; further ones reuse them, and the task details always name the
   exact value. Thread, CPU and NUMA node refer to the task's first
   execution segment.

Critical path, Creation edges, Dependence edges
   Show or hide these overlays.

Grey out unselected
   While a region is selected (see `Region analysis`_), draw all tasks
   outside it in grey, without critical path marks, and all edges that do
   not connect two selected tasks in light grey. The selected tasks keep
   their colours, which matters for the sequential colour modes. On by
   default. When it is off, selected tasks are marked in black instead: a
   ring, or a bar below the node when zoomed out, and a bar above each
   segment in the timeline.

Collapse groups, Expand all
   Collapse all detected task groups into one node each (the state the GUI
   opens in), or expand all groups; see `Collapsing groups`_.

Task id, Fit
   Jump to a task by its id; fit the view to the window (key ``F``).

Graph view
----------

Each task is a node: circle for explicit tasks, square for implicit tasks,
diamond for the initial task. Light edges are task creation (parent to child,
including a parallel region's encountering task to the implicit tasks), dark
edges are ``depend`` dependences. Red marks the critical path.

Tasks that create many children (8 or more, e.g. the task running a
``taskloop`` or a ``single`` that spawns all tasks of a phase) have their
creation edges drawn as a *bus*: a short trunk below the parent, a
horizontal line above each row of children, and a stub down to each child.
This keeps thousands of long crossing lines from covering the graph and
from slowing down drawing. Creation edges on the critical path are still
drawn as direct red lines.

The layout is layered: every edge points downwards (except between
collapsed groups that depend on each other). Layers with more tasks
than fit on a line are wrapped over several rows.

Drag to pan, use the wheel to zoom. While the view moves, the picture
already drawn is shifted and scaled, so it can look blurry or leave the
border empty for a moment; it is redrawn sharp when the movement pauses for
150 ms or the mouse button is released. Hovering and clicking a task only
redraw the marks on top of the picture. Task ids are drawn once the zoom is large
enough.

Collapsing groups
-----------------

Large sets of similar tasks are shown as a single node, and the GUI opens
with all of them collapsed. Collapsing only changes what is drawn: all
metrics, the task origins table and the region analysis are always computed
on the full graph.

The server detects these groups:

taskloop
   The tasks one task created for a ``taskloop`` construct (including the
   runtime's helper tasks, see :doc:`runtime-notes`).
tasks from one source line
   Eight or more sibling tasks created by the same task at the same source
   location, e.g. a ``task`` construct inside a loop.
subtree
   All tasks created below an explicit task, directly or indirectly, if
   there are at least 32 of them, e.g. a recursion. The topmost such task is
   taken, and it stays visible above its group.

The first two include the tasks their members create, so groups can be
nested. In addition, *Collapse subtree* in the task details collapses
everything below any task, e.g. a whole parallel region.

A collapsed group is drawn as a stack of cards, wider the more tasks it
holds, with the number of tasks (``×2300``) below it. Its colour is that of
its typical task in the current colour mode (most frequent thread, CPU,
source location ..., mean times), and it has a red outline if any of its
tasks is on the critical path. All edges between two nodes are merged into
one line. The graph is laid out again whenever groups are collapsed or
expanded.

A click on a group node expands it; *Collapse its group* in the details of
one of its tasks collapses it again. *Expand all* in the toolbar expands
everything, *Collapse groups* collapses all detected groups. Shift+click on
a group node selects its tasks for the region analysis, as does a selection
rectangle around it.

Hovering a group node shows its kind and source location, the number of
tasks, their total work with the share of T\ :sub:`1`, the average, shortest
and longest task, when and on how many threads they ran, their average
parallelism and peak (as in the task origins table) and their work on the
critical path.

**Loading on demand.** To keep large traces manageable, the browser first
receives only the tasks outside groups and a summary of every group. The
tasks of a group are fetched from the server when the group is expanded for
the first time, or when one of them is needed, e.g. after entering its id in
the *Task id* field. For the example trace with 15,231 tasks the first
transfer is 0.3 MB instead of 3.5 MB. Three actions need all tasks and load
them: *Expand all*, the timeline view, and *Select parallel region*.

Notes:

* Groups that depend on each other in both directions, e.g. the task types
  of a blocked factorisation, form a cycle once collapsed. The group created
  first is then placed above the others, and some edges point upwards.
* Repeated executions of the same ``taskloop`` or loop by the same task
  form one group.
* The timeline view always shows all tasks.

Timeline view
-------------

One row per OpenMP thread, with the execution segments of all tasks over
time. A red strip below a segment marks a critical-path task. Drag to pan,
the wheel zooms the time axis.

Run information
---------------

*Run information* below the process summary unfolds what the trace header
says about the run of the selected rank: OpenMP runtime, its version and
library, executable, command line, working directory, the CPUs the process
was allowed to use, the number of MPI processes, and the OpenMP-related
environment variables. Entries the recording tool could not determine are
absent, and traces of older versions have none; then the section is hidden.
*CPUs the process may use* shows the binding of the MPI launcher, see
:ref:`binding`.

Task origins
------------

A table of all source locations that create tasks, with, for each:

* the location (file and line, or function and library without debug
  information), the function containing it, and whether it is a ``task``
  construct, a ``taskloop`` or a parallel region (whose implicit tasks are
  counted);
* the number of tasks created there and their average work per task;
* their total work time and its share of T\ :sub:`1`, also drawn as a bar;
* the part of that work on the critical path, and its share of the critical
  path length T\ :sub:`∞`;
* the average parallelism of these tasks on their own (*Par.*);
* the measured peak number of them running at the same time (*Peak*).

The field above the table filters the rows: only locations whose source
file path, file and line, function name (as shown or as compiled, e.g.
``omp_outlined``), library or kind contain the typed text are listed; case
is ignored. While a filter is active, the last row sums up tasks, work and
critical time of the matching locations; clicking it selects all their
tasks for the region analysis. :kbd:`Esc` in the field clears it. The
filter is kept when switching ranks.

The initial task, which holds the serial code outside parallel regions, has
its own row. The last row sums up all tasks. Click a column header to sort,
hover a row for the function name, and click a row to select all its tasks for the region analysis. With *Colour by: Source
location*, each row shows the colour its tasks have in the graph.

Task details
------------

Clicking a task shows:

* kind and OMPT flags (``undeferred``, ``untied``, ``final``, ...), and badges
  for tasks created by a ``taskloop``, tasks on the critical path, tasks that started on a different thread
  than the one that created them (stolen), tasks that ran on several threads
  (untied tasks resumed elsewhere) and tasks the OS moved between CPUs;
* source file and line of the construct, with its address;
* the function containing the construct. Code inside OpenMP constructs is
  compiled into outlined helper functions (clang names them
  ``<function>.omp_outlined...``); the GUI shows the enclosing function of
  the program and names the helper function below it;
* exclusive work time, lifetime, and the part of the work on the critical
  path;
* creating task and thread, creation time and the time until the task first
  ran;
* parallel region with the source location and function where it starts,
  team size and, for implicit tasks, thread number;
* threads, CPUs and NUMA nodes used;
* number of children, predecessors and successors by dependence (clickable);
* the items of the ``depend`` clause;
* every execution segment: thread, CPU (start → end if it changed), NUMA
  node, start and duration.

*Select critical path* below the process summary selects every task with
work on the critical path of the whole graph. The region's critical path
T\ :sub:`∞` then equals that of the graph; its work T\ :sub:`1` is larger,
because only part of some tasks' work lies on the path, and its measured
concurrency shows whether the critical tasks ever ran at the same time.

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

Tasks on the selection's own critical path get a thick black outline, and
the path is drawn as a thick black line. With *Grey out unselected* (the
default) the rest of the graph is drawn in grey. *Clear selection* in the
panel, or the :kbd:`Esc` key, ends the selection. The quantities are defined in :doc:`metrics`.

Links
-----

The state can be preset through the URL fragment, e.g.::

   http://127.0.0.1:8700/#rank=1&view=timeline&color=loc&task=1306&select=subtree

``rank``
   rank to show
``view``
   ``graph`` or ``timeline``
``color``
   ``thread``, ``cpu``, ``numa``, ``loc``, ``region``, ``kind``, ``work``,
   ``create``, ``start`` or ``wait``
``task``
   task id to select and centre
``expand``
   ``all``: expand all groups (loads all tasks)
``perf``
   show the frame time display (no value needed)
``gpubase``
   draw the graph with the browser's GPU canvas instead of the software
   canvas used by default (no value needed); for performance comparisons
``select``
   ``critical``: select all tasks on the critical path; ``subtree`` or
   ``region``: select relative to ``task``
