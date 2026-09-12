//! Port of packages/coding-agent/src/bun/restore-sandbox-env.ts
//!
//! Workaround for https://github.com/oven-sh/bun/issues/27802
//!
//! Bun compiled binaries have an empty `process.env` when running inside
//! sandbox environments (e.g. nono on Linux/macOS). On Linux we can recover
//! the environment from `/proc/self/environ`.
//!
//! Rust reads the real process environment directly, so `process.env` is never
//! empty; the `process.versions.bun` guard below mirrors the TypeScript gate
//! (Bun sets `PI_BUN_VERSION` for the ported binary).

use std::io::Read;

/// Restore environment variables from `/proc/self/environ` when running
/// inside a sandbox where Bun's `process.env` is empty.
pub fn restore_sandbox_env() {
    if std::env::var_os("PI_BUN_VERSION").is_none() {
        return;
    }

    // If process.env already has entries, nothing to fix.
    if !std::env::vars_os().next().is_none() {
        return;
    }

    if let Ok(mut file) = std::fs::File::open("/proc/self/environ") {
        let mut data = String::new();
        if file.read_to_string(&mut data).is_ok() {
            for entry in data.split('\0') {
                let Some(index) = entry.find('=') else {
                    continue;
                };
                if index > 0 {
                    // Rust cannot mutate the process environment after start on
                    // every platform; the recovered values are exposed to the
                    // port's shell env builder instead.
                    sandbox_env()
                        .lock()
                        .unwrap()
                        .insert(entry[..index].to_string(), entry[index + 1..].to_string());
                }
            }
        }
    }
}

/// Recovered sandbox environment values (the port of the `process.env` writes).
pub fn sandbox_env() -> &'static std::sync::Mutex<std::collections::BTreeMap<String, String>> {
    static ENV: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String, String>>> =
        std::sync::OnceLock::new();
    ENV.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_is_a_no_op_without_bun() {
        let before = sandbox_env().lock().unwrap().len();
        restore_sandbox_env();
        assert_eq!(sandbox_env().lock().unwrap().len(), before);
    }
}
