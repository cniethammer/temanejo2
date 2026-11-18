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

Max group size
   The largest number of nodes a group shows when it is expanded; see
   `Collapsing groups`_. Changing it regroups the rank.

Collapse groups, Expand all
   Collapse all detected task groups into one node each (the state the GUI
   opens in), or expand all groups; see `Collapsing groups`_.

Task id, Fit
   Jump to a task by its id; fit the view to the window (key ``F``).

Graph view
----------

Each task is a node: circle for explicit tasks, square for implicit tasks,
diamond for the initial task. Grey edges are task creation (parent to child,
including a parallel region's encountering task to the implicit tasks),
coloured edges with arrowheads are ``depend`` dependences, coloured by type
(see `Dependence types`_). Red marks the critical path.

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

Clicking a task also highlights its edges: its creation edge (drawn as a
direct line also where the graph draws a bus) and its dependence edges to
predecessors and successors are drawn again on top, wider and opaque, so
they can be followed through crossing edges. If the task is inside a
collapsed group, the group node's edges are highlighted. Edge kinds switched
off in the toolbar stay hidden.

Dependence types
----------------

OMPT reports a dependence edge only as a pair of tasks. Temanejo derives
*why* the successor waits from the ``depend`` clauses of both tasks, which
OMPT reports with the address and type of each item: every address that
appears in both clauses is classified as

====== ======= ===================================================================
Type   Colour  Meaning
====== ======= ===================================================================
RAW    blue    read after write (flow): the predecessor writes (``out``,
               ``inout``), the successor reads (``in``, ``inout``)
WAR    orange  write after read (anti): the predecessor reads, the successor
               writes
WAW    purple  write after write (output): both write, the successor does not
               read
?      grey    the two clauses share no address, e.g. an edge the runtime adds
               for ``inoutset`` or ``mutexinoutset``
====== ======= ===================================================================

``inout`` followed by ``in`` or ``inout`` counts as RAW: the write is what the
successor waits for. ``mutexinoutset`` and ``inoutset`` count as ``inout``;
``omp_all_memory`` matches every address.

Two tasks can be ordered by several variables and so by several types at
once. Each type present is drawn as a line of its own colour, side by side,
each with an arrowhead at the successor, the task that waits: tasks with a
RAW and a WAR dependence are connected by a blue and an orange line. An
edge between collapsed groups carries the types of all edges it stands for.
On the critical path (or the critical path of a selected region) the lines
lie on a red (black) band. When zoomed out far, the lines coincide and the
arrowheads are left out; when zoomed in far enough to show task ids, each
edge is labelled with its types (e.g. ``RAW+WAR``) where the label covers
no node, arrowhead or other label. The legend lists the types present in
the view.

WAR and WAW dependences carry no data from one task to the other; they
exist because both tasks use the same memory. Giving the tasks separate
copies (renaming, privatisation) removes them, which can be a way to more
parallelism.

With clang, ``out`` items are reported as ``inout``, so a dependence between
two ``out`` items appears as RAW, not WAW; see :doc:`runtime-notes`.

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

The first two include the tasks their members create. In addition,
*Collapse subtree* in the task details collapses everything below any task,
e.g. a whole parallel region.

**Maximum group size.** Expanding a group never shows more than *Max group
size* nodes (default 1000, set in the toolbar or with ``--max-group``). A
group with more tasks is split into nested groups, which are expanded one
after the other:

* many sibling tasks are split by creation order into parts of at most that
  many tasks; the tooltip of a part says which siblings it holds
  (*sibling tasks 1001–2000 of 147440*);
* of a large task tree the upper levels are shown, as many as half the
  limit allows, and what is below becomes subtree groups, each filled up to
  the limit where the tree allows.

Changing *Max group size* regroups the tasks of the rank on the server and
reloads it collapsed; a smaller value keeps the graph lighter, a larger one
needs fewer clicks. The minimum is 16.

A collapsed group is drawn as a stack of cards, wider the more tasks it
holds, with the number of tasks (``×2300``) below it. Its colour is that of
its typical task in the current colour mode (most frequent thread, CPU,
source location ..., mean times), and it has a red outline if any of its
tasks is on the critical path. All edges between two nodes are merged into
one line. The graph is laid out again whenever groups are collapsed or
expanded.

A click on a group node expands it; *Collapse its group* in the details of
one of its tasks collapses it again. *Expand all* in the toolbar expands
everything, *Collapse groups* collapses all detected groups at all levels. Shift+click on
a group node selects its tasks for the region analysis, as does a selection
rectangle around it.

Hovering a group node shows its kind and source location, the number of
tasks, their total work with the share of T\ :sub:`1`, the average, shortest
and longest task, when and on how many threads they ran, their average
parallelism and peak (as in the task origins table) and their work on the
critical path.

**Loading on demand.** To keep large traces manageable, the browser first
receives only the tasks outside groups and a summary of every group. The
tasks a group shows are fetched from the server when it is expanded for the
first time, or when one of them is needed, e.g. after entering its id in the
*Task id* field; the dependence edges between the visible nodes are fetched
with every collapse and expansion. Parallelism and peak in a group's tooltip
are computed by the server when the group is first hovered. Two actions
need all tasks and load them in one piece: *Expand all* (which asks for
confirmation above 50,000 tasks) and *Select parallel region*. The timeline
does not load task records; see `Timeline view`_.

Notes:

* Groups that depend on each other in both directions, e.g. the task types
  of a blocked factorisation, form a cycle once collapsed. The group created
  first is then placed above the others, and some edges point upwards.
* Repeated executions of the same ``taskloop`` or loop by the same task
  form one group.
* The timeline view always shows all tasks, whatever is collapsed in the
  graph.

Timeline view
-------------

One row per OpenMP thread, with the execution of all tasks over time. A red
strip below marks critical-path tasks. Drag to pan, the wheel zooms the time
axis.

The browser asks the server only for the visible time range, at the
resolution of the window, so the timeline works the same for small and very
large traces:

* **Zoomed out**, each pixel column of a thread is one bar. Its height is
  the fraction of that time the thread was executing tasks, its colour that
  of the task taking most of the column, and the red strip appears if any
  task in the column is on the critical path. Hovering or clicking refers to
  that task.
* **Zoomed in**, once at most 4,000 segments are in view, the real
  execution segments are drawn, separated by a small gap, and each can be
  hovered and clicked.

Clicking shows the task's details, loading its record if necessary; the
tooltip of a task that is not loaded yet shows only its id. Shift+drag
selects the tasks that executed in a time range; with *Grey out unselected*
everything else is grey, also when zoomed out.

**Critical path strip.** With the *Critical path* overlay on, a strip above
the threads shows for every moment of the run what happens with respect to
the critical path:

.. list-table::
   :header-rows: 1
   :widths: 14 30 56

   * - Colour
     - Meaning
     - What it suggests
   * - red
     - a task on the critical path is running
     - the path is being worked on
   * - grey
     - gap: no critical task runs, and every thread runs some other task
     - throughput-bound: more threads or less work would help, a shorter
       critical path would not
   * - amber
     - gap: a critical task is ready (created, its dependences finished)
       but not started while a thread is idle
     - scheduling problem: e.g. ``priority`` clauses for the critical tasks
   * - light grey
     - gap: threads are idle and no critical task is ready
     - runtime overhead between tasks, or time spent outside tasks

Like the thread rows, the strip is fetched for the visible range: zoomed
out, each pixel column shows the class that takes most of its time, so
short intervals between fine-grained tasks only become visible when zoomed
in; then the exact intervals are drawn. Hovering the strip shows the
interval or column under the mouse; the legend gives the share of each
class in the whole run. The definitions are in :doc:`metrics`, "Critical
path gaps". The classification is computed by the server when the timeline
is first shown.

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
* number of children;
* predecessors (*Depends on*) and successors (*Required by*) by dependence,
  clickable, each with the variables that order the two tasks: the
  dependence type (see `Dependence types`_), the address, and the types of
  the two ``depend`` items, e.g. ``RAW 0x7ffe4a6754a0 (inout → in)``. OMPT
  reports addresses only; variable names are not shown;
* the items of the ``depend`` clause;
* every execution segment: thread, CPU (start → end if it changed), NUMA
  node, start and duration.

*Select critical path* below the process summary selects every task with
work on the critical path of the whole graph. The region's critical path
T\ :sub:`∞` then equals that of the graph; its work T\ :sub:`1` is larger,
because only part of some tasks' work lies on the path, and its measured
concurrency shows whether the critical tasks ever ran at the same time.

*Expand critical path* next to it expands every group that contains a task
on the critical path, through all nesting levels, so that the whole path is
visible in the graph while everything else stays collapsed. Together with
*Select critical path* and *Grey out unselected* the path then stands out
from the tasks around it.

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
   ``all``: expand all groups (loads all tasks); ``critical``: expand the
   groups containing the critical path
``perf``
   show the frame time display (no value needed)
``gpubase``
   draw the graph with the browser's GPU canvas instead of the software
   canvas used by default (no value needed); for performance comparisons
``select``
   ``critical``: select all tasks on the critical path; ``subtree`` or
   ``region``: select relative to ``task``
