//! Helpers for the P3a measurement tests (tests only).
use super::Window;
use loog_editor_core::text::TextBuffer;
use std::time::{Duration, Instant};

pub(crate) const SAMPLES: usize = 50;
pub(crate) const MIB: usize = 1024 * 1024;

/// Prints the samples and returns the 95th percentile.
pub(crate) fn report(label: &str, mut samples: Vec<Duration>) -> Duration {
    let raw: Vec<String> = samples.iter().map(|d| format!("{:.2}", ms(*d))).collect();
    samples.sort();
    let at = |q: f64| ms(samples[((samples.len() - 1) as f64 * q).round() as usize]);
    println!(
        "BENCH {label}: n={} p50={:.2}ms p95={:.2}ms max={:.2}ms\n  raw(ms)=[{}]",
        samples.len(),
        at(0.5),
        at(0.95),
        ms(*samples.last().unwrap()),
        raw.join(", ")
    );
    samples[((samples.len() - 1) as f64 * 0.95).round() as usize]
}

pub(crate) fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

pub(crate) fn timed<T>(mut run: impl FnMut() -> T) -> Vec<Duration> {
    drop(run());
    (0..SAMPLES)
        .map(|_| {
            let started = Instant::now();
            drop(run());
            started.elapsed()
        })
        .collect()
}

/// The windows the editor asks for when the top of the file is visible: one per
/// line, as `with_highlights` builds them.
pub(crate) fn viewport(buffer: &TextBuffer, first_line: usize, lines: usize) -> Vec<Window> {
    (first_line..(first_line + lines).min(buffer.line_count()))
        .map(|line| Window {
            line: buffer.line_range(line),
            range: buffer.line_range(line),
        })
        .collect()
}
