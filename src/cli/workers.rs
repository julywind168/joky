//! CLI-only startup policy. Embedding APIs never consult the environment.
use super::cli_argument_error;
use joky::{runtime::parallel::ExecutionMode, Error};
use std::ffi::OsStr;

pub(super) fn parse(value: &OsStr, source: &str) -> Result<usize, Error> {
    value
        .to_str()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| (1..=256).contains(n))
        .ok_or_else(|| cli_argument_error(format!("{source} requires a worker count in 1..=256")))
}

pub(super) fn resolve(
    parallel: bool,
    explicit: Option<usize>,
    environment: Option<&OsStr>,
) -> Result<ExecutionMode, Error> {
    if !parallel {
        if environment.is_some() {
            return Err(cli_argument_error(
                "JOKY_TASK_WORKERS requires Parallel execution; use --parallel",
            ));
        }
        return Ok(ExecutionMode::Local);
    }
    let workers = match explicit {
        Some(n) => n,
        None => environment
            .map(|v| parse(v, "JOKY_TASK_WORKERS"))
            .transpose()?
            .unwrap_or(1),
    };
    Ok(ExecutionMode::Parallel { workers })
}
