Trace file format
=================

Every process traced by ``libtmj_ompt.so`` writes one binary file,
``rank-<rank>.tmj``, into the trace directory (see :doc:`usage`). This page
specifies the format, version ``03``. Reader and writer are implemented in
``crates/tmj-trace/src/lib.rs``.

A trace is written in blocks while the program runs, so its size is not
limited by memory and a run that crashes still leaves the events written up
to then. The previous versions are described in `Versions 01 and 02`_ and
are still read.

Conventions
-----------

* All integers are unsigned and **little endian**.
* ``str`` denotes a string: a ``u32`` byte length followed by that many bytes
  of UTF-8, without terminator. Readers reject lengths above 1 MiB.
* There is no padding between fields, except the two reserved bytes inside an
  event record.
* Offsets in this page are in bytes.

File layout
-----------

A file consists of a header followed by any number of blocks.

.. list-table:: Header
   :header-rows: 1
   :widths: 22 12 66

   * - Field
     - Type
     - Content
   * - ``magic``
     - 8 bytes
     - ASCII ``TMJTRC03``. The last two characters are the format version;
       an incompatible change increments it.
   * - ``rank``
     - ``u32``
     - MPI rank of the process (0 when not started by an MPI launcher).
   * - ``pid``
     - ``u32``
     - Operating system process id.
   * - ``epoch_ns``
     - ``u64``
     - ``CLOCK_REALTIME`` in nanoseconds since the Unix epoch, taken when the
       tool was initialized. Relates the run to wall-clock time; event
       timestamps use a different clock.
   * - ``host``
     - ``str``
     - Host name of the node.
   * - ``runtime_module``
     - ``str``
     - Absolute path of the OpenMP runtime library. Code addresses whose
       symbol record names this module lie inside the runtime (see
       `Taskloop tasks`_).
   * - ``n_info``
     - ``u32``
     - Number of information entries that follow; may be 0.
   * - ``info``
     - ``n_info`` × (``str``, ``str``)
     - Key and value of each entry, see `Run information`_.

Run information
~~~~~~~~~~~~~~~

The information entries describe the run. **All of them are optional**: the
tool writes an entry only if it could determine the value, and other writers
may add entries or write none. Readers must ignore unknown keys and must not
depend on any key being present. Values are text, at most 64 KiB; an entry
appears at most once.

Keys written by ``libtmj_ompt.so``:

.. list-table::
   :header-rows: 1
   :widths: 30 70

   * - Key
     - Value
   * - ``tool.version``
     - Version of the recording tool.
   * - ``omp.runtime_version``
     - The version string the OpenMP runtime passed to the tool
       (``ompt_start_tool``), e.g. ``LLVM OMP version: 5.0.20140926``.
   * - ``omp.version``
     - The runtime's ``_OPENMP`` value, ``yyyymm``, e.g. ``201611``.
   * - ``omp.num_procs``
     - Number of processors available to the runtime
       (``ompt_get_num_procs``), if the runtime knows it when the tool
       starts. LLVM ``libomp`` 23 does not, so the entry is missing there.
   * - ``process.exe``
     - Absolute path of the executable.
   * - ``process.args``
     - The command line, arguments separated by spaces.
   * - ``process.cwd``
     - Working directory when the tool started.
   * - ``process.cpus_allowed``
     - CPUs the process may run on when the tool starts, as a list such as
       ``0-3,8`` (``Cpus_allowed_list`` of ``/proc/self/status``). Shows the
       binding applied by the MPI launcher or batch system.
   * - ``mpi.world_size``
     - Number of MPI processes, from ``OMPI_COMM_WORLD_SIZE`` or
       ``PMI_SIZE``.
   * - ``env.<NAME>``
     - Value of the environment variable ``<NAME>``, for every variable
       whose name starts with ``OMP_``, ``KMP_``, ``GOMP_``, ``LIBOMP_`` or
       ``TEMANEJO_``, sorted by name. A variable set to the empty string is
       written with an empty value; an unset variable has no entry.

The environment shows what was requested, not what the runtime did with it:
team sizes actually used are in the ``IMPLICIT_TASK`` events, and the CPUs
tasks ran on in every event.

Every block starts with a ``u8`` type:

.. list-table:: Blocks
   :header-rows: 1
   :widths: 8 16 76

   * - Type
     - Name
     - Content after the type byte
   * - 1
     - events
     - ``u32 n``, then ``n`` event records of 56 bytes, see `Event
       records`_. Written by one thread when its buffer is full or old (see
       :doc:`usage`), and for all threads at the end.
   * - 2
     - symbols
     - ``u32 n``, then ``n`` `symbol records`_. A symbols block precedes the
       events block in which a code address occurs for the first time.
   * - 3
     - end
     - ``u64``: the total number of events in the file. It is the last block
       and marks the trace as complete.

**Incomplete traces.** If the process was killed, crashed or left through
``_exit``, the file has no end block, and its last block may be cut off.
Readers should use all blocks that are complete, drop a cut-off one, and
report the trace as incomplete: events of every thread are missing from its
last written block on. ``temanejo`` does this and prints a warning.

Event records
-------------

Every event is a fixed 56-byte record:

.. list-table::
   :header-rows: 1
   :widths: 10 12 14 64

   * - Offset
     - Field
     - Type
     - Content
   * - 0
     - ``kind``
     - ``u8``
     - Event kind, see `Event kinds`_.
   * - 1
     - ``endpoint``
     - ``u8``
     - For scoped events: 1 = begin, 2 = end (OMPT
       ``ompt_scope_endpoint_t``; 3 = begin-and-end is passed through if a
       runtime reports it). 0 for all other kinds.
   * - 2
     - ``cpu``
     - ``u16``
     - Logical CPU (hardware thread) the calling thread ran on when the
       event was recorded, from ``getcpu(2)``.
   * - 4
     - ``numa``
     - ``u16``
     - NUMA node of that CPU.
   * - 6
     - reserved
     - 2 bytes
     - Written as 0; ignore when reading.
   * - 8
     - ``thread``
     - ``u32``
     - Tool thread id: dense numbers from 0, assigned in the order in which
       threads record their first event. Not the OpenMP thread number.
   * - 12
     - ``flags``
     - ``u32``
     - Kind-specific flags, see the table below; 0 if unused.
   * - 16
     - ``ts``
     - ``u64``
     - ``CLOCK_MONOTONIC`` timestamp in nanoseconds.
   * - 24
     - ``a``
     - ``u64``
     - Payload, meaning depends on ``kind``.
   * - 32
     - ``b``
     - ``u64``
     - Payload.
   * - 40
     - ``c``
     - ``u64``
     - Payload.
   * - 48
     - ``d``
     - ``u64``
     - Payload.

Unused payload fields are 0. ``endpoint`` is 0 where the table says –.

Event order
~~~~~~~~~~~

Within an events block all records come from one thread, in the order they
were recorded, and the blocks of one thread appear in the order they were
written. Blocks of different threads are interleaved. The file is therefore
*not* globally sorted by time, but reading the blocks in file order yields
every thread's events in order. A reader that needs a single timeline
concatenates the blocks and sorts by ``ts`` with a stable sort, which keeps
the per-thread order of events with equal timestamps.

Identifiers
~~~~~~~~~~~

Task and parallel region ids are assigned by the tool and stored in the
runtime's ``ompt_data_t`` slots, so the same task has the same id in every
event. Tasks and parallel regions have separate counters, both starting at 1.
**0 means "no id"**, for instance when the runtime passed no data pointer or
the object was created before the tool attached.

Event kinds
~~~~~~~~~~~

Each kind corresponds to the OMPT callback named below its name; the
callback's arguments are described in the OpenMP specification, section
"OMPT Callback Signatures".

.. list-table::
   :header-rows: 1
   :widths: 6 30 12 52

   * - kind
     - Name / OMPT callback
     - endpoint
     - Payload
   * - 1
     - ``THREAD_BEGIN``

       ``thread_begin``
     - –
     - ``a`` thread type (1), ``b`` OS thread id
   * - 2
     - ``THREAD_END``

       ``thread_end``
     - –
     - none
   * - 3
     - ``PARALLEL_BEGIN``

       ``parallel_begin``
     - –
     - ``a`` region id, ``b`` encountering task id, ``c`` requested team size, ``d`` code address
   * - 4
     - ``PARALLEL_END``

       ``parallel_end``
     - –
     - ``a`` region id, ``b`` encountering task id
   * - 5
     - ``TASK_CREATE``

       ``task_create``
     - –
     - ``a`` new task id, ``b`` creating task id, ``c`` reported code
       address (6), ``d`` code address
   * - 6
     - ``TASK_SCHEDULE``

       ``task_schedule``
     - –
     - ``a`` prior task id, ``b`` prior task status (2), ``c`` next task id
   * - 7
     - ``IMPLICIT_TASK``

       ``implicit_task``
     - begin / end
     - ``a`` task id, ``b`` region id, ``c`` thread number in team, ``d`` actual team size
   * - 8
     - ``DEPENDENCE_VAR``

       ``dependences``
     - –
     - ``a`` task id, ``b`` dependence variable (3), ``c`` dependence type (4)
   * - 9
     - ``TASK_DEPENDENCE``

       ``task_dependence``
     - –
     - ``a`` predecessor task id, ``b`` successor task id
   * - 10
     - ``SYNC_REGION``

       ``sync_region``
     - begin / end
     - ``a`` sync kind (5), ``b`` task id, ``c`` region id, ``d`` code address
   * - 11
     - ``SYNC_WAIT``

       ``sync_region_wait``
     - begin / end
     - same as ``SYNC_REGION``; brackets the time actually spent waiting
   * - 12
     - ``WORK``

       ``work``
     - begin / end
     - ``a`` work type (7), ``b`` task id, ``c`` iteration count, ``d`` code
       address (8)

``flags`` holds the OMPT ``flags`` argument for ``PARALLEL_BEGIN`` and
``PARALLEL_END`` (``ompt_parallel_flag_t``) and for ``TASK_CREATE`` and
``IMPLICIT_TASK`` (``ompt_task_flag_t``: 0x1 initial, 0x2 implicit, 0x4
explicit, 0x8 target, 0x08000000 undeferred, 0x10000000 untied, 0x20000000
final, 0x40000000 mergeable, 0x80000000 merged). For ``WORK`` events bit 0x1 means that ``d`` was found by unwinding the
stack (8). It is 0 for the other kinds.

"Code address" is the ``codeptr_ra`` argument: the return address of the
runtime call made by the construct, used to find its source location through
the `symbol records`_. 0 if the runtime did not provide one.

Notes:

(1) ``ompt_thread_t``: 1 initial, 2 worker, 3 other, 4 unknown.

(2) ``ompt_task_status_t``: 1 complete, 2 yield, 3 cancel, 4 detach, 5 early
    fulfill, 6 late fulfill, 7 switch. With 5 and 6 no task switch happens.

(3) The ``variable`` member of ``ompt_dependence_t``; for ordinary ``depend``
    items the address of the list item. One record is written per item.

(4) ``ompt_dependence_type_t``: 1 in, 2 out, 3 inout, 4 mutexinoutset,
    5 source, 6 sink, 7 inoutset, 34 out with all memory, 35 inout with all
    memory.

(5) ``ompt_sync_region_t``: 1 barrier, 2 implicit barrier (both deprecated),
    3 explicit barrier, 4 implementation-defined barrier, 5 taskwait,
    6 taskgroup, 7 reduction, 8 implicit barrier at the end of a worksharing
    construct, 9 implicit barrier at the end of a parallel region,
    10 barrier of a teams region.

(6) 0 in the file. After `Taskloop tasks`_ have been attributed by the
    reader, ``c`` holds the address the runtime reported for a task created
    by a ``taskloop`` and ``d`` the address of the construct.

(7) ``ompt_work_t``: 1 loop, 2 sections, 3 single (executing thread),
    4 single (other threads), 5 workshare, 6 distribute, 7 taskloop, 8 scope,
    9 workdistribute, 10–13 loop with static, dynamic, guided or other
    schedule.

(8) For ``taskloop`` constructs libomp reports an address inside the runtime.
    The tool then unwinds the stack and stores the first return address
    outside the runtime and the tool, i.e. the call site in the program, and
    sets bit 0x1 in ``flags``.

Runtime-specific behaviour
~~~~~~~~~~~~~~~~~~~~~~~~~~

These observations about LLVM ``libomp`` 23.1.4 matter when interpreting
traces (see also :doc:`runtime-notes`):

* The initial task is announced through ``task_create`` with the initial
  flag and again through ``implicit_task``; both carry the same id.
* The end event of an implicit task carries no region id (``b`` is 0).
* Tasks created by ``taskloop``, and the construct itself in the ``work``
  callback, are reported with an address inside ``__kmpc_taskloop``; see (8)
  and `Taskloop tasks`_.
* Worker threads report the end of the barrier at the end of a parallel
  region only when they are reused for the next region, if at all. Nothing
  is executed in the implicit task after that barrier has begun.

Symbol records
--------------

One record exists for every distinct non-zero code address in the
``d`` field of ``PARALLEL_BEGIN``, ``TASK_CREATE``, ``SYNC_REGION``,
``SYNC_WAIT`` and ``WORK`` events. The records are produced with
``dladdr1(3)`` while the process runs, when an address first occurs in a
buffer being written, so the server can resolve addresses after the process
has exited or crashed:

.. list-table::
   :header-rows: 1
   :widths: 14 10 76

   * - Field
     - Type
     - Content
   * - ``addr``
     - ``u64``
     - Runtime code address, as in the events.
   * - ``load_bias``
     - ``u64``
     - Load bias of the containing module (``l_addr`` of its
       ``link_map``). ``addr - load_bias`` is the address in the module's
       ELF file, as needed by ``addr2line``.
   * - ``module``
     - ``str``
     - Absolute path of the executable or shared library containing
       ``addr``. Empty if the address is not in any loaded module.
   * - ``symbol``
     - ``str``
     - Nearest dynamic symbol, if any. Usually empty for functions of the
       main program; the server then takes the function name from the debug
       information.

Because ``addr`` is a return address, it points just after the call
instruction. The server looks up ``addr - load_bias - 1`` to get the line of
the call itself.

Taskloop tasks
--------------

LLVM ``libomp`` reports a code address inside the runtime for the tasks of a
``taskloop`` (see :doc:`runtime-notes`). The tool stores what it is given;
the correction is the reader's job, because it needs events of other
threads. After reading all blocks:

1. For every ``WORK`` begin event of work type 7 (``taskloop``) note task
   ``b``, time ``ts`` and address ``d``.
2. For every ``TASK_CREATE`` whose address ``d`` has a symbol record with
   ``module`` equal to the header's ``runtime_module``: take the latest
   taskloop of its creating task ``b`` that began at or before the event's
   ``ts``. If there is one, move ``d`` to ``c`` and set ``d`` to the
   taskloop's address.

A task with ``c != 0`` was created by a ``taskloop``. ``tmj_trace::Trace::read``
does this (``attribute_taskloop_tasks``).

Example: reading a trace in Python
----------------------------------

The following script prints the header and counts events per kind. It needs
only the Python standard library, reads complete and incomplete version 03
files, and does not attribute taskloop tasks:

.. code-block:: python

   import struct, sys, collections

   NAMES = {1: "THREAD_BEGIN", 2: "THREAD_END", 3: "PARALLEL_BEGIN",
            4: "PARALLEL_END", 5: "TASK_CREATE", 6: "TASK_SCHEDULE",
            7: "IMPLICIT_TASK", 8: "DEPENDENCE_VAR", 9: "TASK_DEPENDENCE",
            10: "SYNC_REGION", 11: "SYNC_WAIT", 12: "WORK"}
   EVENT = struct.Struct("<BBHH2xIIQQQQQ")   # 56 bytes

   class Truncated(Exception):
       pass

   def read(f, n):
       data = f.read(n)
       if len(data) < n:
           raise Truncated
       return data

   def read_str(f):
       (n,) = struct.unpack("<I", read(f, 4))
       return read(f, n).decode("utf-8", "replace")

   events, syms, complete = [], [], False
   with open(sys.argv[1], "rb") as f:
       if f.read(8) != b"TMJTRC03":
           sys.exit("not a version 03 temanejo trace")
       rank, pid, epoch_ns = struct.unpack("<IIQ", read(f, 16))
       host, runtime_module = read_str(f), read_str(f)
       (n_info,) = struct.unpack("<I", read(f, 4))
       info = dict((read_str(f), read_str(f)) for _ in range(n_info))
       try:
           while not complete:
               kind = f.read(1)
               if not kind:
                   break                          # no end block
               if kind[0] == 1:                   # events
                   (n,) = struct.unpack("<I", read(f, 4))
                   block = read(f, n * EVENT.size)
                   events += EVENT.iter_unpack(block)
               elif kind[0] == 2:                 # symbols
                   (n,) = struct.unpack("<I", read(f, 4))
                   block = []
                   for _ in range(n):
                       addr, load_bias = struct.unpack("<QQ", read(f, 16))
                       block.append((addr, load_bias, read_str(f), read_str(f)))
                   syms += block
               elif kind[0] == 3:                 # end
                   read(f, 8)
                   complete = True
               else:
                   sys.exit(f"unknown block type {kind[0]}")
       except Truncated:
           pass                                   # drop the cut-off block

   print(f"rank {rank} pid {pid} host {host}: {len(events)} events, "
         f"{len(syms)} symbols, {'complete' if complete else 'INCOMPLETE'}")
   print("runtime:", info.get("omp.runtime_version", "unknown"),
         "| threads requested:", info.get("env.OMP_NUM_THREADS", "not set"))
   counts = collections.Counter(NAMES.get(e[0], e[0]) for e in events)
   for name, n in counts.most_common():
       print(f"  {name:16} {n}")
   # Fields of an event tuple:
   # kind, endpoint, cpu, numa, thread, flags, ts, a, b, c, d

Reading in Rust
---------------

.. code-block:: rust

   let mut f = std::io::BufReader::new(std::fs::File::open("rank-0.tmj")?);
   let trace = tmj_trace::Trace::read(&mut f)?;
   for e in &trace.events {
       if e.kind == tmj_trace::kind::TASK_CREATE {
           println!("task {} created by {} at {} ns", e.a, e.b, e.ts);
       }
   }

``trace.complete`` is false for a trace without end block.

Versions 01 and 02
------------------

**Version 02** (``TMJTRC02``) is version 03 without the information entries:
its header ends after ``runtime_module``, and the first block follows
directly.

**Version 01** (``TMJTRC01``) files were written in one piece when the
program ended:

.. code-block:: text

   magic "TMJTRC01"
   u32 rank, u32 pid, u64 epoch_ns, str host
   u64 n_events, n_events event records
   u32 n_syms,   n_syms symbol records

Event and symbol records are as in version 03. There are no
``runtime_module`` and no information entries; the tool had already
attributed taskloop tasks, so ``TASK_CREATE`` events carry ``c`` as described
in `Taskloop tasks`_.
