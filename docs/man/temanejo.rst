temanejo
========

Synopsis
--------

**temanejo** *trace-dir* [**-p** *port*] [**--bind** *address*] [**--max-group** *n*] [**--summary**]

**temanejo** **-h** | **--help**

Description
-----------

**temanejo** loads the task graph traces of one program run from
*trace-dir*, analyses them and serves a graphical user interface for a web
browser.

The traces are recorded by the OMPT tool ``libtmj_ompt.so``, which the
OpenMP runtime loads when ``OMP_TOOL_LIBRARIES`` names it. Every process of
the run writes one file ``rank-``\ *N*\ ``.tmj``; **temanejo** reads all
files with the extension ``.tmj`` in *trace-dir*. Files that cannot be read
are reported and skipped.

For every rank **temanejo** reconstructs the task graph from the OpenMP
events (initial, implicit and explicit tasks, task creation, ``depend``
dependences, ``taskwait``, ``taskgroup`` and barriers), measures the work
time of every task, computes the critical path and the average parallelism,
lays out the graph and resolves code addresses to source file and line.

It then prints one line per rank and, unless **--summary** is given, listens
for HTTP connections and prints the address of the GUI. The GUI shows the
graph and a per-thread timeline of each rank, the details of individual tasks,
and the degree of parallelism of selected regions. The server runs until it
is interrupted.

Options
-------

*trace-dir*
   Directory containing the trace files.

**-p** *port*, **--port** *port*
   TCP port to listen on. Default: 8700.

**--bind** *address*
   Address to listen on. Default: ``127.0.0.1``, which only accepts
   connections from the same machine. Use ``0.0.0.0`` to accept connections
   from other hosts; the server has no authentication.

**--max-group** *n*
   Largest number of nodes (tasks and nested groups) a task group shows in
   the GUI when it is expanded. Groups with more tasks are split into nested
   groups. Default: 1000, minimum: 16. The GUI can change it per rank.

**--summary**
   Print the summary table and exit without starting the server.

**-h**, **--help**
   Print a usage line and exit.

Output
------

The summary table has one line per rank with these columns:

rank, host
   MPI rank and host name of the process.

tasks, threads
   Number of tasks of all kinds and number of OpenMP threads.

wall[ms]
   Time from the first to the last recorded event.

work[ms]
   T\ :sub:`1`: sum of the exclusive work times of all tasks, excluding
   time spent suspended or waiting in synchronisation.

crit.path[ms]
   T\ :sub:`∞`: length of the longest chain of dependent work, based on the
   measured work times.

avg.parallelism
   T\ :sub:`1` / T\ :sub:`∞`, the average number of threads the task graph
   could keep busy.

The chapter "Metrics reference" of the full documentation defines all
metrics shown in the summary and the GUI.

Exit status
-----------

0
   Success (**--summary** or **--help**).
1
   The trace directory cannot be read, contains no usable trace files, or
   the address cannot be bound.
2
   Invalid command line.

Environment
-----------

**temanejo** runs ``llvm-addr2line``, or ``addr2line`` if the former is not
found, from ``PATH`` to obtain source locations. ``DEBUGINFOD_URLS`` is
removed from their environment so that no debug information is fetched over
the network.

The variables that control trace recording (``OMP_TOOL_LIBRARIES``,
``TEMANEJO_TRACE_DIR``, ``TEMANEJO_BUFFER_SIZE``, ``TEMANEJO_FLUSH_INTERVAL``,
``TEMANEJO_SYNC``, ``TEMANEJO_VERBOSE``) are read by the application
being traced, not by **temanejo**.

Files
-----

*trace-dir*\ ``/rank-``\ *N*\ ``.tmj``
   Trace of MPI rank *N*.

The executables and shared libraries recorded in the traces are read for
their debug information. If they have been moved or rebuilt since the run,
source locations are missing or wrong.

Examples
--------

Build and trace the example program shipped in the source tree, two MPI
ranks with four OpenMP threads each, then view it::

   make -C examples
   OMP_NUM_THREADS=4 OMP_PLACES=cores OMP_PROC_BIND=spread,close \
     scripts/tmj-run -o traces mpirun -np 2 --map-by slot:PE=4 examples/mpi_omp_tasks
   temanejo traces

Record a run of any application without the wrapper script::

   export OMP_TOOL_LIBRARIES=$PWD/target/release/libtmj_ompt.so
   export TEMANEJO_TRACE_DIR=$PWD/traces
   OMP_NUM_THREADS=4 mpirun -np 2 -x OMP_TOOL_LIBRARIES -x TEMANEJO_TRACE_DIR ./app
   temanejo traces

The same with the wrapper script from the source tree::

   OMP_NUM_THREADS=4 scripts/tmj-run -o traces mpirun -np 2 ./app
   temanejo traces

Serve on another port and view from a workstation through SSH::

   temanejo traces --port 9000                  # on the cluster login node
   ssh -L 9000:localhost:9000 login-node        # on the workstation

Then open http://localhost:9000/.

Notes
-----

The application must use an OpenMP runtime with OMPT support, such as LLVM
``libomp``; GCC's ``libgomp`` does not provide OMPT. Compile with ``-g`` to
get source locations.

Each rank's timeline starts at its own first event; clocks of different
ranks are not aligned.

Bind every MPI process to as many cores as it has OpenMP threads, e.g.
``mpirun --map-by slot:PE=4`` with ``OMP_PLACES=cores`` and
``OMP_PROC_BIND=spread,close``. Open MPI's default binds each process to a
single core; threads sharing a core are switched out in the middle of
tasks, and that time is counted as task work.

Time inside MPI calls counts as work of the calling task. Serial MPI calls
such as ``MPI_Finalize`` belong to the initial task and can dominate the
critical path.

See also
--------

**mpirun**\ (1), **addr2line**\ (1), **llvm-addr2line**\ (1)

The full documentation, including the trace file format, is in the ``docs``
directory of the source tree.
