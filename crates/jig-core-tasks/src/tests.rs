use crate::{Retry, failures::backoff};
use skein_lib::{Duration, Rng};

#[test]
fn exponential_equal_jitter_saturates_without_overflow_and_replays() {
    let retry = Retry { retries: 4, base: Duration::from_millis(10), max: Duration::from_secs(1) };
    let mut first = Rng::new(42);
    let mut second = Rng::new(42);
    for times in [1, 2, 3, 4, 7, 32, 64, u32::MAX] {
        let factor = 1_u64.checked_shl(times.saturating_sub(1)).unwrap_or(u64::MAX);
        let ceiling = retry.base.saturating_mul(factor).min(retry.max);
        let pause = backoff(times, retry, &mut first);
        assert!(
            pause.as_nanos() >= ceiling.as_nanos().div_euclid(2) && pause <= ceiling,
            "pause stays in equal-jitter half-to-ceiling range"
        );
        assert_eq!(pause, backoff(times, retry, &mut second), "same seed replays backoff");
    }
}
