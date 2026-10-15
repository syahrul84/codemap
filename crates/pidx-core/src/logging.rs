//! Logging setup. In MCP serve mode stdout carries only protocol messages,
//! so every log line goes to stderr.

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

use crate::{Error, Result};

/// Installs the global tracing subscriber, writing to stderr.
///
/// `level` accepts any `EnvFilter` directive, e.g. `info` or `pidx_core=debug`.
pub fn init(level: &str) -> Result<()> {
    let filter = EnvFilter::try_new(level)
        .map_err(|e| Error::Config(format!("invalid log level {level:?}: {e}")))?;

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_target(false)
        .try_init()
        .map_err(|e| Error::Config(format!("logging already initialised: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_level() {
        let err = init("not a [valid filter").unwrap_err();
        assert!(matches!(err, Error::Config(_)), "got {err:?}");
    }
}
