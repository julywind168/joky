//! Startup progress stays on stderr and finishes before the program takes over.
use std::{io::IsTerminal, path::Path, time::Instant};

pub(super) struct Progress<'a> {
    path: &'a Path,
    enabled: bool,
    started: Instant,
    compilation: Instant,
}

impl<'a> Progress<'a> {
    pub(super) fn new(path: &'a Path, enabled: Option<bool>) -> Self {
        let started = Instant::now();
        let progress = Self {
            path,
            enabled: enabled.unwrap_or_else(|| std::io::stderr().is_terminal()),
            started,
            compilation: started,
        };
        if progress.enabled {
            eprintln!("   Loading {}", path.display());
        }
        progress
    }

    pub(super) fn compiling(&mut self, interpreter: bool) {
        self.compilation = Instant::now();
        if self.enabled {
            let backend = if interpreter { "bytecode" } else { "JIT" };
            eprintln!(
                " Compiling {} ({backend}; loaded in {:.2}s)",
                self.path.display(),
                self.compilation.duration_since(self.started).as_secs_f64(),
            );
        }
    }

    pub(super) fn running(&self) {
        if self.enabled {
            let now = Instant::now();
            eprintln!(
                "   Running {} (compiled in {:.2}s; startup {:.2}s)",
                self.path.display(),
                now.duration_since(self.compilation).as_secs_f64(),
                now.duration_since(self.started).as_secs_f64(),
            );
        }
    }
}
