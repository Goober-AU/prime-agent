//! Port of packages/coding-agent/src/bun/cli.ts
//!
//! The TypeScript sets `process.title`, silences `process.emitWarning`, restores
//! the sandbox environment, registers the Bedrock provider module, then imports
//! the CLI entry point. Rust cannot rename the process title portably and has no
//! dynamic `import()`, so the same steps run in order inside one function.

use crate::bun::register_bedrock::register_bedrock;
use crate::bun::restore_sandbox_env::restore_sandbox_env;
use crate::config::APP_NAME;

/// Port of the module body of `bun/cli.ts`.
///
/// Returns the process exit code of the CLI entry point.
pub fn run() -> i32 {
    set_process_title(APP_NAME);
    restore_sandbox_env();
    register_bedrock();
    crate::cli_entry::main_entry(std::env::args().collect())
}

/// `process.title = APP_NAME` (best effort: Windows has no portable setter).
fn set_process_title(title: &str) {
    let _ = title;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_name_is_the_package_config_name() {
        assert_eq!(APP_NAME, "prime-agent");
    }
}
