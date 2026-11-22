//! Time and chance, injected: every wait rsi.py makes is measured, slept or
//! spread with these, so a test can run hours of pauses instantly and exactly.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    /// Epoch seconds - Python's `time.time()`.
    fn time(&self) -> f64;
    /// Seconds on a clock that never goes back - `time.monotonic()`.
    fn monotonic(&self) -> f64;
    fn sleep(&self, seconds: f64);
    /// A random number in [a, b] - `random.uniform`, used only to spread
    /// waits, so it lives with the time.
    fn uniform(&self, a: f64, b: f64) -> f64;
}

/// The real clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn time(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }

    fn monotonic(&self) -> f64 {
        static START: OnceLock<Instant> = OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_secs_f64()
    }

    fn sleep(&self, seconds: f64) {
        if seconds > 0.0 && seconds.is_finite() {
            std::thread::sleep(Duration::from_secs_f64(seconds));
        }
    }

    fn uniform(&self, a: f64, b: f64) -> f64 {
        // splitmix64: jitter needs spread, not secrecy.
        static STATE: AtomicU64 = AtomicU64::new(0);
        let seed = STATE.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
            ^ SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        a + (b - a) * ((z >> 11) as f64 / (1u64 << 53) as f64)
    }
}
