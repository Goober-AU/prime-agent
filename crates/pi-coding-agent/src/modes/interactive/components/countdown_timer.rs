//! Port of packages/coding-agent/src/modes/interactive/components/countdown-timer.ts

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use pi_tui::tui::TUI;

/// Port of `CountdownTimer`.
///
/// The TypeScript timer calls back into its owner (which is not `Send`), so the
/// port keeps the state in shared atomics and moves only the atomics into the
/// interval task. The task posts the tick to the owner through an `mpsc`
/// channel; the owner drains it from [`CountdownTimer::poll`], which preserves
/// the TypeScript order: tick -> requestRender -> expire/dispose. The callbacks
/// therefore stay on the owner thread and need no `Send` bound.
pub struct CountdownTimer {
    remaining_seconds: Arc<AtomicI64>,
    disposed: Arc<AtomicBool>,
    pending_ticks: tokio::sync::mpsc::UnboundedReceiver<()>,
    tick_sender: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    interval_task: Option<tokio::task::JoinHandle<()>>,
    on_tick: Option<Box<dyn Fn(f64)>>,
    on_expire: Option<Box<dyn Fn()>>,
    tui: Option<Rc<RefCell<TUI>>>,
}

impl CountdownTimer {
    /// Port of the `CountdownTimer` constructor.
    pub fn new(
        timeout_ms: f64,
        tui: Option<Rc<RefCell<TUI>>>,
        on_tick: Box<dyn Fn(f64)>,
        on_expire: Box<dyn Fn()>,
    ) -> Self {
        let remaining_seconds = Arc::new(AtomicI64::new(js_ceil(timeout_ms / 1000.0)));
        let (tick_sender, pending_ticks) = tokio::sync::mpsc::unbounded_channel();
        let mut timer = Self {
            remaining_seconds,
            disposed: Arc::new(AtomicBool::new(false)),
            pending_ticks,
            tick_sender: Some(tick_sender),
            interval_task: None,
            on_tick: Some(on_tick),
            on_expire: Some(on_expire),
            tui,
        };
        timer.on_tick(timer.remaining_seconds.load(Ordering::SeqCst) as f64);
        timer.start();
        timer
    }

    fn on_tick(&self, seconds: f64) {
        if let Some(on_tick) = &self.on_tick {
            on_tick(seconds);
        }
    }

    /// Port of the `setInterval` callback.
    fn start(&mut self) {
        let Some(sender) = self.tick_sender.clone() else {
            return;
        };
        let remaining_seconds = Arc::clone(&self.remaining_seconds);
        let disposed = Arc::clone(&self.disposed);
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_millis(1000));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await;
            while !disposed.load(Ordering::SeqCst) {
                ticker.tick().await;
                if disposed.load(Ordering::SeqCst) {
                    break;
                }
                remaining_seconds.fetch_sub(1, Ordering::SeqCst);
                if sender.send(()).is_err() {
                    break;
                }
            }
        });
        self.interval_task = Some(task);
    }

    /// Runs the pending interval callbacks: `onTick`, `requestRender`, and
    /// `dispose()` + `onExpire()` once the remainder reaches zero.
    pub fn poll(&mut self) {
        while let Ok(()) = self.pending_ticks.try_recv() {
            let seconds = self.remaining_seconds.load(Ordering::SeqCst) as f64;
            self.on_tick(seconds);
            if let Some(tui) = &self.tui {
                tui.borrow_mut().request_render();
            }
            if self.remaining_seconds.load(Ordering::SeqCst) <= 0 {
                self.dispose();
                if let Some(on_expire) = self.on_expire.take() {
                    on_expire();
                }
                return;
            }
        }
    }

    pub fn remaining_seconds(&self) -> f64 {
        self.remaining_seconds.load(Ordering::SeqCst) as f64
    }

    /// Port of `dispose`.
    pub fn dispose(&mut self) {
        if !self.disposed.swap(true, Ordering::SeqCst) {
            self.tick_sender = None;
        }
        if let Some(task) = self.interval_task.take() {
            task.abort();
        }
    }
}

impl Drop for CountdownTimer {
    fn drop(&mut self) {
        self.dispose();
    }
}

/// `Math.ceil` for a finite JavaScript number.
fn js_ceil(value: f64) -> i64 {
    if !value.is_finite() {
        return 0;
    }
    value.ceil() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceils_timeout_to_seconds() {
        assert_eq!(js_ceil(50.0 / 1000.0), 1);
        assert_eq!(js_ceil(1500.0 / 1000.0), 2);
        assert_eq!(js_ceil(2000.0 / 1000.0), 2);
    }

    #[test]
    fn ticks_immediately_then_expires_after_the_remaining_ticks() {
        let ticks = Arc::new(std::sync::Mutex::new(Vec::<f64>::new()));
        let expired = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&ticks);
        let did_expire = Arc::clone(&expired);
        let mut timer = CountdownTimer::new(
            2000.0,
            None,
            Box::new(move |seconds| observed.lock().expect("ticks").push(seconds)),
            Box::new(move || did_expire.store(true, Ordering::SeqCst)),
        );

        // The constructor ticks once with the full remainder.
        assert_eq!(*ticks.lock().expect("ticks"), vec![2.0]);

        std::thread::sleep(std::time::Duration::from_millis(2100));
        timer.poll();
        assert_eq!(timer.remaining_seconds(), 0.0);
        assert!(expired.load(Ordering::SeqCst));
        assert_eq!(*ticks.lock().expect("ticks"), vec![2.0, 1.0, 0.0]);
        assert!(timer.tick_sender.is_none());
    }

    #[test]
    fn dispose_stops_the_interval() {
        let mut timer = CountdownTimer::new(5000.0, None, Box::new(|_| {}), Box::new(|| {}));
        timer.dispose();
        assert!(timer.tick_sender.is_none());
        assert!(timer.interval_task.is_none());
    }
}
