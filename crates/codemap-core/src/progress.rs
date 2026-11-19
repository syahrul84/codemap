//! How far the refresh of an index has got, for whoever is waiting on it
//! (the MCP server answers from the previous index past its time budget and
//! says where the re-index stands).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The steps of a refresh, in order.
pub const STEPS: &[&str] = &[
    "looking for changed files",
    "reading PHP",
    "routes, models and database tables",
    "frontend code",
    "Swift, Kotlin and Rust",
];

struct Running {
    index: PathBuf,
    step: usize,
    files: Option<usize>,
    started: Instant,
}

static RUNNING: Mutex<Vec<Running>> = Mutex::new(Vec::new());

/// Where a refresh stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// 1-based.
    pub step: usize,
    pub of: usize,
    pub name: &'static str,
    /// Files found changed, once the first step has counted them.
    pub files: Option<usize>,
    pub elapsed: Duration,
}

impl Progress {
    /// `step 3 of 5 (frontend code), 412 changed files, 3.1 s so far`.
    pub fn text(&self) -> String {
        let mut s = format!("step {} of {} ({})", self.step, self.of, self.name);
        if let Some(n) = self.files {
            s.push_str(&format!(
                ", {n} changed file{}",
                if n == 1 { "" } else { "s" }
            ));
        }
        s.push_str(&format!(", {:.1} s so far", self.elapsed.as_secs_f64()));
        s
    }
}

/// Marks the refresh of `index` as running until the returned value is
/// dropped. An index in memory (`None`) is not tracked.
pub fn begin(index: Option<&Path>) -> Guard {
    if let (Some(index), Ok(mut running)) = (index, RUNNING.lock()) {
        running.retain(|r| r.index != index);
        running.push(Running {
            index: index.to_path_buf(),
            step: 0,
            files: None,
            started: Instant::now(),
        });
    }
    Guard(index.map(Path::to_path_buf))
}

/// The refresh of `index` has reached the step named `name` (one of
/// [`STEPS`]); `files` is the number of changed files when it is known.
pub fn step(index: Option<&Path>, name: &str, files: Option<usize>) {
    let (Some(index), Ok(mut running)) = (index, RUNNING.lock()) else {
        return;
    };
    if let Some(r) = running.iter_mut().find(|r| r.index == index) {
        if let Some(i) = STEPS.iter().position(|s| *s == name) {
            r.step = i;
        }
        if files.is_some() {
            r.files = files;
        }
    }
}

/// Where the refresh of `index` stands, when one is running in this process.
pub fn of(index: &Path) -> Option<Progress> {
    let running = RUNNING.lock().ok()?;
    let r = running.iter().find(|r| r.index == index)?;
    Some(Progress {
        step: r.step + 1,
        of: STEPS.len(),
        name: STEPS[r.step],
        files: r.files,
        elapsed: r.started.elapsed(),
    })
}

/// Ends the tracking of a refresh when dropped (also when it failed).
pub struct Guard(Option<PathBuf>);

impl Drop for Guard {
    fn drop(&mut self) {
        if let (Some(index), Ok(mut running)) = (&self.0, RUNNING.lock()) {
            running.retain(|r| r.index != *index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refresh_is_followed_step_by_step() {
        let index = Path::new("/nowhere/progress-test/index.db");
        assert_eq!(of(index), None);
        {
            let _running = begin(Some(index));
            let p = of(index).unwrap();
            assert_eq!((p.step, p.of, p.name), (1, 5, "looking for changed files"));
            assert_eq!(p.files, None);
            step(Some(index), "frontend code", Some(412));
            let p = of(index).unwrap();
            assert_eq!((p.step, p.name, p.files), (4, "frontend code", Some(412)));
            assert!(
                p.text()
                    .starts_with("step 4 of 5 (frontend code), 412 changed files, "),
                "{}",
                p.text()
            );
            // A later step keeps the count.
            step(Some(index), "Swift, Kotlin and Rust", None);
            assert_eq!(of(index).unwrap().files, Some(412));
        }
        assert_eq!(of(index), None);
        // Nothing to track for an index in memory.
        let _none = begin(None);
        step(None, "reading PHP", Some(1));
    }
}
