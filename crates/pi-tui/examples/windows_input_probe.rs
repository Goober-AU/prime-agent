use pi_tui::terminal::{ProcessTerminal, Terminal, TerminalStopOptions};
use std::{cell::RefCell, rc::Rc, time::{Duration, Instant}};

fn main() {
    let path = std::env::args().nth(1).expect("result path");
    let events = Rc::new(RefCell::new(Vec::new()));
    let received = events.clone();
    let mut terminal = ProcessTerminal::new();
    terminal.start(Box::new(move |data| {
        received.borrow_mut().push(serde_json::json!({"raw": data, "key": pi_tui::keys::parse_key(&data)}));
    }), Box::new(|| {}));
    terminal.write("INPUT_PROBE_READY\r\n");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(8) {
        if !terminal.poll_input().expect("input poll") { break; }
        std::thread::sleep(Duration::from_millis(5));
    }
    terminal.stop(TerminalStopOptions::default());
    std::fs::write(path, serde_json::to_vec_pretty(&*events.borrow()).unwrap()).unwrap();
}
