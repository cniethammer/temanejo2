//! Trace file format: one file per (MPI) process, `rank-<N>.tmj`.
//!
//! Version 03 is written in blocks, so a process can append to its file
//! while it runs (all integers little endian):
//!
//! ```text
//! magic "TMJTRC03"
//! u32 rank, u32 pid, u64 epoch_ns (CLOCK_REALTIME at tool init),
//! str host, str runtime_module (path of the OpenMP runtime library),
//! u32 n_info, n_info * { str key, str value } (optional run information)
//! blocks, each starting with a type byte, until the end of the file:
//!   1 events:  u32 n, n * Event (56 bytes each)
//!   2 symbols: u32 n, n * { u64 addr, u64 load_bias, str module, str symbol }
//!   3 end:     u64 total number of events; written when the run finished
//! ```
//! `str` is a u32 length followed by UTF-8 bytes. A file without end block,
//! or cut off inside a block, comes from a run that did not finish; what was
//! written is still used.
//!
//! Version 02 (`TMJTRC02`) is version 03 without the information entries.
//! Version 01 (`TMJTRC01`: header without runtime_module, then `u64 n` and
//! all events, then `u32 n` and all symbols) and version 02 are still read.
//!
//! The full specification is in `docs/trace-format.rst`.

use std::io::{self, Read, Write};

pub const MAGIC: &[u8; 8] = b"TMJTRC03";
/// Magic of version 02: chunked like version 03, header without information entries.
pub const MAGIC_V2: &[u8; 8] = b"TMJTRC02";
/// Magic of the previous, unchunked format, which is still read.
pub const MAGIC_V1: &[u8; 8] = b"TMJTRC01";

pub const BLOCK_EVENTS: u8 = 1;
pub const BLOCK_SYMBOLS: u8 = 2;
pub const BLOCK_END: u8 = 3;

/// Event kinds. The meaning of the generic payload fields `a..d` is given per kind.
pub mod kind {
    /// a = ompt thread type, b = OS tid
    pub const THREAD_BEGIN: u8 = 1;
    pub const THREAD_END: u8 = 2;
    /// a = parallel id, b = encountering task, c = requested parallelism, d = codeptr
    pub const PARALLEL_BEGIN: u8 = 3;
    /// a = parallel id, b = encountering task
    pub const PARALLEL_END: u8 = 4;
    /// a = new task, b = encountering task, d = codeptr; flags = ompt task flags.
    /// After reading (`Trace::attribute_taskloop_tasks`): c = codeptr as
    /// reported by the runtime if it pointed into the runtime and `d` was
    /// replaced by the codeptr of the enclosing taskloop, else 0.
    pub const TASK_CREATE: u8 = 5;
    /// a = prior task, b = prior task status, c = next task
    pub const TASK_SCHEDULE: u8 = 6;
    /// endpoint; a = task, b = parallel id, c = thread index in team,
    /// d = actual parallelism; flags = ompt task flags
    pub const IMPLICIT_TASK: u8 = 7;
    /// a = task, b = variable address, c = ompt dependence type
    pub const DEPENDENCE_VAR: u8 = 8;
    /// a = predecessor task, b = successor task
    pub const TASK_DEPENDENCE: u8 = 9;
    /// endpoint; a = ompt sync region kind, b = task, c = parallel id, d = codeptr
    pub const SYNC_REGION: u8 = 10;
    /// Same payload as SYNC_REGION; brackets the time actually spent waiting.
    pub const SYNC_WAIT: u8 = 11;
    /// endpoint; a = ompt work type, b = task, c = count, d = codeptr
    pub const WORK: u8 = 12;
}

/// `ompt_work_t` value of a taskloop construct.
pub const WORK_TASKLOOP: u64 = 7;

/// `flags` bit of a WORK event: the runtime reported a code address inside
/// itself, and `d` was found by unwinding the stack instead.
pub const WORK_CODEPTR_UNWOUND: u32 = 0x1;

pub const ENDPOINT_BEGIN: u8 = 1;
pub const ENDPOINT_END: u8 = 2;

#[derive(Clone, Copy, Debug, Default)]
pub struct Event {
    pub kind: u8,
    pub endpoint: u8,
    /// Hardware thread (logical CPU) the calling thread was on when the event fired.
    pub cpu: u16,
    /// NUMA node of that CPU.
    pub numa: u16,
    /// Tool-assigned dense thread id.
    pub thread: u32,
    pub flags: u32,
    /// CLOCK_MONOTONIC nanoseconds.
    pub ts: u64,
    pub a: u64,
    pub b: u64,
    pub c: u64,
    pub d: u64,
}

pub const EVENT_SIZE: usize = 56;

impl Event {
    pub fn encode(&self, out: &mut [u8; EVENT_SIZE]) {
        out[0] = self.kind;
        out[1] = self.endpoint;
        out[2..4].copy_from_slice(&self.cpu.to_le_bytes());
        out[4..6].copy_from_slice(&self.numa.to_le_bytes());
        out[6..8].copy_from_slice(&[0, 0]);
        out[8..12].copy_from_slice(&self.thread.to_le_bytes());
        out[12..16].copy_from_slice(&self.flags.to_le_bytes());
        out[16..24].copy_from_slice(&self.ts.to_le_bytes());
        out[24..32].copy_from_slice(&self.a.to_le_bytes());
        out[32..40].copy_from_slice(&self.b.to_le_bytes());
        out[40..48].copy_from_slice(&self.c.to_le_bytes());
        out[48..56].copy_from_slice(&self.d.to_le_bytes());
    }

    pub fn decode(b: &[u8; EVENT_SIZE]) -> Event {
        let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        Event {
            kind: b[0],
            endpoint: b[1],
            cpu: u16_at(2),
            numa: u16_at(4),
            thread: u32_at(8),
            flags: u32_at(12),
            ts: u64_at(16),
            a: u64_at(24),
            b: u64_at(32),
            c: u64_at(40),
            d: u64_at(48),
        }
    }
}

/// A code address seen in the trace, with what the dynamic loader knew about it.
#[derive(Clone, Debug, Default)]
pub struct Sym {
    pub addr: u64,
    /// Load bias of the containing module (`addr - load_bias` is the link-time address).
    pub load_bias: u64,
    pub module: String,
    pub symbol: String,
}

#[derive(Clone, Debug, Default)]
pub struct Trace {
    pub rank: u32,
    pub pid: u32,
    pub epoch_ns: u64,
    pub host: String,
    pub events: Vec<Event>,
    pub syms: Vec<Sym>,
    /// Path of the OpenMP runtime library (empty in version 01 files).
    pub runtime_module: String,
    /// Optional information about the run as key/value pairs, in file order
    /// (see `docs/trace-format.rst` for the keys the tool writes). Only what
    /// was available is present; readers must not rely on any key.
    pub info: Vec<(String, String)>,
    /// False if the file has no end block: the run did not finish cleanly
    /// and events are missing from some point on.
    pub complete: bool,
}

fn write_str<W: Write>(w: &mut W, s: &str) -> io::Result<()> {
    w.write_all(&(s.len() as u32).to_le_bytes())?;
    w.write_all(s.as_bytes())
}

fn read_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64<R: Read>(r: &mut R) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

fn read_str<R: Read>(r: &mut R) -> io::Result<String> {
    let n = read_u32(r)? as usize;
    if n > 1 << 20 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "string too long"));
    }
    let mut b = vec![0u8; n];
    r.read_exact(&mut b)?;
    Ok(String::from_utf8_lossy(&b).into_owned())
}

/// File header of a version 03 trace.
pub fn encode_header(out: &mut Vec<u8>, rank: u32, pid: u32, epoch_ns: u64, host: &str, runtime_module: &str, info: &[(String, String)]) {
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&rank.to_le_bytes());
    out.extend_from_slice(&pid.to_le_bytes());
    out.extend_from_slice(&epoch_ns.to_le_bytes());
    write_str(out, host).unwrap();
    write_str(out, runtime_module).unwrap();
    out.extend_from_slice(&(info.len() as u32).to_le_bytes());
    for (key, value) in info {
        write_str(out, key).unwrap();
        // Values are cut to what readers accept as a string.
        let mut end = value.len().min(MAX_INFO_VALUE);
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        write_str(out, &value[..end]).unwrap();
    }
}

/// Longest value of an information entry in bytes.
pub const MAX_INFO_VALUE: usize = 64 << 10;

/// Appends an events block. Events of one thread must be written in the
/// order they were recorded; blocks of different threads may interleave.
pub fn encode_events_block(out: &mut Vec<u8>, events: &[Event]) {
    out.reserve(5 + events.len() * EVENT_SIZE);
    out.push(BLOCK_EVENTS);
    out.extend_from_slice(&(events.len() as u32).to_le_bytes());
    let mut buf = [0u8; EVENT_SIZE];
    for e in events {
        e.encode(&mut buf);
        out.extend_from_slice(&buf);
    }
}

pub fn encode_symbols_block(out: &mut Vec<u8>, syms: &[Sym]) {
    out.push(BLOCK_SYMBOLS);
    out.extend_from_slice(&(syms.len() as u32).to_le_bytes());
    for s in syms {
        out.extend_from_slice(&s.addr.to_le_bytes());
        out.extend_from_slice(&s.load_bias.to_le_bytes());
        write_str(out, &s.module).unwrap();
        write_str(out, &s.symbol).unwrap();
    }
}

/// Appends the end block, which marks the trace as complete.
pub fn encode_end_block(out: &mut Vec<u8>, total_events: u64) {
    out.push(BLOCK_END);
    out.extend_from_slice(&total_events.to_le_bytes());
}

fn read_events<R: Read>(r: &mut R, n: usize, events: &mut Vec<Event>) -> io::Result<()> {
    events.reserve(n.min(1 << 24));
    let mut buf = [0u8; EVENT_SIZE];
    for _ in 0..n {
        r.read_exact(&mut buf)?;
        events.push(Event::decode(&buf));
    }
    Ok(())
}

fn read_syms<R: Read>(r: &mut R, n: usize, syms: &mut Vec<Sym>) -> io::Result<()> {
    for _ in 0..n {
        let addr = read_u64(r)?;
        let load_bias = read_u64(r)?;
        let module = read_str(r)?;
        let symbol = read_str(r)?;
        syms.push(Sym { addr, load_bias, module, symbol });
    }
    Ok(())
}

impl Trace {
    /// Writes the trace as a version 03 file with one events block.
    pub fn write<W: Write>(&self, w: &mut W) -> io::Result<()> {
        let mut out = Vec::new();
        encode_header(&mut out, self.rank, self.pid, self.epoch_ns, &self.host, &self.runtime_module, &self.info);
        encode_events_block(&mut out, &self.events);
        encode_symbols_block(&mut out, &self.syms);
        if self.complete {
            encode_end_block(&mut out, self.events.len() as u64);
        }
        w.write_all(&out)
    }

    /// Reads a version 01, 02 or 03 trace. A chunked file that ends without
    /// end block or inside a block yields the events and symbols read up to
    /// there, with `complete` false. Taskloop tasks are attributed to their
    /// construct (`attribute_taskloop_tasks`).
    pub fn read<R: Read>(r: &mut R) -> io::Result<Trace> {
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        let (v1, v2) = (&magic == MAGIC_V1, &magic == MAGIC_V2);
        if !v1 && !v2 && &magic != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a temanejo trace file"));
        }
        let mut t = Trace { rank: read_u32(r)?, pid: read_u32(r)?, epoch_ns: read_u64(r)?, host: read_str(r)?, ..Default::default() };
        if v1 {
            let n = read_u64(r)? as usize;
            read_events(r, n, &mut t.events)?;
            let n = read_u32(r)? as usize;
            read_syms(r, n, &mut t.syms)?;
            t.complete = true;
            return Ok(t);
        }
        t.runtime_module = read_str(r)?;
        if !v2 {
            for _ in 0..read_u32(r)? {
                let key = read_str(r)?;
                t.info.push((key, read_str(r)?));
            }
        }
        loop {
            let mut kind = [0u8; 1];
            if r.read_exact(&mut kind).is_err() {
                break; // end of file without end block
            }
            // A block that cannot be read completely is dropped as a whole.
            let (n_events, n_syms) = (t.events.len(), t.syms.len());
            let block = match kind[0] {
                BLOCK_EVENTS => read_u32(r).and_then(|n| read_events(r, n as usize, &mut t.events)),
                BLOCK_SYMBOLS => read_u32(r).and_then(|n| read_syms(r, n as usize, &mut t.syms)),
                BLOCK_END => read_u64(r).map(|_| t.complete = true),
                other => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unknown block type {other}"))),
            };
            if block.is_err() {
                t.events.truncate(n_events);
                t.syms.truncate(n_syms);
                break;
            }
            if t.complete {
                break;
            }
        }
        t.attribute_taskloop_tasks();
        Ok(t)
    }

    /// OpenMP runtimes may create the tasks of a `taskloop` inside the
    /// runtime and report a code address in the runtime for them (LLVM
    /// libomp does, also for internal helper tasks that create tasks on
    /// other threads later). The WORK event of the construct carries the
    /// program's address. This gives every TASK_CREATE whose address lies in
    /// the runtime module the address of the latest taskloop its creating
    /// task began before, and keeps the reported address in `c`.
    ///
    /// Needs `runtime_module` and the symbol records; events that already
    /// have `c` set (version 01 files) are left alone.
    pub fn attribute_taskloop_tasks(&mut self) {
        use std::collections::{HashMap, HashSet};
        if self.runtime_module.is_empty() {
            return;
        }
        let mut taskloops: HashMap<u64, Vec<(u64, u64)>> = HashMap::new(); // task -> (ts, codeptr)
        for e in &self.events {
            if e.kind == kind::WORK && e.endpoint == ENDPOINT_BEGIN && e.a == WORK_TASKLOOP && e.d != 0 {
                taskloops.entry(e.b).or_default().push((e.ts, e.d));
            }
        }
        if taskloops.is_empty() {
            return;
        }
        for v in taskloops.values_mut() {
            v.sort_unstable();
        }
        let in_runtime: HashSet<u64> = self.syms.iter().filter(|s| s.module == self.runtime_module).map(|s| s.addr).collect();
        for e in self.events.iter_mut().filter(|e| e.kind == kind::TASK_CREATE && e.c == 0 && in_runtime.contains(&e.d)) {
            let Some(v) = taskloops.get(&e.b) else { continue };
            let i = v.partition_point(|&(ts, _)| ts <= e.ts);
            if i > 0 {
                e.c = e.d;
                e.d = v[i - 1].1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Trace {
        let e = |i: u64| Event {
            kind: (i % 12) as u8 + 1,
            endpoint: (i % 3) as u8,
            cpu: 1000 + i as u16,
            numa: 7,
            thread: 42 + i as u32,
            flags: 0x8000_0004,
            ts: u64::MAX - i,
            a: i,
            b: i << 32,
            c: 0x0123_4567_89ab_cdef,
            d: 0xffff_0000_0000_0001 + i,
        };
        Trace {
            rank: 3,
            pid: 4711,
            epoch_ns: 1_700_000_000_000_000_000,
            host: "node-ä1".into(),
            events: (0..5).map(e).collect(),
            syms: vec![
                Sym { addr: 0x401000, load_bias: 0, module: "/bin/app".into(), symbol: String::new() },
                Sym { addr: 0x7f00_0000_1234, load_bias: 0x7f00_0000_0000, module: "/lib/libomp.so".into(), symbol: "__kmpc_fork_call".into() },
            ],
            runtime_module: "/lib/libomp.so".into(),
            info: vec![("omp.runtime_version".into(), "Test OMP 1.0".into()), ("env.OMP_NUM_THREADS".into(), "4,2".into()), ("empty".into(), String::new())],
            complete: true,
        }
    }

    #[test]
    fn event_record_is_56_bytes_little_endian() {
        let mut buf = [0u8; EVENT_SIZE];
        let e = Event { kind: 5, endpoint: 2, cpu: 0x0102, numa: 0x0304, thread: 0x0506_0708, flags: 9, ts: 0x1122_3344_5566_7788, a: 1, b: 2, c: 3, d: 4 };
        e.encode(&mut buf);
        assert_eq!(&buf[..8], &[5, 2, 0x02, 0x01, 0x04, 0x03, 0, 0]);
        assert_eq!(&buf[8..12], &[0x08, 0x07, 0x06, 0x05]);
        assert_eq!(&buf[16..24], &0x1122_3344_5566_7788u64.to_le_bytes());
        assert_eq!(&buf[48..56], &4u64.to_le_bytes());
    }

    #[test]
    fn write_read_roundtrip() {
        let t = sample();
        let mut bytes = Vec::new();
        t.write(&mut bytes).unwrap();
        assert_eq!(&bytes[..8], MAGIC);
        let r = Trace::read(&mut bytes.as_slice()).unwrap();
        assert_eq!((r.rank, r.pid, r.epoch_ns, r.host.as_str()), (3, 4711, t.epoch_ns, "node-ä1"));
        assert_eq!(r.events.len(), t.events.len());
        for (a, b) in r.events.iter().zip(&t.events) {
            assert_eq!(
                (a.kind, a.endpoint, a.cpu, a.numa, a.thread, a.flags, a.ts, a.a, a.b, a.c, a.d),
                (b.kind, b.endpoint, b.cpu, b.numa, b.thread, b.flags, b.ts, b.a, b.b, b.c, b.d)
            );
        }
        assert_eq!(r.info, t.info, "information entries keep their order");
        assert_eq!(r.runtime_module, "/lib/libomp.so");
        assert_eq!(r.syms.len(), 2);
        assert_eq!((r.syms[1].addr, r.syms[1].load_bias), (t.syms[1].addr, t.syms[1].load_bias));
        assert_eq!((r.syms[1].module.as_str(), r.syms[1].symbol.as_str()), ("/lib/libomp.so", "__kmpc_fork_call"));
    }

    #[test]
    fn rejects_bad_magic_and_truncation() {
        let mut bytes = Vec::new();
        sample().write(&mut bytes).unwrap();
        let mut bad = bytes.clone();
        bad[7] = b'9';
        assert!(Trace::read(&mut bad.as_slice()).is_err());
        // Cut inside the header: nothing usable.
        assert!(Trace::read(&mut &bytes[..20]).is_err());
    }

    fn event(kind: u8, thread: u32, ts: u64, a: u64, b: u64, d: u64) -> Event {
        Event { kind, endpoint: 0, thread, ts, a, b, d, ..Default::default() }
    }

    /// A file as the tool writes it: blocks of two threads interleaved,
    /// symbols in between, end block last.
    fn chunked(end: bool) -> Vec<u8> {
        let mut out = Vec::new();
        encode_header(&mut out, 1, 99, 5, "n1", "/lib/libomp.so", &[]);
        encode_events_block(&mut out, &[event(kind::THREAD_BEGIN, 0, 1, 0, 0, 0), event(kind::TASK_CREATE, 0, 2, 7, 1, 0)]);
        encode_symbols_block(&mut out, &[Sym { addr: 0x10, load_bias: 0, module: "/bin/app".into(), symbol: "f".into() }]);
        encode_events_block(&mut out, &[event(kind::THREAD_BEGIN, 1, 3, 0, 0, 0)]);
        encode_events_block(&mut out, &[event(kind::THREAD_END, 0, 9, 0, 0, 0)]);
        if end {
            encode_end_block(&mut out, 4);
        }
        out
    }

    #[test]
    fn reads_interleaved_blocks_in_file_order() {
        let t = Trace::read(&mut chunked(true).as_slice()).unwrap();
        assert!(t.complete);
        assert!(t.info.is_empty(), "a header without information entries is valid");
        assert_eq!((t.rank, t.pid, t.host.as_str(), t.runtime_module.as_str()), (1, 99, "n1", "/lib/libomp.so"));
        assert_eq!(t.events.iter().map(|e| (e.thread, e.ts)).collect::<Vec<_>>(), [(0, 1), (0, 2), (1, 3), (0, 9)]);
        assert_eq!(t.syms.len(), 1);
    }

    #[test]
    fn unfinished_trace_keeps_complete_blocks() {
        // No end block: everything is read, but the trace is not complete.
        let t = Trace::read(&mut chunked(false).as_slice()).unwrap();
        assert!(!t.complete);
        assert_eq!(t.events.len(), 4);
        // Cut in the middle of the last events block: that block is dropped.
        let bytes = chunked(false);
        let t = Trace::read(&mut &bytes[..bytes.len() - 10]).unwrap();
        assert!(!t.complete);
        assert_eq!((t.events.len(), t.syms.len()), (3, 1));
    }

    #[test]
    fn reads_version_02_without_information_entries() {
        // Version 02: the same blocks, but the header ends after runtime_module.
        let v3 = chunked(true);
        let header_end = 8 + 16 + (4 + 2) + (4 + "/lib/libomp.so".len());
        assert_eq!(&v3[header_end..header_end + 4], &0u32.to_le_bytes(), "n_info of the version 03 header");
        let mut v2 = MAGIC_V2.to_vec();
        v2.extend_from_slice(&v3[8..header_end]);
        v2.extend_from_slice(&v3[header_end + 4..]);
        let t = Trace::read(&mut v2.as_slice()).unwrap();
        assert!(t.complete && t.info.is_empty());
        assert_eq!((t.runtime_module.as_str(), t.events.len(), t.syms.len()), ("/lib/libomp.so", 4, 1));
    }

    #[test]
    fn reads_version_01() {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC_V1);
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&7u32.to_le_bytes());
        out.extend_from_slice(&11u64.to_le_bytes());
        write_str(&mut out, "old").unwrap();
        out.extend_from_slice(&1u64.to_le_bytes());
        let mut buf = [0u8; EVENT_SIZE];
        event(kind::THREAD_BEGIN, 3, 42, 0, 0, 0).encode(&mut buf);
        out.extend_from_slice(&buf);
        out.extend_from_slice(&0u32.to_le_bytes());
        let t = Trace::read(&mut out.as_slice()).unwrap();
        assert!(t.complete && t.runtime_module.is_empty());
        assert_eq!((t.rank, t.pid, t.epoch_ns, t.host.as_str()), (2, 7, 11, "old"));
        assert_eq!((t.events.len(), t.events[0].thread, t.events[0].ts), (1, 3, 42));
    }

    #[test]
    fn taskloop_tasks_get_the_address_of_their_construct() {
        const RT: u64 = 0x7000; // address inside the runtime
        let work = |ts, task, d| Event { kind: kind::WORK, endpoint: ENDPOINT_BEGIN, ts, a: WORK_TASKLOOP, b: task, d, ..Default::default() };
        let mut t = Trace {
            runtime_module: "/lib/libomp.so".into(),
            syms: vec![
                Sym { addr: RT, load_bias: 0, module: "/lib/libomp.so".into(), symbol: "__kmpc_taskloop".into() },
                Sym { addr: 0x100, load_bias: 0, module: "/bin/app".into(), symbol: String::new() },
            ],
            events: vec![
                event(kind::TASK_CREATE, 0, 5, 10, 1, RT),    // before any taskloop of task 1: unchanged
                work(10, 1, 0x100),                           // task 1 starts a taskloop at 0x100
                event(kind::TASK_CREATE, 0, 11, 11, 1, RT),   // its task
                event(kind::TASK_CREATE, 1, 12, 12, 1, RT),   // created by a helper on another thread
                event(kind::TASK_CREATE, 0, 13, 13, 1, 0x200), // an ordinary task: address in the program
                event(kind::TASK_CREATE, 0, 14, 14, 2, RT),   // task 2 has no taskloop: unchanged
                work(20, 1, 0x300),                           // task 1's second taskloop
                event(kind::TASK_CREATE, 0, 21, 15, 1, RT),
            ],
            ..Default::default()
        };
        t.attribute_taskloop_tasks();
        let created: Vec<(u64, u64, u64)> = t.events.iter().filter(|e| e.kind == kind::TASK_CREATE).map(|e| (e.a, e.d, e.c)).collect();
        assert_eq!(created, [(10, RT, 0), (11, 0x100, RT), (12, 0x100, RT), (13, 0x200, 0), (14, RT, 0), (15, 0x300, RT)]);
        // Applying it again changes nothing.
        t.attribute_taskloop_tasks();
        assert_eq!(t.events.iter().filter(|e| e.kind == kind::TASK_CREATE).map(|e| (e.a, e.d, e.c)).collect::<Vec<_>>(), created);
    }
}
