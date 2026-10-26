//! Merge paths that arrive from several launches (pure, testable).
//!
//! Explorer's classic verb starts one process per selected file. Each one
//! forwards its path. The collector waits until no new path came for `quiet`,
//! but never longer than `max_wait` after the first one.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const QUIET: Duration = Duration::from_millis(150);
pub const MAX_WAIT: Duration = Duration::from_millis(2000);

#[derive(Debug)]
pub struct Collector {
    quiet: Duration,
    max_wait: Duration,
    paths: Vec<PathBuf>,
    first: Option<Instant>,
    last: Option<Instant>,
}

fn same(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

impl Collector {
    pub fn new(quiet: Duration, max_wait: Duration) -> Self {
        Collector { quiet, max_wait, paths: Vec::new(), first: None, last: None }
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.first.is_none()
    }

    /// Add paths (order kept, duplicates ignored, Windows case rules).
    pub fn push(&mut self, paths: impl IntoIterator<Item = PathBuf>, now: Instant) {
        let mut any = false;
        for p in paths {
            any = true;
            if !self.paths.iter().any(|q| same(q, &p)) {
                self.paths.push(p);
            }
        }
        if any {
            self.first.get_or_insert(now);
            self.last = Some(now);
        }
    }

    /// When `poll` will give the paths, if something is waiting.
    pub fn deadline(&self) -> Option<Instant> {
        let (first, last) = (self.first?, self.last?);
        Some((last + self.quiet).min(first + self.max_wait))
    }

    /// The collected paths once the time is up. Resets the collector.
    pub fn poll(&mut self, now: Instant) -> Option<Vec<PathBuf>> {
        if now >= self.deadline()? {
            self.first = None;
            self.last = None;
            Some(std::mem::take(&mut self.paths))
        } else {
            None
        }
    }
}

impl Default for Collector {
    fn default() -> Self {
        Collector::new(QUIET, MAX_WAIT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn nothing_waiting() {
        let mut c = Collector::default();
        assert!(c.is_empty());
        assert_eq!(c.deadline(), None);
        assert_eq!(c.poll(Instant::now()), None);
    }

    #[test]
    fn single_push_flushes_after_quiet_time() {
        let t0 = Instant::now();
        let mut c = Collector::default();
        c.push([PathBuf::from("a")], t0);
        assert_eq!(c.poll(t0 + ms(149)), None);
        assert_eq!(c.poll(t0 + ms(150)), Some(vec![PathBuf::from("a")]));
        assert!(c.is_empty());
        assert_eq!(c.poll(t0 + ms(1000)), None);
    }

    #[test]
    fn later_launches_extend_the_wait_and_merge() {
        let t0 = Instant::now();
        let mut c = Collector::default();
        c.push([PathBuf::from("a")], t0);
        c.push([PathBuf::from("b")], t0 + ms(100));
        assert_eq!(c.poll(t0 + ms(200)), None);
        c.push([PathBuf::from("c")], t0 + ms(240));
        assert_eq!(c.deadline(), Some(t0 + ms(390)));
        let got = c.poll(t0 + ms(390)).unwrap();
        assert_eq!(got, vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")]);
    }

    #[test]
    fn max_wait_caps_a_slow_stream() {
        let t0 = Instant::now();
        let mut c = Collector::new(ms(150), ms(500));
        for i in 0..10u64 {
            c.push([PathBuf::from(format!("f{i}"))], t0 + ms(i * 100));
        }
        // Last push at 900 ms, but the cap is first + 500 ms.
        assert_eq!(c.deadline(), Some(t0 + ms(500)));
        assert_eq!(c.poll(t0 + ms(500)).unwrap().len(), 10);
    }

    #[test]
    fn duplicates_ignored_case_insensitive() {
        let t0 = Instant::now();
        let mut c = Collector::default();
        c.push([PathBuf::from(r"C:\A\x.png"), PathBuf::from(r"c:\a\X.PNG")], t0);
        c.push([PathBuf::from(r"C:\A\x.png")], t0);
        assert_eq!(c.poll(t0 + ms(150)).unwrap().len(), 1);
    }

    #[test]
    fn empty_push_does_not_arm() {
        let mut c = Collector::default();
        c.push(Vec::<PathBuf>::new(), Instant::now());
        assert!(c.is_empty());
    }
}
