//! Startup timing marks, printed only when `COLORMS_TRACE` is set.
//!
//! Times are measured from process creation (`GetProcessTimes`), not from the
//! first line of `main`, so they include the Windows loader — that is what a
//! user actually waits for. Run with `COLORMS_TRACE=1` and redirect stderr.

#![cfg_attr(not(windows), allow(dead_code))]

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FileTime {
    low: u32,
    high: u32,
}

impl FileTime {
    fn value(self) -> u64 {
        ((self.high as u64) << 32) | self.low as u64
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> isize;
    fn GetProcessTimes(
        process: isize,
        creation: *mut FileTime,
        exit: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    fn GetSystemTimePreciseAsFileTime(time: *mut FileTime);
}

/// Milliseconds since this process was created.
#[cfg(windows)]
pub fn elapsed_ms() -> f64 {
    unsafe {
        let mut creation = FileTime::default();
        let mut exit = FileTime::default();
        let mut kernel = FileTime::default();
        let mut user = FileTime::default();
        let mut now = FileTime::default();
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        );
        GetSystemTimePreciseAsFileTime(&mut now);
        let creation = creation.value();
        let now = now.value();
        if now <= creation {
            return 0.0;
        }
        // FILETIME counts 100 ns ticks.
        (now - creation) as f64 / 10_000.0
    }
}

#[cfg(not(windows))]
pub fn elapsed_ms() -> f64 {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs_f64()
        * 1000.0
}

fn enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("COLORMS_TRACE").is_some_and(|value| value != "0"))
}

/// Marks are buffered: writing to a redirected stderr costs ~1.5 ms per line on
/// Windows, which is more than the stages being measured.
static MARKS: std::sync::Mutex<Vec<(f64, String)>> = std::sync::Mutex::new(Vec::new());

/// Print `label` with the time since process creation.
pub fn mark(label: &str) {
    if !enabled() {
        return;
    }
    let elapsed = elapsed_ms();
    if let Ok(mut marks) = MARKS.lock() {
        marks.push((elapsed, label.to_string()));
    }
}

/// Write every buffered mark to stderr in one go.
pub fn flush() {
    if !enabled() {
        return;
    }
    let Ok(mut marks) = MARKS.lock() else {
        return;
    };
    if marks.is_empty() {
        return;
    }
    let mut text = String::with_capacity(marks.len() * 48);
    for (elapsed, label) in marks.iter() {
        text.push_str(&format!("t=+{elapsed:8.1} ms  {label}\n"));
    }
    marks.clear();
    eprint!("{text}");
}
