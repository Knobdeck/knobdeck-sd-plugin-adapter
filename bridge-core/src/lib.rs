//! Pure parts of the bridge: paths, manifest translation, settings store, import.
//! No sockets and no processes here, so all of it is unit-testable.

pub mod feedback;
pub mod import;
pub mod manifest;
pub mod paths;
pub mod settings;

/// Random lowercase hex, `bytes` bytes long.
pub fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    getrandom::getrandom(&mut b).expect("os random");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Lowercase ASCII, digits, `- _ .` only (the host's plugin-id rule).
pub fn slug(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9' | '-' | '_') => c,
            _ => '-',
        })
        .collect();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out.trim_matches('-').to_string()
}
