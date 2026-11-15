//! Trace file format: one file per (MPI) process, `rank-<N>.tmj`.
//!
//! Layout (all integers little endian):
//!
//! ```text
//! magic "TMJTRC01"
//! u32 rank, u32 pid, u64 epoch_ns (CLOCK_REALTIME at tool init), str host
//! u64 n_events, n_events * Event (56 bytes each)
//! u32 n_syms,   n_syms * { u64 addr, u64 load_bias, str module, str symbol }
//! ```
//! `str` is a u32 length followed by UTF-8 bytes.
//!
//! The full specification is in `docs/trace-format.rst`.

use std::io::{self, Read, Write};

pub const MAGIC: &[u8; 8] = b"TMJTRC01";

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
    /// c = codeptr as reported by the runtime if it pointed into the runtime
    /// and `d` was replaced by the codeptr of the enclosing taskloop, else 0.
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

impl Trace {
    pub fn write<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(MAGIC)?;
        w.write_all(&self.rank.to_le_bytes())?;
        w.write_all(&self.pid.to_le_bytes())?;
        w.write_all(&self.epoch_ns.to_le_bytes())?;
        write_str(w, &self.host)?;
        w.write_all(&(self.events.len() as u64).to_le_bytes())?;
        let mut buf = [0u8; EVENT_SIZE];
        for e in &self.events {
            e.encode(&mut buf);
            w.write_all(&buf)?;
        }
        w.write_all(&(self.syms.len() as u32).to_le_bytes())?;
        for s in &self.syms {
            w.write_all(&s.addr.to_le_bytes())?;
            w.write_all(&s.load_bias.to_le_bytes())?;
            write_str(w, &s.module)?;
            write_str(w, &s.symbol)?;
        }
        Ok(())
    }

    pub fn read<R: Read>(r: &mut R) -> io::Result<Trace> {
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a temanejo trace file"));
        }
        let rank = read_u32(r)?;
        let pid = read_u32(r)?;
        let epoch_ns = read_u64(r)?;
        let host = read_str(r)?;
        let n = read_u64(r)? as usize;
        let mut events = Vec::with_capacity(n.min(1 << 24));
        let mut buf = [0u8; EVENT_SIZE];
        for _ in 0..n {
            r.read_exact(&mut buf)?;
            events.push(Event::decode(&buf));
        }
        let ns = read_u32(r)? as usize;
        let mut syms = Vec::with_capacity(ns.min(1 << 20));
        for _ in 0..ns {
            let addr = read_u64(r)?;
            let load_bias = read_u64(r)?;
            let module = read_str(r)?;
            let symbol = read_str(r)?;
            syms.push(Sym { addr, load_bias, module, symbol });
        }
        Ok(Trace { rank, pid, epoch_ns, host, events, syms })
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
        assert!(Trace::read(&mut &bytes[..bytes.len() - 3]).is_err());
    }
}
