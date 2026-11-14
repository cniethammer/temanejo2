//! Maps code addresses to source locations.
//!
//! The tool stores module path and load bias for each address; file and line
//! come from the module's DWARF info via `llvm-addr2line` (or binutils
//! `addr2line`), run once per module with all addresses.

use std::collections::HashMap;
use std::process::Command;
use tmj_trace::Sym;

#[derive(Clone, Default)]
pub struct Loc {
    pub addr: u64,
    pub module: String,
    pub func: String,
    pub file: String,
    pub line: u32,
}

fn addr2line(module: &str, addrs: &[u64]) -> Option<Vec<(String, String)>> {
    for tool in ["llvm-addr2line", "addr2line"] {
        let out = Command::new(tool)
            .env_remove("DEBUGINFOD_URLS") // never hit the network for debug info
            .args(["-f", "-C", "-e", module])
            .args(addrs.iter().map(|a| format!("{a:#x}")))
            .output();
        let Ok(out) = out else { continue };
        if !out.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() < addrs.len() * 2 {
            continue;
        }
        return Some(lines.chunks(2).take(addrs.len()).map(|c| (c[0].to_string(), c[1].to_string())).collect());
    }
    None
}

pub fn symbolize(syms: &[Sym]) -> Vec<Loc> {
    let mut locs: Vec<Loc> = syms
        .iter()
        .map(|s| Loc { addr: s.addr, module: s.module.clone(), func: s.symbol.clone(), ..Default::default() })
        .collect();

    let mut by_module: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, s) in syms.iter().enumerate() {
        if !s.module.is_empty() {
            by_module.entry(&s.module).or_default().push(i);
        }
    }
    for (module, idxs) in by_module {
        // The recorded addresses are return addresses; step back into the call.
        let addrs: Vec<u64> = idxs.iter().map(|&i| syms[i].addr.wrapping_sub(syms[i].load_bias).wrapping_sub(1)).collect();
        let Some(res) = addr2line(module, &addrs) else { continue };
        for (&i, (func, fileline)) in idxs.iter().zip(res) {
            if func != "??" {
                locs[i].func = func;
            }
            // "path/file.c:123" possibly followed by " (discriminator N)"
            let fl = fileline.split(" (").next().unwrap_or("");
            if let Some((file, line)) = fl.rsplit_once(':') {
                if file != "??" {
                    locs[i].file = file.to_string();
                    locs[i].line = line.parse().unwrap_or(0);
                }
            }
        }
    }
    locs
}
