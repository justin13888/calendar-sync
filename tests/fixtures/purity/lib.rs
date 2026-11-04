//! Impure on purpose: clippy must reject both functions with `disallowed_methods`.

/// Reads the clock.
pub fn now() -> std::time::SystemTime {
    std::time::SystemTime::now()
}

/// Reads the filesystem.
///
/// # Errors
///
/// Whatever the read returns.
pub fn read(path: &str) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}
