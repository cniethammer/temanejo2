Temanejo
========

Temanejo records the task graph of hybrid OpenMP + MPI programs through the
OpenMP Tools Interface (OMPT) and shows it in a web browser: which task created
which, how ``depend`` clauses order them, how long each task actually worked,
on which thread and CPU it ran, where the critical path lies and how much
parallelism a selected part of the graph offers.

It follows the split of the original Temanejo/Ayudame debugger: a small
library inside the application collects events, and a separate program
analyses and displays them.

.. rubric:: Quick start

Build, record the bundled example with two MPI ranks of four threads each,
and open the GUI at http://127.0.0.1:8700/:

.. code-block:: sh

   cargo build --release
   make -C examples
   OMP_NUM_THREADS=4 scripts/tmj-run -o traces mpirun -np 2 examples/mpi_omp_tasks
   target/release/temanejo traces

:doc:`usage` explains each step.

.. toctree::
   :maxdepth: 2
   :caption: User guide

   overview
   usage
   gui
   analysis
   metrics

.. toctree::
   :maxdepth: 2
   :caption: Reference

   trace-format
   runtime-notes
   http-api
   man/temanejo
