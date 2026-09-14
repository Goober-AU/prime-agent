//! Port of packages/coding-agent/src/core/kernel/bootstrap-cli.ts
//!
//! The TypeScript is a 13-line top-level `await` script:
//!
//!     try {
//!         const python = await ensureKernelPython();
//!         console.log(`kernel python: ${python}`);
//!     } catch (error) {
//!         console.error(errorMessage(error));
//!         process.exit(1);
//!     }
//!
//! `ensureKernelPython` is `bootstrap::ensure_kernel_python`, and `errorMessage(error)`
//! is `shared::error_message`. This port keeps the same output text and the same exit code.

use crate::core::kernel::bootstrap::{ensure_kernel_python, EnsureKernelPythonOptions};
use crate::core::kernel::shared::error_message;

/// `errorMessage(error): string` for the values this script can observe.
fn error_message_for_display(error: &crate::core::kernel::shared::KernelError) -> String {
    error_message(error)
}

/// Run the script body. Returns the process exit code (0 on success, 1 on failure).
pub async fn run() -> i32 {
    match ensure_kernel_python(EnsureKernelPythonOptions {
        python_skills: None,
        on_progress: None,
    })
    .await
    {
        Ok(python) => {
            println!("kernel python: {python}");
            0
        }
        Err(error) => {
            eprintln!("{}", error_message_for_display(&error));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_message_matches_the_typescript_error_message_helper() {
        // `error instanceof Error ? error.message : String(error)` - the Rust
        // KernelError already implements Display with the same text.
        let error = crate::core::kernel::shared::KernelError::new("boom");
        assert_eq!(error_message_for_display(&error), "boom");
    }
}
