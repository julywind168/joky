//! Command-line entry point for running and building Joky programs.
mod cli;

fn main() -> std::process::ExitCode {
    match cli::execute(std::env::args_os().skip(1)) {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
