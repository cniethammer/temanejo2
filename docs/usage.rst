Recording and viewing a trace
=============================

Quick start
-----------

.. code-block:: sh

   cargo build --release
   make -C examples

   OMP_NUM_THREADS=4 OMP_PLACES=cores OMP_PROC_BIND=spread,close \
     scripts/tmj-run -o traces mpirun -np 2 --map-by slot:PE=4 examples/mpi_omp_tasks
   target/release/temanejo traces

The parts of the recording command:

``OMP_NUM_THREADS=4``
   Four OpenMP threads per process. ``mpirun`` passes it on to the ranks.

``OMP_PLACES=cores OMP_PROC_BIND=spread,close``
   Pin each OpenMP thread to its own core; ``spread,close`` also does this
   for the inner teams of nested parallel regions. See `Binding MPI
   processes and OpenMP threads`_.

``scripts/tmj-run -o traces``
   Attaches the OMPT tool to everything started by the rest of the command
   line and writes the trace files to ``traces/`` (see `Attaching the
   tool`_).

``mpirun -np 2 --map-by slot:PE=4``
   Starts two MPI processes, each bound to four cores of its own; each
   writes its own ``traces/rank-<N>.tmj``. Without ``--map-by``, Open MPI
   binds each process to a single core, see below.

``examples/mpi_omp_tasks``
   The example program: a blocked sweep with ``depend`` clauses, a
   recursive computation with ``taskwait``, and nested parallel regions with
   ``taskgroup`` and ``taskloop``, about 12 000 to 15 000 tasks per rank.
   Optional arguments ``[blocks=24] [fib_n=17] [loop_tasks=8000]`` change
   its size.

Any other MPI + OpenMP program is traced the same way: replace
``examples/mpi_omp_tasks`` with the program and its arguments, and set
``PE`` to the number of threads per process. Without MPI, drop the
``mpirun`` part.

The last command prints a summary per rank and serves the GUI at
http://127.0.0.1:8700/.

Preparing the application
-------------------------

* Compile with an OpenMP compiler whose runtime supports OMPT, e.g. clang
  with ``libomp``. With Open MPI's compiler wrappers select clang through
  ``OMPI_CC=clang`` (``OMPI_CXX=clang++`` for C++).
* Add ``-g`` so the server can map task creation sites to file and line.
  Optimisation can stay enabled.
* No code changes and no linking against the tool are needed.

Attaching the tool
------------------

The OpenMP runtime loads the tool named in ``OMP_TOOL_LIBRARIES`` at start-up.
``scripts/tmj-run`` sets the variables and runs a command:

.. code-block:: sh

   scripts/tmj-run [-o trace-dir] <command> [args...]

It is equivalent to:

.. code-block:: sh

   export OMP_TOOL_LIBRARIES=/path/to/target/release/libtmj_ompt.so
   export TEMANEJO_TRACE_DIR=/absolute/path/to/traces
   <command> [args...]

Open MPI's ``mpirun`` forwards exported variables to ranks on the local node
only. For runs across nodes forward them explicitly:

.. code-block:: sh

   mpirun -np 64 -x OMP_TOOL_LIBRARIES -x TEMANEJO_TRACE_DIR ./app

Under a batch system such as Slurm (``srun``), exported variables are usually
propagated by default.

.. _binding:

Binding MPI processes and OpenMP threads
----------------------------------------

**Check the binding before interpreting a trace.** Temanejo measures time
inside the program. If several OpenMP threads share one core, the operating
system switches between them in the middle of tasks. OMPT does not report
this, so the time a task was switched out counts as its work: tasks look
longer, T\ :sub:`1` and T\ :sub:`∞` grow, and the measured concurrency
cannot exceed the number of cores actually available.

**Open MPI's default binding.** Open MPI 5.0.11 bound each process of
``mpirun -np 2`` to a single core, whatever ``OMP_NUM_THREADS`` is::

   $ OMP_NUM_THREADS=4 mpirun -np 2 --report-bindings true
   Rank 0 bound to package[0][core:L0]
   Rank 1 bound to package[0][core:L1]

All four OpenMP threads of a rank then share the two hardware threads of
one core. Give each process as many cores as it has threads with
``--map-by slot:PE=<threads>``, and pin the threads inside it with
``OMP_PLACES`` and ``OMP_PROC_BIND``::

   $ OMP_NUM_THREADS=4 mpirun -np 2 --map-by slot:PE=4 --report-bindings true
   Rank 0 bound to package[0][core:L0-3]
   Rank 1 bound to package[0][core:L4-7]

``--bind-to none`` also avoids the oversubscription, but lets the operating
system move threads between cores at will.

**Nested parallelism.** With ``OMP_PROC_BIND=close`` the inner teams of
nested parallel regions are placed onto overlapping places: in the example
program, two threads shared one core during the nested phase while another
core stayed unused. ``OMP_PROC_BIND=spread,close`` spreads the outer team
over the places and keeps each inner team close to its parent thread; then
every thread had a core of its own.

Measured with the example program, 2 ranks × 4 threads on a node with
8 cores (16 hardware threads), rank 1:

.. list-table::
   :header-rows: 1
   :widths: 52 16 16 16

   * - Binding
     - wall time
     - work T₁
     - avg. concurrency
   * - default (one core per rank)
     - 457 ms
     - 799 ms
     - 1.75
   * - ``--map-by slot:PE=4``, ``OMP_PROC_BIND=close``
     - 201 ms
     - 634 ms
     - 3.16
   * - ``--map-by slot:PE=4``, ``OMP_PROC_BIND=spread,close``
     - 210 ms
     - 653 ms
     - 3.11

The wall times of the last two rows differ by run-to-run variation; what
``spread,close`` changes is the placement in the nested phase.

**Checking the binding in Temanejo.** *Colour by: CPU* in the timeline
shows which logical CPU each segment ran on; the task details list the CPUs
of a task and flag tasks the operating system moved between CPUs. Two
threads alternating on the same CPU, or the same pair of CPUs, mean they
share a core. Logical CPUs are numbered by the operating system;
``lscpu -e=CPU,CORE`` shows which ones belong to the same core.

.. _mpi-time:

MPI time in the trace
---------------------

OMPT reports OpenMP events only. Time a task spends inside MPI calls is
therefore counted as work of that task, and Temanejo cannot tell it apart
from computation. Keep this in mind in particular for the critical path:

* **Serial MPI calls belong to the initial task.** Calls outside parallel
  regions, such as a final ``MPI_Reduce`` and ``MPI_Finalize``, run in the
  initial task. Everything that runs serially lies on the critical path. In
  the example program the initial task's last segment, from the end of the
  last parallel region to the end of the trace, takes about 42–51 ms and
  makes up two thirds of T\ :sub:`∞`. In the OpenMP-only build of the same
  program (``-DNO_MPI``) it takes 0.03 ms, and the initial task's share of
  T\ :sub:`∞` is 4 %.
* **Waiting counts as work.** A rank waiting in a collective or a blocking
  receive for another rank looks as if it were computing. Load imbalance
  between ranks therefore shows up as long tasks, not as waiting.
* **Not everything is in the trace.** The trace starts when the OpenMP
  runtime initialises, usually at the first OpenMP construct, so
  ``MPI_Init`` before it is not included. It ends when the runtime shuts
  down at program exit, after ``MPI_Finalize``.
* **Ranks are analysed separately.** Clocks of different ranks are not
  aligned, and messages between ranks are not part of the graph.

**What to do.**

* Look at the initial task's share of the critical path (*Task origins*,
  row *initial task*) and at its segments in the task details. A long
  segment before the first or after the last parallel region is serial
  code, often MPI.
* To analyse only the OpenMP part, select it as a region: click an implicit
  task of a parallel region and use *Select parallel region*, or Shift+drag
  over the graph below the initial task. The region's critical path then
  leaves out the serial code.
* Keep MPI calls inside tasks short or put them in tasks of their own, so
  that they show up as separate tasks with their own source location.

Environment variables of the tool
---------------------------------

``OMP_TOOL_LIBRARIES``
   Read by the OpenMP runtime, not by the tool: path of ``libtmj_ompt.so``.

``TEMANEJO_TRACE_DIR``
   Directory the trace files are written to; created if missing. Default:
   ``tmj-trace`` relative to each process's working directory. It must be on
   a file system that every node can write to.

``TEMANEJO_VERBOSE``
   If set (to any value), the tool reports on standard error which runtime it
   attached to, which callbacks are unavailable, and how many events it wrote.

``TEMANEJO_LIB``
   Read by ``tmj-run`` only: tool library to use instead of
   ``target/release/libtmj_ompt.so``.

``PMIX_RANK``, ``OMPI_COMM_WORLD_RANK``, ``PMI_RANK``
   Set by the MPI launcher. The first one present that holds a number is used
   as the rank in the trace file name. Without any of them the rank is 0.

Output
------

Each process writes ``<TEMANEJO_TRACE_DIR>/rank-<rank>.tmj`` when the OpenMP
runtime finalizes, normally at program exit. The format is described in
:doc:`trace-format`. A process that crashes, is killed or leaves through
``_exit`` writes no file. Existing files of the same name are overwritten, so
use a fresh directory per run.

Events are kept in memory until then: 56 bytes per event, about five events
per task (the example program: 60 890 events for 12 037 tasks, 3.4 MB).

Viewing
-------

.. code-block:: sh

   temanejo traces                    # http://127.0.0.1:8700/
   temanejo traces --port 9000
   temanejo traces --summary          # table only, no server

The server only needs the trace directory and, for source locations, read
access to the application binaries at the paths recorded in the trace. On a
cluster run it on the login node and forward the port::

   ssh -L 8700:localhost:8700 login-node

All options are listed in :doc:`man/temanejo`.
