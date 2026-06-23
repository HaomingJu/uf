use crate::config::Config;
use std::io::IsTerminal;

pub fn diagnostics_enabled() -> bool {
    !std::io::stderr().is_terminal()
}

pub fn debug_enabled(config: &Config) -> bool {
    config.debug && diagnostics_enabled()
}

pub fn write(message: &str) {
    if diagnostics_enabled() {
        eprintln!("{message}");
    }
}

pub fn write_debug(config: &Config, message: &str) {
    if debug_enabled(config) {
        write(message);
    }
}

pub fn suppressed_debug_notice(config: &Config) -> Option<&'static str> {
    if config.debug && !diagnostics_enabled() {
        Some(
            "Debug logging is enabled, but stderr is attached to the terminal, so diagnostics are suppressed. Redirect stderr to a file to capture logs.",
        )
    } else {
        None
    }
}
