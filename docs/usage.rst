Recording and viewing a trace
=============================

Quick start
-----------

.. code-block:: sh

   cargo build --release
   make -C examples

   OMP_NUM_THREADS=4 scripts/tmj-run -o traces mpirun -np 2 examples/mpi_omp_tasks
   target/release/temanejo traces

The parts of the recording command:

``OMP_NUM_THREADS=4``
   Four OpenMP threads per process. ``mpirun`` passes it on to the ranks.

``scripts/tmj-run -o traces``
   Attaches the OMPT tool to everything started by the rest of the command
   line and writes the trace files to ``traces/`` (see `Attaching the
   tool`_).

``mpirun -np 2``
   Starts two MPI processes; each writes its own ``traces/rank-<N>.tmj``.

``examples/mpi_omp_tasks``
   The example program: a blocked sweep with ``depend`` clauses, a
   recursive computation with ``taskwait``, and nested parallel regions with
   ``taskgroup`` and ``taskloop``, about 12 000 to 15 000 tasks per rank.
   Optional arguments ``[blocks=24] [fib_n=17] [loop_tasks=8000]`` change
   its size.

Any other MPI + OpenMP program is traced the same way: replace
``examples/mpi_omp_tasks`` with the program and its arguments. Without MPI,
drop ``mpirun -np 2``.

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
