//! Command-specific usage stays separate from argument parsing.
pub(super) fn command(name: &str) -> Option<&'static str> {
    Some(match name {
        "init" => INIT,
        "run" => RUN,
        "build" => BUILD,
        "check" => CHECK,
        "test" => TEST,
        "fmt" => FORMAT,
        "doc" => DOC,
        "explain-parallel" => EXPLAIN,
        _ => return None,
    })
}

pub(super) const INIT: &str =
    "Create a minimal Joky application without overwriting existing files.

Usage: joky init [--name <name>] [<directory>]

Options:
  --name <name>  Project name (default: directory name, or joky-app)
  -h, --help     Show this help

Creates joky.toml and src/main.jk. The default directory is the current directory.
Examples:
  joky init
  joky init hello
  joky run hello";

pub(super) const RUN: &str = "Run a Joky file or project.

Usage: joky run [options] [<file.jk> | <project-dir> | joky.toml] [-- <program-args>...]

Options:
  --interpreter     Use the register bytecode interpreter instead of JIT
  --no-prelude      Disable implicit standard-library imports
  --strict-native-stack  Require budgeted synchronous native paths (macOS arm64)
  --parallel        Enable checked Parallel execution (default: 1 worker)
  --task-workers N  Enable Parallel with 1..256 workers
  -q, --quiet       Hide startup progress
  -v, --verbose     Show progress and timings even when stderr is redirected
  -h, --help        Show this help

Without an input, find the nearest joky.toml in the current directory or its parents.
Program arguments follow -- after an explicit input. Programs start at fn main().
Examples:
  joky run hello.jk
  joky run --interpreter hello.jk
  joky run . -- one two
  joky init hello";

pub(super) const BUILD: &str =
    "Compile a Joky file or project to native output using the system C compiler (cc).

Usage: joky build [options] [<file.jk> | <project-dir> | joky.toml]

Options:
  -o, --output <path>  Output executable or project bundle directory
  --no-prelude        Disable implicit standard-library imports
  --strict-native-stack  Require budgeted synchronous native paths (macOS arm64)
  --parallel          Enable checked Parallel execution (default: 1 worker)
  --task-workers N    Enable Parallel with 1..256 workers; set a startup default
  -h, --help           Show this help

Without an input, find the nearest joky.toml in the current directory or its parents.
Single-file output defaults to the source path without .jk; project bundles default to dist/.
Print compilation and linking stages, then the published path and total build time.
Examples:
  joky build hello.jk -o hello
  joky build . -o dist";

pub(super) const CHECK: &str = "Check syntax and semantics without executing or generating code.

Usage: joky check [--no-prelude] [<file.jk> | <project-dir> | joky.toml]

Options:
  --no-prelude  Disable implicit standard-library imports
  -h, --help    Show this help

Without an input, find the nearest joky.toml in the current directory or its parents.
Single files and library packages do not require main; application projects require an entry.
Success is silent (exit 0); errors go to stderr (exit 1).
Examples:
  joky check
  joky check hello.jk";

pub(super) const TEST: &str = "Run #test functions in isolated processes.

Usage: joky test [options] [<file.jk> | <project-dir> | joky.toml]

Options:
  --interpreter    Use the register bytecode interpreter instead of JIT
  --no-prelude     Disable implicit standard-library imports
  --filter <text>  Run tests whose names contain this text
  --list           List matching tests without executing them
  --nocapture      Display output from successful tests too
  --timeout-ms N   Per-test timeout in milliseconds (default: 30000)
  -h, --help       Show this help

Without an input, find the nearest joky.toml in the current directory or its parents.
Examples:
  joky test
  joky test tests.jk --filter arithmetic";

pub(super) const FORMAT: &str = "Format Joky sources without resolving imports or running code.

Usage:
  joky fmt [--check] [--diff] [<file.jk> | <directory> | joky.toml]
  joky fmt [--check] [--diff] --stdin

Options:
  --check      Report all formatting differences without writing (exit 1)
  --diff       Preview a unified diff without writing (exit 1 if changed)
  --stdin      Read stdin; write formatted source to stdout (no files)
  -h, --help   Show this help

Without an input, find the nearest joky.toml in the current directory or its parents.
Projects format only their source-root, without loading path dependencies or requiring main.
Directories recurse through .jk files; hidden directories, target, dist, nested projects and symlinks are skipped.
All selected files must parse before any are rewritten. Writes preserve file permissions.
Formatting uses four spaces and a soft 100-character width. Standalone // fmt: off/on comments preserve a region.
Examples:
  joky fmt
  joky fmt src
  joky fmt --check .
  joky fmt --diff src
  joky fmt --stdin < hello.jk";

pub(super) const DOC: &str = "Generate an offline homepage and standard-library HTML.

Usage: joky doc [--std] [--open] [-o <directory>]

Options:
  --std               Generate only the standard-library reference
  --open              Open the generated index in the default browser
  -o, --output <path>  Output directory (default: target/doc; --std: target/doc/std)
  -h, --help           Show this help

Examples:
  joky doc --open
  joky doc --std -o reference";

pub(super) const EXPLAIN: &str =
    "Explain parallel dispatch eligibility without running the program.

Usage: joky explain-parallel [--no-prelude] [<file.jk> | <project-dir> | joky.toml]

Options:
  --no-prelude  Disable implicit standard-library imports
  -h, --help    Show this help

Without an input, find the nearest joky.toml in the current directory or its parents.
Example:
  joky explain-parallel .";
