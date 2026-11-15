//! OMPT tool recording the task graph of an OpenMP (+MPI) program.
//!
//! Activate with `OMP_TOOL_LIBRARIES=/path/to/libtmj_ompt.so`. Each process
//! writes `<TEMANEJO_TRACE_DIR>/rank-<N>.tmj`: a thread appends its events
//! as a block whenever its buffer is full (`TEMANEJO_BUFFER_SIZE`) or older
//! than `TEMANEJO_FLUSH_INTERVAL`, and everything left when the OpenMP
//! runtime shuts down. The MPI rank is taken from the launcher environment (PMIx / Open MPI),
//! so the tool has no link-time dependency on any MPI library.
//!
//! Callbacks only append fixed-size records to a per-thread buffer, which is
//! written out in one piece; all graph reconstruction happens post mortem in
//! the `temanejo` server.

#![allow(clippy::missing_safety_doc)]

use std::cell::Cell;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;

use tmj_trace::{kind, Event, Sym};

// ---------------------------------------------------------------- OMPT FFI

#[repr(C)]
#[derive(Clone, Copy)]
pub union OmptData {
    value: u64,
    ptr: *mut c_void,
}

#[repr(C)]
pub struct OmptDependence {
    variable: OmptData,
    dependence_type: c_int,
}

type Callback = unsafe extern "C" fn();
type Lookup = unsafe extern "C" fn(*const c_char) -> Option<Callback>;
type SetCallback = unsafe extern "C" fn(c_int, Option<Callback>) -> c_int;

#[repr(C)]
pub struct StartToolResult {
    initialize: unsafe extern "C" fn(Lookup, c_int, *mut OmptData) -> c_int,
    finalize: unsafe extern "C" fn(*mut OmptData),
    tool_data: OmptData,
}

// ompt_callbacks_t values (omp-tools.h)
const CB_THREAD_BEGIN: c_int = 1;
const CB_THREAD_END: c_int = 2;
const CB_PARALLEL_BEGIN: c_int = 3;
const CB_PARALLEL_END: c_int = 4;
const CB_TASK_CREATE: c_int = 5;
const CB_TASK_SCHEDULE: c_int = 6;
const CB_IMPLICIT_TASK: c_int = 7;
const CB_SYNC_REGION_WAIT: c_int = 16;
const CB_WORK: c_int = 20;
const CB_DEPENDENCES: c_int = 18;
const CB_TASK_DEPENDENCE: c_int = 19;
const CB_SYNC_REGION: c_int = 23;

// ---------------------------------------------------------------- libc FFI

#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

#[repr(C)]
struct DlInfo {
    dli_fname: *const c_char,
    dli_fbase: *mut c_void,
    dli_sname: *const c_char,
    dli_saddr: *mut c_void,
}

const CLOCK_REALTIME: c_int = 0;
const CLOCK_MONOTONIC: c_int = 1;
const RTLD_DL_LINKMAP: c_int = 2;

extern "C" {
    fn clock_gettime(clk: c_int, ts: *mut Timespec) -> c_int;
    fn getcpu(cpu: *mut c_uint, node: *mut c_uint) -> c_int;
    fn gettid() -> c_int;
    fn gethostname(name: *mut c_char, len: usize) -> c_int;
    fn dladdr1(addr: *const c_void, info: *mut DlInfo, extra: *mut *mut c_void, flags: c_int) -> c_int;
    fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
    fn backtrace(buffer: *mut *mut c_void, size: c_int) -> c_int;
}

#[inline]
fn now(clk: c_int) -> u64 {
    let mut ts = Timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { clock_gettime(clk, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

// ---------------------------------------------------------------- recording

struct ThreadBuf {
    id: u32,
    events: Vec<Event>,
    /// Timestamp of the oldest buffered event (for `TEMANEJO_FLUSH_INTERVAL`).
    oldest: u64,
    /// Code addresses this thread has already passed on for symbol resolution.
    seen: HashSet<u64>,
}

/// The trace file and what has been written to it; shared by all threads.
struct Output {
    file: std::fs::File,
    path: String,
    rank: u32,
    events: u64,
    blocks: u64,
    /// Code addresses whose symbol record is in the file.
    symbols: HashSet<u64>,
    failed: bool,
}

static OUTPUT: Mutex<Option<Output>> = Mutex::new(None);
/// What the runtime passed to `ompt_start_tool`: its `_OPENMP` version and version string.
static STARTED_BY: Mutex<(c_uint, String)> = Mutex::new((0, String::new()));
/// Events a thread buffers before it writes them (`usize::MAX`: until the end).
static BUFFER_EVENTS: AtomicUsize = AtomicUsize::new(DEFAULT_BUFFER_BYTES / tmj_trace::EVENT_SIZE);
/// Maximum age of a buffered event in ns before the buffer is written (0: no limit).
static FLUSH_INTERVAL_NS: AtomicU64 = AtomicU64::new(0);
static SYNC: AtomicBool = AtomicBool::new(false);

const DEFAULT_BUFFER_BYTES: usize = 1 << 20;
const MIN_BUFFER_EVENTS: usize = 64;

/// Writes the events of `tb` as one block, preceded by the symbols of code
/// addresses that are new in it, and empties the buffer. Called by the
/// owning thread, and by `finalize` for the buffers of other threads.
fn flush(tb: &mut ThreadBuf) {
    if tb.events.is_empty() {
        return;
    }
    let mut new_addrs = Vec::new();
    for e in &tb.events {
        let code = matches!(e.kind, kind::PARALLEL_BEGIN | kind::TASK_CREATE | kind::SYNC_REGION | kind::SYNC_WAIT | kind::WORK);
        if code && e.d != 0 && tb.seen.insert(e.d) {
            new_addrs.push(e.d);
        }
    }
    let mut guard = OUTPUT.lock().unwrap();
    if let Some(out) = guard.as_mut() {
        let mut bytes = Vec::new();
        new_addrs.retain(|a| out.symbols.insert(*a));
        if !new_addrs.is_empty() {
            let syms: Vec<Sym> = new_addrs.iter().map(|&a| unsafe { resolve(a) }).collect();
            tmj_trace::encode_symbols_block(&mut bytes, &syms);
        }
        tmj_trace::encode_events_block(&mut bytes, &tb.events);
        let mut result = out.file.write_all(&bytes);
        if result.is_ok() && SYNC.load(Relaxed) {
            result = out.file.sync_data();
        }
        match result {
            Ok(()) => {
                out.events += tb.events.len() as u64;
                out.blocks += 1;
            }
            Err(e) if !out.failed => {
                out.failed = true;
                eprintln!("[temanejo] rank {}: cannot write {}: {e}; events are lost", out.rank, out.path);
            }
            Err(_) => {}
        }
    }
    drop(guard);
    tb.events.clear();
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static EPOCH_NS: AtomicU64 = AtomicU64::new(0);
/// Load addresses of the OpenMP runtime and of this tool, to recognise code
/// addresses that do not belong to the program.
static RUNTIME_BASE: AtomicU64 = AtomicU64::new(0);
static TOOL_BASE: AtomicU64 = AtomicU64::new(0);

/// Base address of the loaded module containing `addr`, 0 if none.
fn module_base(addr: u64) -> u64 {
    let mut info = DlInfo {
        dli_fname: std::ptr::null(),
        dli_fbase: std::ptr::null_mut(),
        dli_sname: std::ptr::null(),
        dli_saddr: std::ptr::null_mut(),
    };
    if unsafe { dladdr(addr as *const c_void, &mut info) } == 0 { 0 } else { info.dli_fbase as u64 }
}

fn in_runtime(addr: u64) -> bool {
    let rt = RUNTIME_BASE.load(Relaxed);
    rt != 0 && module_base(addr) == rt
}

/// First return address on the current stack that lies neither in this tool
/// nor in the OpenMP runtime: the call site of the construct in the program.
fn program_caller() -> Option<u64> {
    let mut frames = [std::ptr::null_mut::<c_void>(); 48];
    let n = unsafe { backtrace(frames.as_mut_ptr(), frames.len() as c_int) }.max(0) as usize;
    let (rt, tool) = (RUNTIME_BASE.load(Relaxed), TOOL_BASE.load(Relaxed));
    frames[..n].iter().map(|&p| p as u64).find(|&a| {
        let b = module_base(a);
        b != 0 && b != rt && b != tool
    })
}
static NEXT_THREAD: AtomicU32 = AtomicU32::new(0);
static NEXT_TASK: AtomicU64 = AtomicU64::new(1);
static NEXT_PARALLEL: AtomicU64 = AtomicU64::new(1);
/// Addresses of all leaked `ThreadBuf`s; read once in `finalize`.
static REGISTRY: Mutex<Vec<usize>> = Mutex::new(Vec::new());

thread_local! {
    // No destructor on purpose: OpenMP worker threads may outlive finalize.
    static TB: Cell<*mut ThreadBuf> = const { Cell::new(std::ptr::null_mut()) };
}

#[inline]
fn thread_buf() -> &'static mut ThreadBuf {
    TB.with(|c| {
        let mut p = c.get();
        if p.is_null() {
            p = Box::into_raw(Box::new(ThreadBuf {
                id: NEXT_THREAD.fetch_add(1, Relaxed),
                events: Vec::with_capacity(BUFFER_EVENTS.load(Relaxed).min(1 << 14)),
                oldest: 0,
                seen: HashSet::new(),
            }));
            REGISTRY.lock().unwrap().push(p as usize);
            c.set(p);
        }
        // Only the owning thread writes; finalize reads after ACTIVE is cleared.
        unsafe { &mut *p }
    })
}

#[inline]
fn rec(kind: u8, endpoint: u8, flags: u32, a: u64, b: u64, c: u64, d: u64) {
    if !ACTIVE.load(Relaxed) {
        return;
    }
    let (mut cpu, mut node) = (0 as c_uint, 0 as c_uint);
    unsafe { getcpu(&mut cpu, &mut node) };
    let tb = thread_buf();
    let ts = now(CLOCK_MONOTONIC);
    if tb.events.is_empty() {
        tb.oldest = ts;
    }
    tb.events.push(Event { kind, endpoint, cpu: cpu as u16, numa: node as u16, thread: tb.id, flags, ts, a, b, c, d });
    let interval = FLUSH_INTERVAL_NS.load(Relaxed);
    if tb.events.len() >= BUFFER_EVENTS.load(Relaxed) || (interval != 0 && ts - tb.oldest >= interval) {
        flush(tb);
    }
}

#[inline]
unsafe fn id_of(d: *mut OmptData) -> u64 {
    if d.is_null() { 0 } else { (*d).value }
}

/// Returns the id stored in `d`, assigning a fresh one if it has none yet.
#[inline]
unsafe fn id_or_new(d: *mut OmptData, counter: &AtomicU64) -> u64 {
    if d.is_null() {
        return 0;
    }
    if (*d).value == 0 {
        (*d).value = counter.fetch_add(1, Relaxed);
    }
    (*d).value
}

// ---------------------------------------------------------------- callbacks

unsafe extern "C" fn on_thread_begin(thread_type: c_int, _thread_data: *mut OmptData) {
    rec(kind::THREAD_BEGIN, 0, 0, thread_type as u64, gettid() as u64, 0, 0);
}

unsafe extern "C" fn on_thread_end(_thread_data: *mut OmptData) {
    rec(kind::THREAD_END, 0, 0, 0, 0, 0, 0);
    if ACTIVE.load(Relaxed) {
        flush(thread_buf());
    }
}

unsafe extern "C" fn on_parallel_begin(
    encountering_task: *mut OmptData,
    _frame: *const c_void,
    parallel: *mut OmptData,
    requested: c_uint,
    flags: c_int,
    codeptr: *const c_void,
) {
    let pid = id_or_new(parallel, &NEXT_PARALLEL);
    rec(kind::PARALLEL_BEGIN, 0, flags as u32, pid, id_of(encountering_task), requested as u64, codeptr as u64);
}

unsafe extern "C" fn on_parallel_end(
    parallel: *mut OmptData,
    encountering_task: *mut OmptData,
    flags: c_int,
    _codeptr: *const c_void,
) {
    rec(kind::PARALLEL_END, 0, flags as u32, id_of(parallel), id_of(encountering_task), 0, 0);
}

unsafe extern "C" fn on_implicit_task(
    endpoint: c_int,
    parallel: *mut OmptData,
    task: *mut OmptData,
    actual_parallelism: c_uint,
    index: c_uint,
    flags: c_int,
) {
    // libomp passes no parallel data on the end event, so ids are only
    // assigned on begin; the analysis remembers the task's region.
    let (tid, pid) = if endpoint == tmj_trace::ENDPOINT_BEGIN as c_int {
        (id_or_new(task, &NEXT_TASK), id_or_new(parallel, &NEXT_PARALLEL))
    } else {
        (id_of(task), id_of(parallel))
    };
    rec(kind::IMPLICIT_TASK, endpoint as u8, flags as u32, tid, pid, index as u64, actual_parallelism as u64);
}

unsafe extern "C" fn on_task_create(
    encountering_task: *mut OmptData,
    _frame: *const c_void,
    new_task: *mut OmptData,
    flags: c_int,
    _has_dependences: c_int,
    codeptr: *const c_void,
) {
    let id = id_or_new(new_task, &NEXT_TASK);
    rec(kind::TASK_CREATE, 0, flags as u32, id, id_of(encountering_task), 0, codeptr as u64);
}

unsafe extern "C" fn on_task_schedule(prior: *mut OmptData, status: c_int, next: *mut OmptData) {
    rec(kind::TASK_SCHEDULE, 0, 0, id_of(prior), status as u64, id_of(next), 0);
}

unsafe extern "C" fn on_dependences(task: *mut OmptData, deps: *const OmptDependence, ndeps: c_int) {
    let id = id_of(task);
    for i in 0..ndeps.max(0) as usize {
        let d = &*deps.add(i);
        rec(kind::DEPENDENCE_VAR, 0, 0, id, d.variable.value, d.dependence_type as u64, 0);
    }
}

unsafe extern "C" fn on_task_dependence(src: *mut OmptData, sink: *mut OmptData) {
    rec(kind::TASK_DEPENDENCE, 0, 0, id_of(src), id_of(sink), 0, 0);
}

unsafe extern "C" fn on_sync_region(
    sync_kind: c_int,
    endpoint: c_int,
    parallel: *mut OmptData,
    task: *mut OmptData,
    codeptr: *const c_void,
) {
    rec(kind::SYNC_REGION, endpoint as u8, 0, sync_kind as u64, id_of(task), id_of(parallel), codeptr as u64);
}

unsafe extern "C" fn on_work(
    work_type: c_int,
    endpoint: c_int,
    _parallel: *mut OmptData,
    task: *mut OmptData,
    count: u64,
    codeptr: *const c_void,
) {
    let mut d = codeptr as u64;
    let mut flags = 0;
    // libomp reports an address inside __kmpc_taskloop for taskloop constructs.
    // Taskloop begin/end events are rare (one per construct instance), so
    // unwinding the stack here is cheap.
    if work_type as u64 == tmj_trace::WORK_TASKLOOP && d != 0 && in_runtime(d) {
        if let Some(a) = program_caller() {
            d = a;
            flags = tmj_trace::WORK_CODEPTR_UNWOUND;
        }
    }
    rec(kind::WORK, endpoint as u8, flags, work_type as u64, id_of(task), count, d);
}

unsafe extern "C" fn on_sync_region_wait(
    sync_kind: c_int,
    endpoint: c_int,
    parallel: *mut OmptData,
    task: *mut OmptData,
    codeptr: *const c_void,
) {
    rec(kind::SYNC_WAIT, endpoint as u8, 0, sync_kind as u64, id_of(task), id_of(parallel), codeptr as u64);
}

// ---------------------------------------------------------------- tool entry

fn verbose() -> bool {
    std::env::var_os("TEMANEJO_VERBOSE").is_some()
}

unsafe extern "C" fn initialize(lookup: Lookup, _initial_device_num: c_int, _tool_data: *mut OmptData) -> c_int {
    let Some(f) = lookup(c"ompt_set_callback".as_ptr()) else {
        return 0;
    };
    let set: SetCallback = std::mem::transmute(f);
    macro_rules! register {
        ($id:expr, $f:expr) => {{
            let cb: Callback = std::mem::transmute($f as *const c_void);
            let r = set($id, Some(cb));
            // ompt_set_error = 0, ompt_set_never = 1
            if r <= 1 && verbose() {
                eprintln!("[temanejo] OMPT callback {} is not available in this runtime", $id);
            }
        }};
    }
    register!(CB_THREAD_BEGIN, on_thread_begin);
    register!(CB_THREAD_END, on_thread_end);
    register!(CB_PARALLEL_BEGIN, on_parallel_begin);
    register!(CB_PARALLEL_END, on_parallel_end);
    register!(CB_IMPLICIT_TASK, on_implicit_task);
    register!(CB_TASK_CREATE, on_task_create);
    register!(CB_TASK_SCHEDULE, on_task_schedule);
    register!(CB_DEPENDENCES, on_dependences);
    register!(CB_TASK_DEPENDENCE, on_task_dependence);
    register!(CB_SYNC_REGION, on_sync_region);
    register!(CB_SYNC_REGION_WAIT, on_sync_region_wait);
    register!(CB_WORK, on_work);

    RUNTIME_BASE.store(module_base(lookup as usize as u64), Relaxed);
    TOOL_BASE.store(module_base(on_work as *const () as usize as u64), Relaxed);
    // The first backtrace() call loads the unwinder; do it outside callbacks.
    let mut warm = [std::ptr::null_mut::<c_void>(); 2];
    backtrace(warm.as_mut_ptr(), 2);
    EPOCH_NS.store(now(CLOCK_REALTIME), Relaxed);
    configure();
    if let Err(msg) = open_output(lookup) {
        eprintln!("[temanejo] {msg}; recording is disabled");
        return 0; // zero deactivates the tool
    }
    ACTIVE.store(true, Relaxed);
    1 // non-zero keeps the tool active
}

/// Parses a size like `4096`, `256K`, `4M` or `1G` (bytes, powers of 1024).
fn parse_size(v: &str) -> Option<usize> {
    let v = v.trim();
    let (num, mult) = match v.chars().last()?.to_ascii_uppercase() {
        'K' => (&v[..v.len() - 1], 1 << 10),
        'M' => (&v[..v.len() - 1], 1 << 20),
        'G' => (&v[..v.len() - 1], 1 << 30),
        _ => (v, 1),
    };
    num.trim().parse::<usize>().ok()?.checked_mul(mult)
}

/// Reads the buffering parameters from the environment.
fn configure() {
    if let Ok(v) = std::env::var("TEMANEJO_BUFFER_SIZE") {
        let events = match v.trim().to_ascii_lowercase().as_str() {
            "unlimited" | "0" => Some(usize::MAX),
            _ => parse_size(&v).map(|bytes| (bytes / tmj_trace::EVENT_SIZE).max(MIN_BUFFER_EVENTS)),
        };
        match events {
            Some(n) => BUFFER_EVENTS.store(n, Relaxed),
            None => eprintln!("[temanejo] ignoring TEMANEJO_BUFFER_SIZE={v}: expected a size like 256K, 4M or 'unlimited'"),
        }
    }
    if let Ok(v) = std::env::var("TEMANEJO_FLUSH_INTERVAL") {
        match v.trim().parse::<f64>() {
            Ok(s) if s >= 0.0 && s.is_finite() => FLUSH_INTERVAL_NS.store((s * 1e9) as u64, Relaxed),
            _ => eprintln!("[temanejo] ignoring TEMANEJO_FLUSH_INTERVAL={v}: expected seconds, e.g. 0.5"),
        }
    }
    SYNC.store(std::env::var("TEMANEJO_SYNC").is_ok_and(|v| !matches!(v.trim(), "" | "0")), Relaxed);
}

/// Information about the run for the trace header. Every entry is optional:
/// what cannot be determined is left out.
unsafe fn run_info(lookup: Lookup) -> Vec<(String, String)> {
    let mut info: Vec<(String, String)> = Vec::new();
    let mut add = |key: &str, value: String| {
        if !value.is_empty() {
            info.push((key.to_string(), value));
        }
    };
    add("tool.version", env!("CARGO_PKG_VERSION").to_string());

    // The OpenMP runtime, as it introduced itself.
    let (omp_version, runtime_version) = STARTED_BY.lock().unwrap().clone();
    add("omp.runtime_version", runtime_version);
    if omp_version != 0 {
        add("omp.version", omp_version.to_string()); // the runtime's _OPENMP value, yyyymm
    }
    // OMPT entry point; safe to call during tool initialisation.
    if let Some(f) = lookup(c"ompt_get_num_procs".as_ptr()) {
        let num_procs: unsafe extern "C" fn() -> c_int = std::mem::transmute(f);
        let n = num_procs();
        if n > 0 {
            add("omp.num_procs", n.to_string());
        }
    }

    // The process.
    if let Ok(exe) = std::fs::read_link("/proc/self/exe") {
        add("process.exe", exe.to_string_lossy().into_owned());
    }
    let args: Vec<String> = std::env::args().collect();
    add("process.args", args.join(" "));
    if let Ok(cwd) = std::env::current_dir() {
        add("process.cwd", cwd.to_string_lossy().into_owned());
    }
    // CPUs the process may run on, e.g. "0-3,8": shows how the launcher bound it.
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        if let Some(line) = status.lines().find_map(|l| l.strip_prefix("Cpus_allowed_list:")) {
            add("process.cpus_allowed", line.trim().to_string());
        }
    }
    for (key, vars) in [("mpi.world_size", ["OMPI_COMM_WORLD_SIZE", "PMI_SIZE"])] {
        if let Some(v) = vars.iter().find_map(|v| std::env::var(v).ok()) {
            add(key, v);
        }
    }

    // Environment variables that configure OpenMP runtimes and this tool.
    let mut env: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.to_string_lossy().into_owned())))
        .filter(|(k, _)| ["OMP_", "KMP_", "GOMP_", "LIBOMP_", "TEMANEJO_"].iter().any(|p| k.starts_with(p)))
        .collect();
    env.sort();
    for (k, v) in env {
        info.push((format!("env.{k}"), v)); // an empty value is information too
    }
    info
}

/// Creates the trace file and writes its header.
unsafe fn open_output(lookup: Lookup) -> Result<(), String> {
    let runtime_addr = lookup as usize as u64;
    let rank = mpi_rank();
    let dir = std::env::var("TEMANEJO_TRACE_DIR").unwrap_or_else(|_| "tmj-trace".into());
    let path = format!("{dir}/rank-{rank}.tmj");
    let mut host = [0 as c_char; 256];
    gethostname(host.as_mut_ptr(), host.len() - 1);
    let mut header = Vec::new();
    tmj_trace::encode_header(&mut header, rank, std::process::id(), EPOCH_NS.load(Relaxed), &cstr(host.as_ptr()), &resolve(runtime_addr).module, &run_info(lookup));
    let mut file = std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::File::create(&path))
        .map_err(|e| format!("rank {rank}: cannot create {path}: {e}"))?;
    file.write_all(&header).map_err(|e| format!("rank {rank}: cannot write {path}: {e}"))?;
    if verbose() {
        let buffer = match BUFFER_EVENTS.load(Relaxed) {
            usize::MAX => "unlimited (written at the end)".to_string(),
            n => format!("{n} events ({} KiB) per thread", n * tmj_trace::EVENT_SIZE / 1024),
        };
        let interval = match FLUSH_INTERVAL_NS.load(Relaxed) {
            0 => "off".to_string(),
            ns => format!("{} s", ns as f64 / 1e9),
        };
        eprintln!("[temanejo] rank {rank}: writing {path}; buffer {buffer}, flush interval {interval}, sync {}", if SYNC.load(Relaxed) { "on" } else { "off" });
    }
    *OUTPUT.lock().unwrap() = Some(Output { file, path, rank, events: 0, blocks: 0, symbols: HashSet::new(), failed: false });
    Ok(())
}

fn mpi_rank() -> u32 {
    // Set by PRRTE/PMIx (Open MPI 5), Open MPI itself, and Hydra/Slurm PMI.
    for var in ["PMIX_RANK", "OMPI_COMM_WORLD_RANK", "PMI_RANK"] {
        if let Some(r) = std::env::var(var).ok().and_then(|v| v.trim().parse().ok()) {
            return r;
        }
    }
    0
}

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
}

unsafe fn resolve(addr: u64) -> Sym {
    let mut info = DlInfo {
        dli_fname: std::ptr::null(),
        dli_fbase: std::ptr::null_mut(),
        dli_sname: std::ptr::null(),
        dli_saddr: std::ptr::null_mut(),
    };
    let mut link_map: *mut c_void = std::ptr::null_mut();
    let mut sym = Sym { addr, ..Default::default() };
    if dladdr1(addr as *const c_void, &mut info, &mut link_map, RTLD_DL_LINKMAP) != 0 {
        sym.symbol = cstr(info.dli_sname);
        let mut module = cstr(info.dli_fname);
        // struct link_map starts with `ElfW(Addr) l_addr`, the load bias.
        if !link_map.is_null() {
            sym.load_bias = *(link_map as *const usize) as u64;
        }
        // The main program is reported as argv[0]; make it an absolute path.
        if let Ok(p) = std::fs::canonicalize(&module) {
            module = p.to_string_lossy().into_owned();
        } else if let Ok(p) = std::fs::read_link("/proc/self/exe") {
            module = p.to_string_lossy().into_owned();
        }
        sym.module = module;
    }
    sym
}

/// Writes what is still buffered and the end block that marks the trace as
/// complete. Events recorded by other threads after this point are dropped.
unsafe extern "C" fn finalize(_tool_data: *mut OmptData) {
    if !ACTIVE.swap(false, Relaxed) {
        return;
    }
    let bufs: Vec<usize> = REGISTRY.lock().unwrap().clone();
    for p in bufs {
        flush(&mut *(p as *mut ThreadBuf));
    }
    let Some(mut out) = OUTPUT.lock().unwrap().take() else { return };
    let mut end = Vec::new();
    tmj_trace::encode_end_block(&mut end, out.events);
    let result = out.file.write_all(&end).and_then(|_| out.file.sync_all());
    match result {
        Ok(()) if out.failed => {}
        Ok(()) if verbose() => eprintln!("[temanejo] rank {}: {} events in {} blocks -> {}", out.rank, out.events, out.blocks, out.path),
        Ok(()) => {}
        Err(e) => eprintln!("[temanejo] rank {}: cannot finish {}: {e}", out.rank, out.path),
    }
}

/// Entry point looked up by the OpenMP runtime (OpenMP 5.x, section "OMPT tool initialization").
#[no_mangle]
pub unsafe extern "C" fn ompt_start_tool(omp_version: c_uint, runtime_version: *const c_char) -> *mut StartToolResult {
    *STARTED_BY.lock().unwrap() = (omp_version, cstr(runtime_version));
    if verbose() {
        eprintln!("[temanejo] OMPT tool attached to runtime: {}", cstr(runtime_version));
    }
    Box::into_raw(Box::new(StartToolResult { initialize, finalize, tool_data: OmptData { value: 0 } }))
}
