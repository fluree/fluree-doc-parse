//! Test support: whether a piece of work grows in step with its input.
//!
//! A test that a reader takes linear time once held its run under a fixed
//! number of seconds. The bound was set on a fast machine for a debug build,
//! and a slower runner crossed it on work that was linear all along. What
//! tells linear work from quadratic is how the time grows with the input,
//! which no machine's speed changes: four times the input takes about four
//! times as long when the work is linear, and sixteen when it is quadratic.

use std::time::{Duration, Instant};

/// The most the run at four times the input may take, as a multiple of the
/// run at the input: between linear's 4 and quadratic's 16, with room for a
/// run a little over linear (an `n log n` sort is under 5) and for noise.
pub const MAX_GROWTH: f64 = 8.0;

/// The least the run at the smaller input must take for its time to mean
/// something against the timer and the machine's noise. Below it, the input
/// is doubled until it does.
pub const MIN_RUN: Duration = Duration::from_millis(25);

/// Asserts that `run` on `make(4 n)` takes at most [`MAX_GROWTH`] times as
/// long as on `make(n)`: linear in `n`, not quadratic.
///
/// `n` is doubled first while a run on `make(n)` takes under [`MIN_RUN`],
/// up to 64 times; a run still under it counts as taking [`MIN_RUN`].
/// Building the input is not timed. Each size is run three times and its
/// fastest run kept, so a pause of the machine's, or a test running beside
/// this one, does not count as the work's.
pub fn assert_linear<T>(what: &str, n: usize, make: impl Fn(usize) -> T, run: impl Fn(&T)) {
    let time = |n: usize| {
        let input = make(n);
        (0..3)
            .map(|_| {
                let start = Instant::now();
                run(&input);
                start.elapsed()
            })
            .min()
            .unwrap_or_default()
    };
    let start = n.max(1);
    let mut n = start;
    let mut small = time(n);
    while small < MIN_RUN && n < start * 64 {
        n *= 2;
        small = time(n);
    }
    // Work that barely grows (an input refused at once) is measured against
    // the least run that means something, not against the timer's noise.
    let large = time(4 * n);
    let growth = large.as_secs_f64() / small.max(MIN_RUN).as_secs_f64();
    assert!(
        growth <= MAX_GROWTH,
        "{what}: {small:?} at {n}, {large:?} at {} ({growth:.1} times)",
        4 * n
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_work_passes_and_quadratic_work_fails() {
        // The work is a sleep as long as the work would take, so the test
        // checks the helper's arithmetic, not the runner: a sleep is not
        // slowed by the tests running beside this one, as counting is.
        let sleep = |micros: usize| std::thread::sleep(Duration::from_micros(micros as u64));
        assert_linear("linear", 30_000, |n| n, |&n| sleep(n));
        let quadratic = std::panic::catch_unwind(|| {
            assert_linear("quadratic", 6_000, |n| n, |&n| sleep(n * n / 1_000))
        });
        assert!(quadratic.is_err());
    }
}
