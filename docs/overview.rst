Overview
========

Components
----------

.. list-table::
   :header-rows: 1
   :widths: 22 22 56

   * - Component
     - Built as
     - Role
   * - OMPT tool
     - ``libtmj_ompt.so``
     - Loaded by the OpenMP runtime through ``OMP_TOOL_LIBRARIES``. Records
       OMPT events into per-thread memory buffers and writes one trace file
       per process when the runtime shuts down. Corresponds to Ayudame.
   * - Server
     - ``temanejo``
     - Reads the trace files of one run, reconstructs and analyses the task
       graph of every process and serves the GUI over HTTP. See
       :doc:`man/temanejo`.
   * - GUI
     - embedded in ``temanejo``
     - HTML, CSS and JavaScript drawing on a canvas; no external
       dependencies. Corresponds to the Temanejo GUI. See :doc:`gui`.
   * - Trace format
     - ``tmj-trace`` crate
     - Shared by tool and server. See :doc:`trace-format`.

Data flow::

   application, rank 0..N-1                         temanejo <trace-dir>
   +-----------------------------+   rank-0.tmj    +---------------------------+
   | libomp --OMPT callbacks--+  |   rank-1.tmj    | graph reconstruction      |
   |                          v  | ------------->  | critical path, layout     | --HTTP--> browser
   |               libtmj_ompt.so|   ...           | source lines (addr2line)  |
   +-----------------------------+                 +---------------------------+

Differences from the original Temanejo
--------------------------------------

* **Post mortem.** Events are written when the OpenMP runtime finalizes, not
  streamed while the program runs. There is no execution control
  (breakpoints, stepping).
* **Standard interface.** Events come from OMPT, so any OpenMP runtime that
  implements OMPT works without modifications to the runtime.
* **Performance data.** Every task carries measured execution segments with
  thread, CPU and NUMA node, which the critical path and parallelism
  analysis are based on.

Requirements
------------

* Linux.
* Rust 1.77 or newer to build. The workspace has no external crate
  dependencies and builds offline.
* An OpenMP runtime with OMPT support, such as LLVM ``libomp`` (developed
  against LLVM 23). GCC's ``libgomp`` has no OMPT support, so the application
  must be compiled with clang, e.g. ``OMPI_CC=clang mpicc -fopenmp``.
* An MPI implementation that starts processes through PMIx or PMI (developed
  against Open MPI 5.0). The tool does not link against MPI.
* For source locations: the application built with ``-g``, and
  ``llvm-addr2line`` or GNU ``addr2line`` where the server runs.
* For the documentation: Sphinx (``pip install -r docs/requirements.txt``).

Building
--------

.. code-block:: sh

   cargo build --release            # target/release/libtmj_ompt.so, target/release/temanejo
   make -C examples                 # example program (needs clang and mpicc)
   make -C docs html man            # docs/_build/html, docs/_build/man/temanejo.1
   cargo test --workspace           # unit tests

The tests need neither MPI nor an OpenMP runtime: the analysis is tested on
small synthetic traces written by hand after the OMPT callback semantics
(``crates/temanejo/src/tests.rs``), with work, critical path and origin
shares computed by hand. They also check invariants that hold for every
trace: critical times per task never exceed work, they add up to T\ :sub:`∞`,
and the task origins partition the tasks so their work and critical times
add up to T\ :sub:`1` and T\ :sub:`∞`. The trace file format has
round-trip tests in ``crates/tmj-trace``.

Source layout
-------------

========================== ===================================================
``crates/tmj-trace``       trace file format (reader and writer)
``crates/tmj-ompt``        OMPT tool
``crates/temanejo/src``    server: ``model.rs`` graph reconstruction,
                           ``analysis.rs`` critical path, parallelism and
                           task origins, ``tests.rs``,
                           ``layout.rs``, ``symbolize.rs``, ``main.rs``
                           (command line, HTTP, JSON)
``crates/temanejo/web``    GUI
``examples``               MPI + OpenMP test program
``scripts/tmj-run``        launcher wrapper
``docs``                   this documentation
========================== ===================================================
