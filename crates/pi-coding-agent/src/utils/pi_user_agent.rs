//! Port of packages/coding-agent/src/utils/pi-user-agent.ts

/// Bun sets `process.versions.bun`; Node reports only `process.version`. The
/// Rust port has no JS runtime, so the runtime token reports the port's own
/// version, which is the value this port reports everywhere else.
fn runtime_string() -> String {
    format!("node/{}", env!("CARGO_PKG_VERSION"))
}

/// Node's `process.platform` name for the host this binary was built for.
pub fn process_platform() -> &'static str {
    if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "android") {
        "android"
    } else {
        "linux"
    }
}

/// Node's `process.arch` name for the host this binary was built for.
pub fn process_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x64"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86") {
        "ia32"
    } else if cfg!(target_arch = "arm") {
        "arm"
    } else {
        "unknown"
    }
}

pub fn get_pi_user_agent(version: &str) -> String {
    let runtime = runtime_string();
    format!(
        "prime-agent/{} ({}; {}; {})",
        version,
        process_platform(),
        runtime,
        process_arch()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_the_expected_user_agent() {
        let agent = get_pi_user_agent("0.9.3");
        assert!(agent.starts_with("prime-agent/0.9.3 ("));
        assert!(agent.contains(&format!("({}; node/", process_platform())));
        assert!(agent.ends_with(&format!("; {})", process_arch())));
    }
}
