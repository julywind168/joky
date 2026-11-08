//! Startup progress stays on stderr and finishes before the program takes over.
use std::{fmt::Display, io::IsTerminal, path::Path, time::Instant};

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
        progress.status("Loading", path.display());
        progress
    }

    pub(super) fn compiling(&mut self, interpreter: bool) {
        self.compilation = Instant::now();
        let backend = if interpreter { "bytecode" } else { "JIT" };
        self.status(
            "Compiling",
            format_args!(
                "{} ({backend}; loaded in {:.2}s)",
                self.path.display(),
                self.compilation.duration_since(self.started).as_secs_f64(),
            ),
        );
    }

    pub(super) fn running(&self) {
        self.status(
            "Running",
            format_args!(
                "{} (compiled in {:.2}s)",
                self.path.display(),
                self.compilation.elapsed().as_secs_f64(),
            ),
        );
    }

    fn status(&self, action: &str, details: impl Display) {
        if !self.enabled {
            return;
        }
        // Share Ariadne's stderr color policy, including NO_COLOR and forced color.
        if concolor::get(concolor::Stream::Stderr).color() {
            eprintln!("\x1b[1;32m{action:>10}\x1b[0m {details}");
        } else {
            eprintln!("{action:>10} {details}");
        }
    }
}
