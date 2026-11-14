# Temanejo – task graph performance tool for OpenMP + MPI (draft)

Records the task graph of a hybrid OpenMP + MPI program through the OMPT
interface and shows it in a browser: who created which task, which `depend`
clauses order them, how long each task really worked, on which thread and CPU
it ran, where the critical path is and how parallel a selected part of the
graph is.

The structure follows the original Temanejo/Ayudame pair:

| Role | Original | Here |
|---|---|---|
| Runtime-side event collector | Ayudame library | `libtmj_ompt.so`, an OMPT tool (Rust `cdylib`) |
| Graph display and analysis | Temanejo (Python/GTK) | `temanejo` server (Rust) + browser GUI (plain JS, canvas) |

Unlike the original, which streamed events over a socket and could steer the
program, this draft works **post mortem**: each process writes a trace file
when the OpenMP runtime shuts down, and the server analyses the files.

```
 app (rank 0..N-1)                               temanejo <trace-dir>
 ┌──────────────────────────┐   rank-0.tmj      ┌───────────────────────────┐
 │ libomp ──OMPT callbacks──┼─▶ rank-1.tmj ───▶ │ graph reconstruction      │
 │   └▶ libtmj_ompt.so      │   …               │ critical path, layout     │──HTTP──▶ browser
 └──────────────────────────┘                   │ source lines (addr2line)  │
                                                └───────────────────────────┘
```

Everything is Rust without external crates (it builds offline on a cluster
login node); the GUI is HTML/CSS/JavaScript embedded in the server binary.
The only C in the repository is the example program.

## Requirements

- Linux, Rust ≥ 1.77
- An OpenMP runtime with OMPT: LLVM `libomp` (developed against LLVM/clang 23).
  GCC's `libgomp` has no OMPT, so the application must be built with clang
  (`OMPI_CC=clang mpicc -fopenmp …`).
- Any MPI launched through PMIx or PMI (developed against Open MPI 5.0). The
  tool does not link against MPI.
- `llvm-addr2line` or `addr2line` on the machine running the server, and the
  application built with `-g`, for file:line source locations.

## Quick start

```sh
cargo build --release
make -C examples                       # builds mpi_omp_tasks (and omp_tasks without MPI)

OMP_NUM_THREADS=4 scripts/tmj-run -o traces mpirun -np 2 examples/mpi_omp_tasks
target/release/temanejo traces         # prints a summary, then serves http://127.0.0.1:8700/
```

`tmj-run` only sets two environment variables; the manual equivalent is

```sh
export OMP_TOOL_LIBRARIES=$PWD/target/release/libtmj_ompt.so
export TEMANEJO_TRACE_DIR=$PWD/traces      # default: ./tmj-trace
mpirun -np 2 -x OMP_TOOL_LIBRARIES -x TEMANEJO_TRACE_DIR ./app
```

`TEMANEJO_VERBOSE=1` makes the tool report what it attached to and wrote.
The trace directory must be on a file system all ranks can write to.
On a cluster, run the server where the traces are and forward the port:
`ssh -L 8700:localhost:8700 login-node`.

Server options: `temanejo <trace-dir> [--port 8700] [--bind 127.0.0.1] [--summary]`
(`--summary` prints the per-rank table and exits).

## The GUI

- **MPI rank** selector switches between the graphs of the processes.
- **Graph view**: tasks as nodes (circle = explicit, square = implicit,
  diamond = initial task), light edges = task creation, dark edges =
  `depend` dependences, red = critical path. Drag to pan, wheel to zoom,
  `F` or *Fit* to see everything.
- **Timeline view**: one row per thread with the execution segments of all
  tasks; the red strip below a segment marks critical-path tasks.
- **Colour by** thread, CPU, NUMA node, source location, parallel region,
  task kind or work time.
- **Click a task** for its details: source file and line, work time, time
  spent on the critical path, creating task and thread, parallel region and
  team, `depend` items, and one row per execution segment with thread, CPU
  and NUMA node. Badges flag tasks that ran on a different thread than the
  one that created them (stolen), migrated between threads (untied tasks) or
  were moved between CPUs by the OS.
- **Shift+drag** selects a region of the graph (or a time range in the
  timeline); *Select subtree* and *Select parallel region* select
  structurally. For the selection the panel shows work T₁, critical path
  T∞, average parallelism T₁/T∞, the measured average and peak concurrency,
  a concurrency-over-time chart and the work share per thread.
- Views can be linked: `#rank=1&view=timeline&color=loc&task=1306&select=subtree`.

## How the numbers are obtained

**Recording.** The tool registers the OMPT callbacks `thread_begin/end`,
`parallel_begin/end`, `implicit_task`, `task_create`, `task_schedule`,
`dependences`, `task_dependence`, `sync_region` and `sync_region_wait`. Each
callback appends one 56-byte record (timestamp from `CLOCK_MONOTONIC`, CPU
and NUMA node from `getcpu`, ids stored in the `ompt_data_t` slots) to a
per-thread buffer – no locks, no I/O while the program runs. Code addresses
(`codeptr_ra`) are resolved to module + load bias at shutdown; file and line
are looked up by the server.

**Work time.** A task's work time is exclusive: the sum of the intervals in
which it was the running task of a thread, without the time it was suspended
at a scheduling point and without time waiting in `taskwait`, at the end of a
`taskgroup` or in a barrier. Each interval is an *execution segment* with its
thread and CPU, which is what makes stealing and migration visible.

**Critical path.** Tasks alone cannot carry the ordering "child must finish
before the parent's `taskwait` returns" without creating cycles, so the
analysis cuts every task into *parts* at task creation, parallel fork/join
and synchronisation points. Parts are weighted with the work measured
between the cuts and connected by: sequence within a task, creation, `depend`
dependences, and joins (children → `taskwait`, group members → `taskgroup`
end, team and its tasks → barrier and region end). The critical path is the
longest path through this acyclic graph; T₁/T∞ is the average parallelism
the graph offers independent of the number of threads used. For a selected
region the same computation runs on the parts of the selected tasks only.
See `crates/temanejo/src/model.rs` and `analysis.rs`.

## Layout

```
crates/tmj-trace   trace file format (shared)
crates/tmj-ompt    OMPT tool → libtmj_ompt.so
crates/temanejo    server: model.rs (graph reconstruction), analysis.rs (critical path,
                   parallelism), layout.rs, symbolize.rs, main.rs (HTTP + JSON), web/ (GUI)
examples/          MPI + OpenMP test program (dependences, recursion + taskwait,
                   nested parallel regions with taskgroup/taskloop)
scripts/tmj-run    launcher wrapper
```

## Status and known limitations

Tested with clang/libomp 23.1 and Open MPI 5.0.11 on one node: 2 ranks × 4
threads, 12 000 and 15 000 tasks per rank. There are no automated tests yet.

- Post mortem only; no live view and no execution control (breakpoints,
  stepping) as in the original Temanejo.
- Events stay in memory until the runtime shuts down (56 bytes per event,
  roughly 5 events per task). A program that aborts or calls `_exit` leaves
  no trace.
- The MPI rank is read from `PMIX_RANK`, `OMPI_COMM_WORLD_RANK` or
  `PMI_RANK`. Without any of them every process writes `rank-0.tmj`.
- Clocks of different ranks are not aligned; each rank's time starts at its
  first OMPT event. MPI calls are not recorded, so time blocked in MPI counts
  as work of the task that made the call.
- Barrier joins assume every thread of a team passes the same sequence of
  barriers (no `cancel`). `detach`, target tasks and `taskwait depend(...)`
  pseudo tasks are recorded but not treated specially.
- The layered layout is simple (barycenter ordering, wide layers wrapped);
  very dense dependence graphs become hard to read when zoomed out.
- The server keeps all ranks in memory and has no authentication; it binds
  to localhost by default.
- libomp 23.1 was seen to deadlock on `untied` tasks combined with
  `taskwait` in deep recursion, with and without the tool; the example
  therefore uses tied tasks only, and task migration between threads is
  implemented but untested.
