//! End-to-end checks of the installed-style command-line interface.
//! Static diagnostic matrices use `build` once per source, retaining publication checks.
//! The syntax, assignment, dependency and entry-point regressions separately cover
//! diagnostic parity across CLI routes. Fixtures already executed in both VM modes
//! with resource assertions need only standalone AOT coverage here.
#[cfg(unix)]
#[path = "fixtures/map_stress_oracle.rs"]
mod map_stress_oracle;

#[cfg(unix)]
#[path = "support/runtime.rs"]
mod runtime;

#[path = "support/scratch.rs"]
mod scratch;

#[path = "support/cli.rs"]
mod support;

#[cfg(unix)]
use runtime::runtime_archive;

use std::process::Command;

use support::{failure, success, Workspace};

// VM/AOT probe registries record integer events without doing I/O. CLI runs the
// same fixture algorithms with a synchronous Joky stdout adapter instead.
#[cfg(unix)]
fn recording_cli_source(source: &str) -> String {
    source.replace(
        "extern fn record_int(value: Int32)",
        "import joky/io.{println} fn record_int(value: Int32) { println(&value) }",
    )
}

#[cfg(unix)]
#[path = "cli/http.rs"]
mod http;
#[cfg(unix)]
#[path = "support/tcp_server.rs"]
mod tcp_server;
#[cfg(unix)]
#[path = "cli/tls.rs"]
mod tls;

#[path = "cli/arguments.rs"]
mod arguments;
#[path = "cli/assertions.rs"]
mod assertions;
#[path = "cli/build.rs"]
mod build;
#[path = "cli/check.rs"]
mod check;
#[path = "cli/containers.rs"]
mod containers;
#[path = "cli/diagnostics.rs"]
mod diagnostics;
#[path = "cli/doc.rs"]
mod doc;
#[path = "cli/domains.rs"]
mod domains;
#[path = "cli/embedding.rs"]
mod embedding;
#[path = "cli/format.rs"]
mod format;
#[path = "cli/io.rs"]
mod io;
#[path = "cli/language.rs"]
mod language;
#[path = "cli/modules.rs"]
mod modules;
#[path = "cli/tasks.rs"]
mod tasks;

#[path = "cli/packages.rs"]
mod packages;
#[path = "cli/process.rs"]
mod process;
#[path = "cli/project.rs"]
mod project;

#[cfg(unix)]
#[path = "cli/workers.rs"]
mod workers;

#[path = "cli/testing.rs"]
mod testing;

#[cfg(unix)]
#[path = "cli/shutdown.rs"]
mod shutdown;
