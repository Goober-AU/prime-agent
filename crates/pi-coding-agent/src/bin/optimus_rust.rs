//! optimus-rust: the port's own command (packages/coding-agent/src/cli.ts entry).
fn main() {
    std::process::exit(pi_coding_agent::cli_entry::main_entry(std::env::args().collect()));
}
