//! Watch folders: the pure parts (no Windows calls).
//!
//! - [`parse_watch_op`]: the `op` text of a `[[watch]]` entry.
//! - [`pattern_matches`] and [`WatchEntry::accepts`]: the `include` patterns.
//! - [`ignored_name`], [`in_output_folder`]: what a watcher never converts.
//! - [`Stability`]: Filee's rule. A file is ready when its size and time stay
//!   the same for 2 seconds and it opens without sharing. The clock is a
//!   parameter (a `Duration` since any fixed start), so tests need no sleeping.

use crate::config::WatchEntry;
use crate::format::FormatId;
use crate::ops::{Operation, ToolKind};
use std::path::{Component, Path};
use std::time::Duration;

/// A file must keep its size and time for this long.
pub const STABLE_FOR: Duration = Duration::from_secs(2);
/// Give up on a file after this long.
pub const GIVE_UP_AFTER: Duration = Duration::from_secs(5 * 60);
/// Check an unstable file again after this long.
pub const RECHECK: Duration = Duration::from_millis(500);
/// First wait after "file is open elsewhere"; it doubles up to [`BACKOFF_MAX`].
pub const BACKOFF_START: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Operation of a watch entry: `convert:jpg` or a tool name. Tools that need a
/// dialog (Trim, Crop) or take several files (Merge PDFs) are not allowed.
pub fn parse_watch_op(text: &str) -> Option<Operation> {
    let t = text.trim();
    if let Some(ext) = t.strip_prefix("convert:") {
        return FormatId::from_extension(ext.trim()).map(Operation::Convert);
    }
    ToolKind::from_slug(t)
        .filter(|k| !k.needs_dialog() && !k.is_batch())
        .map(Operation::Tool)
}

/// Text for `WatchEntry::op` (inverse of [`parse_watch_op`]).
pub fn watch_op_text(op: Operation) -> Option<String> {
    match op {
        Operation::Convert(f) => Some(format!("convert:{}", f.extension())),
        Operation::Tool(t) if !t.needs_dialog() && !t.is_batch() => Some(t.slug()),
        _ => None,
    }
}

/// Case-insensitive match of `name` against a pattern with `*` (any text) and `?` (one character).
pub fn pattern_matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

impl WatchEntry {
    /// Whether the file name passes the `include` list (an empty list passes all).
    pub fn accepts(&self, file_name: &str) -> bool {
        self.include.is_empty() || self.include.iter().any(|p| pattern_matches(p.trim(), file_name))
    }
}

/// Names that a watcher never converts: Zest's own temp files, partial
/// downloads and system files.
pub fn ignored_name(name: &str) -> bool {
    let l = name.to_lowercase();
    l.starts_with("~$")
        || l == "desktop.ini"
        || l == "thumbs.db"
        || [".zest-tmp", ".crdownload", ".part", ".partial", ".tmp", ".download"]
            .iter()
            .any(|s| l.ends_with(s))
}

/// True when `path` is inside an output folder of Zest (`<name> (pages)`, or a
/// `.zest-tmp` folder) below `root`. Folders above `root` do not count.
pub fn in_output_folder(root: &Path, path: &Path) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let Some(parent) = rel.parent() else { return false };
    parent.components().any(|c| match c {
        Component::Normal(n) => {
            let n = n.to_string_lossy().to_lowercase();
            n.ends_with(" (pages)") || n.ends_with(".zest-tmp")
        }
        _ => false,
    })
}

/// What a check of the file on disk found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSig {
    pub size: u64,
    /// Modified time in any fixed unit (for example 100 ns ticks).
    pub mtime: u64,
}

/// What to do with a file after a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Check again at [`Stability::next_check`].
    Wait,
    /// Stable and not held by another program: convert it now.
    Ready,
    /// The file is gone.
    Gone,
    /// Five minutes passed without a result.
    GiveUp,
}

/// The state of one file that waits to become ready.
#[derive(Debug, Clone)]
pub struct Stability {
    first_seen: Duration,
    sig: Option<FileSig>,
    changed_at: Duration,
    next_check: Duration,
    busy_count: u32,
}

impl Stability {
    /// A file seen at time `now`; the first check is due at once.
    pub fn new(now: Duration) -> Self {
        Stability { first_seen: now, sig: None, changed_at: now, next_check: now, busy_count: 0 }
    }

    /// When `check` should run next.
    pub fn next_check(&self) -> Duration {
        self.next_check
    }

    /// A new event for the file (it changed again): it must be stable for 2 s again.
    pub fn touched(&mut self, now: Duration) {
        self.changed_at = now;
        self.sig = None;
        self.next_check = now;
    }

    /// Look at the file. `sig` is `None` if it does not exist. `can_open_exclusive`
    /// opens it with share mode 0; it runs only when the file is stable.
    pub fn check(
        &mut self,
        now: Duration,
        sig: Option<FileSig>,
        can_open_exclusive: impl FnOnce() -> bool,
    ) -> Verdict {
        let Some(sig) = sig else { return Verdict::Gone };
        if now.saturating_sub(self.first_seen) > GIVE_UP_AFTER {
            return Verdict::GiveUp;
        }
        if self.sig != Some(sig) {
            self.sig = Some(sig);
            self.changed_at = now;
            self.next_check = now + RECHECK;
            return Verdict::Wait;
        }
        let stable_for = now.saturating_sub(self.changed_at);
        if stable_for < STABLE_FOR {
            self.next_check = now + (STABLE_FOR - stable_for);
            return Verdict::Wait;
        }
        if can_open_exclusive() {
            return Verdict::Ready;
        }
        // Held by another program: wait 1 s, 2 s, 4 s ... up to 30 s.
        let wait = BACKOFF_START
            .saturating_mul(1u32 << self.busy_count.min(5))
            .min(BACKOFF_MAX);
        self.busy_count += 1;
        self.next_check = now + wait;
        Verdict::Wait
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }
    fn sig(size: u64) -> Option<FileSig> {
        Some(FileSig { size, mtime: 7 })
    }

    #[test]
    fn patterns() {
        assert!(pattern_matches("*.heic", "IMG_0001.HEIC"));
        assert!(pattern_matches("*.heic", ".heic"));
        assert!(!pattern_matches("*.heic", "a.heic.part"));
        assert!(pattern_matches("img_????.jpg", "IMG_0042.jpg"));
        assert!(!pattern_matches("img_????.jpg", "IMG_042.jpg"));
        assert!(pattern_matches("*", "anything"));
        assert!(pattern_matches("a*b*c", "a--b--c"));
        assert!(!pattern_matches("a*b*c", "a--b--d"));
        assert!(pattern_matches("scan*", "Scan 1.png"));
        assert!(pattern_matches("exact.png", "Exact.PNG"));
        assert!(!pattern_matches("", "x"));
    }

    #[test]
    fn include_list() {
        let mut e = WatchEntry {
            folder: "D:\\in".into(),
            op: "convert:jpg".into(),
            recursive: false,
            enabled: true,
            include: vec![],
        };
        assert!(e.accepts("a.png"));
        e.include = vec!["*.heic".into(), " shot*.png ".into()];
        assert!(e.accepts("a.HEIC") && e.accepts("Shot 2.png"));
        assert!(!e.accepts("a.png"));
    }

    #[test]
    fn operations() {
        use FormatId::*;
        assert_eq!(parse_watch_op("convert:jpg"), Some(Operation::Convert(Jpeg)));
        assert_eq!(parse_watch_op(" convert:PNG "), Some(Operation::Convert(Png)));
        assert_eq!(parse_watch_op("compress"), Some(Operation::Tool(ToolKind::Compress)));
        assert_eq!(parse_watch_op("size25"), Some(Operation::Tool(ToolKind::VideoTargetSize(25))));
        assert_eq!(parse_watch_op("merge-pdf"), None);
        assert_eq!(parse_watch_op("trim-1-2"), None);
        assert_eq!(parse_watch_op("crop-1-2-3-4"), None);
        assert_eq!(parse_watch_op("convert:xyz"), None);
        assert_eq!(parse_watch_op("zip"), None);
        for op in [Operation::Convert(Webp), Operation::Tool(ToolKind::Resize50)] {
            assert_eq!(parse_watch_op(&watch_op_text(op).unwrap()), Some(op));
        }
        assert_eq!(watch_op_text(Operation::Tool(ToolKind::MergePdf)), None);
    }

    #[test]
    fn ignore_rules() {
        assert!(ignored_name("a.jpg.zest-tmp"));
        assert!(ignored_name("Movie.mp4.crdownload"));
        assert!(ignored_name("~$doc.docx"));
        assert!(ignored_name("Thumbs.db"));
        assert!(!ignored_name("photo.jpg"));
        let root = Path::new(r"D:\in");
        assert!(in_output_folder(root, Path::new(r"D:\in\scan (pages)\scan-p001.png")));
        assert!(in_output_folder(root, Path::new(r"D:\in\a\b (pages)\c\x.png")));
        assert!(!in_output_folder(root, Path::new(r"D:\in\scan.png")));
        assert!(!in_output_folder(root, Path::new(r"D:\in\sub\scan.png")));
        // A watched folder that is itself a "(pages)" folder is the user's choice.
        assert!(!in_output_folder(Path::new(r"D:\x (pages)"), Path::new(r"D:\x (pages)\a.png")));
    }

    #[test]
    fn ready_after_two_quiet_seconds() {
        let mut s = Stability::new(ms(0));
        // First look: the signature is new, so wait.
        assert_eq!(s.check(ms(0), sig(10), || panic!("not stable yet")), Verdict::Wait);
        assert_eq!(s.next_check(), ms(500));
        assert_eq!(s.check(ms(500), sig(10), || panic!("not stable yet")), Verdict::Wait);
        assert_eq!(s.check(ms(1900), sig(10), || panic!("not stable yet")), Verdict::Wait);
        assert!(s.next_check() <= ms(2000) && s.next_check() > ms(1900));
        assert_eq!(s.check(ms(2000), sig(10), || true), Verdict::Ready);
    }

    #[test]
    fn growth_restarts_the_wait() {
        let mut s = Stability::new(ms(0));
        s.check(ms(0), sig(10), || true);
        assert_eq!(s.check(ms(1500), sig(20), || panic!("still growing")), Verdict::Wait);
        // 2 s after the change at 1500, not after the first look.
        assert_eq!(s.check(ms(3000), sig(20), || panic!("only 1.5 s")), Verdict::Wait);
        assert_eq!(s.check(ms(3500), sig(20), || true), Verdict::Ready);
        // A changed time alone also restarts it.
        let mut s = Stability::new(ms(0));
        s.check(ms(0), sig(10), || true);
        let other = Some(FileSig { size: 10, mtime: 8 });
        assert_eq!(s.check(ms(2500), other, || panic!("time changed")), Verdict::Wait);
    }

    #[test]
    fn touched_restarts_the_wait() {
        let mut s = Stability::new(ms(0));
        s.check(ms(0), sig(10), || true);
        s.touched(ms(1800));
        assert_eq!(s.check(ms(2100), sig(10), || panic!("touched")), Verdict::Wait);
        assert_eq!(s.check(ms(4200), sig(10), || true), Verdict::Ready);
    }

    #[test]
    fn busy_file_backs_off_then_succeeds() {
        let mut s = Stability::new(ms(0));
        s.check(ms(0), sig(5), || true);
        let mut t = ms(2000);
        let mut waits = Vec::new();
        for _ in 0..7 {
            assert_eq!(s.check(t, sig(5), || false), Verdict::Wait);
            waits.push(s.next_check() - t);
            t = s.next_check();
        }
        assert_eq!(
            waits,
            [1, 2, 4, 8, 16, 30, 30].map(Duration::from_secs),
            "doubles, capped at 30 s"
        );
        assert_eq!(s.check(t, sig(5), || true), Verdict::Ready);
    }

    #[test]
    fn gone_and_give_up() {
        let mut s = Stability::new(ms(0));
        assert_eq!(s.check(ms(100), None, || true), Verdict::Gone);
        let mut s = Stability::new(ms(0));
        s.check(ms(0), sig(5), || true);
        assert_eq!(s.check(Duration::from_secs(299), sig(5), || false), Verdict::Wait);
        assert_eq!(s.check(Duration::from_secs(301), sig(5), || false), Verdict::GiveUp);
        // A file that keeps growing is given up on, too.
        let mut s = Stability::new(ms(0));
        let mut size = 1;
        let mut t = ms(0);
        while t < Duration::from_secs(300) {
            assert_eq!(s.check(t, sig(size), || true), Verdict::Wait);
            size += 1;
            t += Duration::from_secs(1);
        }
        assert_eq!(s.check(t + Duration::from_secs(2), sig(size + 5), || true), Verdict::GiveUp);
    }
}
