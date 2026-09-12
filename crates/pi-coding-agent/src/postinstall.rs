//! Port of packages/coding-agent/src/postinstall.ts
//!
//! The TypeScript is a top-level-await install hook: it bootstraps the kernel
//! Python and the managed tools only when the installer asked for them, and it
//! never fails the install. The port keeps the same env flags, the same
//! `Promise.all` tool order, and the same best-effort catch.

use crate::core::kernel::bootstrap::{ensure_kernel_python, EnsureKernelPythonOptions};
use crate::utils::tools_manager::{ensure_tool, FD, RG};

/// `PRIME_AGENT_BOOTSTRAP_KERNEL_ON_INSTALL`.
pub const BOOTSTRAP_KERNEL_ON_INSTALL_ENV: &str = "PRIME_AGENT_BOOTSTRAP_KERNEL_ON_INSTALL";
/// `PRIME_AGENT_BOOTSTRAP_TOOLS_ON_INSTALL`.
pub const BOOTSTRAP_TOOLS_ON_INSTALL_ENV: &str = "PRIME_AGENT_BOOTSTRAP_TOOLS_ON_INSTALL";
/// `PRIME_AGENT_INSTALL_UV`.
pub const INSTALL_UV_ENV: &str = "PRIME_AGENT_INSTALL_UV";
/// The first line of the install-hook failure message.
pub const POSTINSTALL_SKIPPED_PREFIX: &str = "prime-agent: postinstall setup skipped: ";

/// `oneLine(message)`.
pub fn one_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<&str>>().join(" ")
}

/// `errorMessage(error)`.
pub fn error_message(message: &str) -> String {
    message.to_string()
}

/// The top-level-await install hook, returning the exit code the script uses.
///
/// `process.exit(0)` becomes the returned code, so the caller decides how to
/// end the process exactly like the script's `process.exit(0)` does.
pub async fn run_postinstall() -> i32 {
    let bootstrap_kernel = std::env::var(BOOTSTRAP_KERNEL_ON_INSTALL_ENV).as_deref() == Ok("1");
    let bootstrap_tools = std::env::var(BOOTSTRAP_TOOLS_ON_INSTALL_ENV).as_deref() == Ok("1");

    if !bootstrap_kernel && !bootstrap_tools {
        return 0;
    }

    if bootstrap_kernel && std::env::var(INSTALL_UV_ENV).is_err() {
        std::env::set_var(INSTALL_UV_ENV, "1");
    }

    let result: Result<(), String> = async {
        if bootstrap_tools {
            // `await Promise.all([ensureTool("fd", true), ensureTool("rg", true)])`:
            // both calls run against the same tool manager, so the port keeps
            // their completion order without spawning tasks.
            let (fd, rg) = futures::join!(
                ensure_tool(FD, true),
                ensure_tool(RG, true)
            );
            let _ = (fd, rg);
        }
        if bootstrap_kernel {
            ensure_kernel_python(EnsureKernelPythonOptions::default())
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    .await;

    if let Err(message) = result {
        eprintln!("{POSTINSTALL_SKIPPED_PREFIX}{}", one_line(&error_message(&message)));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_line_collapses_whitespace() {
        assert_eq!(one_line("  a\n b\t c  "), "a b c");
        assert_eq!(one_line(""), "");
    }

    #[test]
    fn the_install_hook_does_nothing_without_the_flags() {
        std::env::remove_var(BOOTSTRAP_KERNEL_ON_INSTALL_ENV);
        std::env::remove_var(BOOTSTRAP_TOOLS_ON_INSTALL_ENV);
        std::env::remove_var(INSTALL_UV_ENV);
        assert_eq!(futures::executor::block_on(run_postinstall()), 0);
        assert!(std::env::var(INSTALL_UV_ENV).is_err());
    }

    #[test]
    fn requesting_the_kernel_sets_the_uv_flag() {
        // Only the flag write is asserted; the kernel bootstrap itself needs a
        // Python toolchain, so the hook's own catch keeps the exit code 0.
        std::env::set_var(BOOTSTRAP_KERNEL_ON_INSTALL_ENV, "1");
        std::env::remove_var(BOOTSTRAP_TOOLS_ON_INSTALL_ENV);
        std::env::remove_var(INSTALL_UV_ENV);
        assert_eq!(futures::executor::block_on(run_postinstall()), 0);
        assert_eq!(std::env::var(INSTALL_UV_ENV).as_deref(), Ok("1"));
        std::env::remove_var(BOOTSTRAP_KERNEL_ON_INSTALL_ENV);
        std::env::remove_var(INSTALL_UV_ENV);
    }
}
