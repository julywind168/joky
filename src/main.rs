//! Command-line entry point for running and building Joky programs.
mod cli;

fn main() -> std::process::ExitCode {
    std::process::ExitCode::from(cli::execute(std::env::args_os().skip(1)))
}
