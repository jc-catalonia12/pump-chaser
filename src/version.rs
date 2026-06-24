//! Compile-time build metadata embedded in API responses.

use serde_json::{json, Value};

/// Version, build timestamp, git ref, and target platform of this binary.
pub fn build_metadata() -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "build_unix": env!("BUILD_UNIX").parse::<u64>().unwrap_or(0),
        "git_sha": option_env!("GIT_SHA"),
        "build_os": std::env::consts::OS,
        "build_arch": std::env::consts::ARCH,
    })
}
