//! Maps code addresses to source locations.
//!
//! The tool stores module path and load bias for each address; file and line
//! come from the module's DWARF info via `llvm-addr2line` (or binutils
//! `addr2line`), run once per module with all addresses.
//!
//! Traces recorded on another system name modules by their paths there; the
//! files given with `--exe` replace recorded modules with the same file name.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
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

/// Local copies of modules recorded in a trace, keyed by file name.
#[derive(Default)]
pub struct ModuleMap {
    by_name: HashMap<String, PathBuf>,
}

impl ModuleMap {
    /// Adds a local file; recorded modules with the same file name are read
    /// from it. Returns false if another file with that name was added before.
    pub fn add(&mut self, path: PathBuf) -> bool {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.by_name.insert(name, path).is_none()
    }

    /// The file to read the debug information of the recorded `module` from.
    fn resolve<'a>(&'a self, module: &'a str) -> &'a str {
        Path::new(module)
            .file_name()
            .and_then(|n| self.by_name.get(n.to_string_lossy().as_ref()))
            .and_then(|p| p.to_str())
            .unwrap_or(module)
    }

    /// Warns about given files that match none of the `modules` recorded in
    /// the traces, listing the recorded modules.
    pub fn warn_unused<'a>(&self, modules: impl IntoIterator<Item = &'a str>) {
        let recorded: HashSet<&str> = modules.into_iter().filter(|m| !m.is_empty()).collect();
        let names: HashSet<String> = recorded.iter().filter_map(|m| Path::new(m).file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
        let mut unused: Vec<&PathBuf> = self.by_name.iter().filter(|(n, _)| !names.contains(*n)).map(|(_, p)| p).collect();
        if unused.is_empty() {
            return;
        }
        unused.sort();
        let mut recorded: Vec<&str> = recorded.into_iter().collect();
        recorded.sort();
        for p in unused {
            eprintln!("temanejo: --exe {}: no module with this file name is recorded in the traces", p.display());
        }
        eprintln!("temanejo: recorded modules:\n  {}", recorded.join("\n  "));
    }
}

pub fn symbolize(syms: &[Sym], modules: &ModuleMap) -> Vec<Loc> {
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
        let Some(res) = addr2line(modules.resolve(module), &addrs) else { continue };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modules_resolve_by_file_name() {
        let mut m = ModuleMap::default();
        assert!(m.add("/local/build/app".into()));
        assert!(m.add("/local/lib/libfoo.so".into()));
        assert!(!m.add("/elsewhere/app".into()));
        assert_eq!(m.resolve("/remote/home/u/app"), "/elsewhere/app");
        assert_eq!(m.resolve("/remote/lib/libfoo.so"), "/local/lib/libfoo.so");
        assert_eq!(m.resolve("/usr/lib/libomp.so"), "/usr/lib/libomp.so");
    }
}
