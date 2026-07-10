//! `whisper-cli`: thin HTTP client of the `whisperd` `/v1/*` API (`ARCHITECTURE.md` §4,
//! D-0009) — "CLI parity is free" since it never links `whisper-core` directly.
//!
//! Not yet implemented: no HTTP calls exist yet (that is a later Phase-3 task once
//! `whisper-iface` serves `/v1/*`); this binary currently only reports its version.

fn version_string() -> String {
    format!("whisper-cli {}", env!("CARGO_PKG_VERSION"))
}

fn main() {
    println!("{}", version_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_string_includes_crate_version() {
        assert!(version_string().starts_with("whisper-cli "));
    }
}
