Notes on OpenMP runtime behaviour
=================================

Temanejo records what the OpenMP runtime reports through OMPT. Some of what
the runtime reports differs from what the source code suggests. This page
lists the cases that were observed and how they show up in the GUI.

Observed with
-------------

============================= =====================================================
Compiler                      clang 23.1.4 (Arch Linux package ``clang 23.1.4-1``,
                              target ``x86_64-pc-linux-gnu``)
OpenMP runtime                LLVM ``libomp`` from ``openmp 23.1.4-1``
                              (``/usr/lib/x86_64-pc-linux-gnu/libomp.so``); it
                              identifies itself to OMPT tools as
                              ``LLVM OMP version: 5.0.20140926``
MPI                           Open MPI 5.0.11
Compile flags                 ``-fopenmp -g -O1``
Program                       ``examples/mpi_omp_tasks.c``, 2–3 ranks × 4 threads
============================= =====================================================

Other versions may behave differently. When in doubt, check the numbers
against the source, as done below.

``taskloop``: tasks attributed to ``__kmpc_taskloop``
-----------------------------------------------------

**What you see.** In the task origins table and the task details, the tasks
of a ``taskloop`` construct have the location
``__kmpc_taskloop (libomp.so)`` instead of a file and line of the program.
There are also more of them than the construct asks for.

In the example program, the construct

.. code-block:: c

   #pragma omp taskloop num_tasks(loop_tasks / 4) nogroup   // num_tasks(2000)
   for (int i = 0; i < loop_tasks; i++)
     burn(1500 + (i % 7) * 300);

runs once in each of two inner teams (a ``single`` inside two nested
``parallel`` regions), so 2 × 2000 = 4000 tasks are expected. The trace of
rank 1 contains 4254 tasks with the location ``__kmpc_taskloop``: 2127 per
team.

**Why the location is in the runtime.** For a ``task`` construct clang emits
the task allocation and submission at the construct, so the OMPT
``task_create`` callback receives a return address (``codeptr_ra``) in the
program. For ``taskloop`` clang emits a single call to the runtime entry
point ``__kmpc_taskloop``, which splits the iteration space and creates the
tasks itself. libomp passes a return address *inside* ``__kmpc_taskloop`` to
``task_create`` for these tasks, not the address of the call in the program.
The OpenMP specification intends ``codeptr_ra`` to identify the construct in
the program, so this is a limitation of libomp; Temanejo records the address
as reported.

**Why there are more tasks.** For large task counts libomp does not create
the tasks of a ``taskloop`` one after the other. It splits the iteration
range recursively into halves, using internal helper tasks that each create
the tasks of their half. These helper tasks are reported through
``task_create`` like the loop chunks: same location, same flags (explicit,
``0x4``). The trace shows them clearly:

* 2127 − 2000 = 127 = 2\ :sup:`7` − 1 extra tasks per team, the inner nodes
  of a binary split tree of depth 7;
* 125 of the 2127 tasks per team run for less than 10 µs, whereas a loop
  chunk runs for about 60–100 µs;
* the first tasks created are a chain of short ones (19, 7.6, 7.2, 3.2, 3.1,
  ... µs), followed by the first loop chunks (59, 79, 68 µs);
* the tasks are created on both threads of each team (1064 and 1063), even
  though only one thread executes the ``single`` and thus the ``taskloop``.

The task count at which libomp switches to recursive splitting was not
determined.

**How the tasks are connected.** libomp reports the task that encountered the
``taskloop`` as the creator of all these tasks, also of those actually
created by a helper task on another thread. In the graph all of them are
therefore children of the encountering task (here: the implicit task
executing the ``single``), and the helper hierarchy is not visible.

**Consequences for the analysis.**

* The task origins row ``__kmpc_taskloop (libomp.so)`` combines the loop
  chunks and the helper tasks of *all* ``taskloop`` constructs of the
  program. Its work time is almost entirely the work of the loop bodies.
* Helper tasks can be recognised only by their short work time.
* The critical path does not include the chain of helper tasks between the
  encountering task and a loop chunk; it uses a direct creation edge
  instead. With helper tasks of a few microseconds and a depth of 7, the
  effect on T\ :sub:`∞` is small.

**Possible improvement (not implemented).** libomp also reports the
``taskloop`` construct through the OMPT ``work`` callback
(``ompt_work_taskloop``) with the program's return address. The tool could
remember that address per encountering task and assign it to tasks whose
reported address lies inside the OpenMP runtime.

Nested parallel regions: ``__kmp_invoke_microtask``
---------------------------------------------------

**What you see.** The implicit tasks of a parallel region that is nested
inside another parallel region have the location
``__kmp_invoke_microtask (libomp.so)``.

In the example program the outer region at line 68 is attributed correctly
(``phase_nested``, line 68). The two instances of the inner region at line 70
both report a ``parallel_begin`` return address inside
``__kmp_invoke_microtask``, the runtime function that calls the outlined body
of the outer region, rather than an address in the outlined function where
the inner ``parallel`` construct is.

**Consequences.** The task origins table shows one row
``__kmp_invoke_microtask (libomp.so)`` for the implicit tasks of all nested
regions. The parallel region of such a task in the task details shows the
same location. Structure, timing and critical path are not affected.

Event details
-------------

Further libomp behaviour that matters when reading the raw trace (initial
task announced twice, missing region id on implicit task end, late barrier
end events of worker threads) is listed in :doc:`trace-format`, section
"Runtime-specific behaviour".

``untied`` tasks and ``taskwait``
---------------------------------

A first version of the example program created the recursive tasks of the
``fib`` phase as ``untied`` tasks with ``taskwait``. With
``OMP_NUM_THREADS=4`` and recursion depth 14 the program did not finish (it
was stopped after 30 s; the run takes about 0.05 s with tied tasks), with
and without the Temanejo tool attached. Smaller depths finished. The cause
was not investigated; the example now uses tied tasks. As a consequence,
the detection of tasks migrating between threads (possible only for untied
tasks) has not been tested.
