//! Where an answer's time went, step by step, for the report of a slow one.
//!
//! A tool that does several things in a row calls [`mark`] after each; the
//! server turns timing on around a call with [`start`] and reads the steps
//! back with [`finish`]. Without `start`, `mark` does nothing.

use std::cell::RefCell;
use std::time::{Duration, Instant};

type Steps = Vec<(&'static str, Duration)>;

thread_local! {
    static STEPS: RefCell<Option<(Instant, Steps)>> = const { RefCell::new(None) };
}

/// Starts timing on this thread.
pub fn start() {
    STEPS.with(|s| *s.borrow_mut() = Some((Instant::now(), Vec::new())));
}

/// The time since `start`, or since the mark before this one, went to `step`.
pub fn mark(step: &'static str) {
    STEPS.with(|s| {
        if let Some((last, steps)) = s.borrow_mut().as_mut() {
            let now = Instant::now();
            let took = now.duration_since(*last);
            *last = now;
            match steps.iter_mut().find(|(name, _)| *name == step) {
                Some((_, total)) => *total += took,
                None => steps.push((step, took)),
            }
        }
    });
}

/// Stops timing on this thread: the steps marked since `start`, longest first.
pub fn finish() -> Steps {
    let mut steps = STEPS
        .with(|s| s.borrow_mut().take())
        .map(|(_, steps)| steps)
        .unwrap_or_default();
    steps.sort_by_key(|(_, took)| std::cmp::Reverse(*took));
    steps
}

/// `unused code 6.1 s, deleted names 3.0 s`: the steps that took a tenth of a
/// second or more, the four longest. Empty when none did.
pub fn text(steps: &[(&'static str, Duration)]) -> String {
    steps
        .iter()
        .filter(|(_, took)| took.as_millis() >= 100)
        .take(4)
        .map(|(step, took)| format!("{step} {:.1} s", took.as_secs_f64()))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_are_kept_only_between_start_and_finish() {
        mark("before");
        assert!(finish().is_empty());

        start();
        std::thread::sleep(Duration::from_millis(120));
        mark("slow step");
        mark("quick step");
        std::thread::sleep(Duration::from_millis(5));
        mark("slow step");
        let steps = finish();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].0, "slow step");
        assert!(steps[0].1 >= Duration::from_millis(120));
        assert!(text(&steps).starts_with("slow step 0.1 s"));
        assert!(!text(&steps).contains("quick step"));

        mark("after");
        assert!(finish().is_empty());
    }
}
