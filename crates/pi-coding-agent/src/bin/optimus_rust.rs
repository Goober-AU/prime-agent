//! optimus-rust: the port's own command (packages/coding-agent/src/cli.ts entry).
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--internal-telegram-worker") {
        // A standalone worker must not enter the UI or start another daemon.
        let result = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all()
            .build().map_err(|error| error.to_string()).and_then(|runtime| {
                runtime.block_on(pi_coding_agent::modes::telegram::worker::telegram_worker_main(&args))
            });
        if let Err(error) = result { eprintln!("{error}"); std::process::exit(1); }
        return;
    }
    std::process::exit(pi_coding_agent::cli_entry::main_entry(args));
}
