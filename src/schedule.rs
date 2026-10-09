//! Fixed-phase scheduling that skips missed slots instead of catching up in a burst.
pub fn next_deadline_ms(previous: u64, now: u64, interval: u64) -> u64 {
    assert!(interval > 0);
    previous + ((now.saturating_sub(previous) / interval) + 1) * interval
}

/// Time not counted by the native timer while clocks were gated. Saturation
/// prevents millisecond AON quantization from moving application time backward.
pub fn sleep_correction_us(aon_elapsed_ms: u64, native_elapsed_us: u64) -> u64 {
    (aon_elapsed_ms * 1000).saturating_sub(native_elapsed_us)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upload_time_does_not_extend_interval() {
        assert_eq!(next_deadline_ms(0, 5_000, 900_000), 900_000);
        assert_eq!(next_deadline_ms(900_000, 960_000, 900_000), 1_800_000);
    }
    #[test]
    fn missed_slots_are_skipped_without_burst_sampling() {
        assert_eq!(next_deadline_ms(0, 1_900_000, 900_000), 2_700_000);
        assert_eq!(next_deadline_ms(0, 900_000, 900_000), 1_800_000);
    }
    #[test]
    fn sleep_is_included_without_double_counting_entry_and_exit() {
        let before_native = 60_000_000;
        let after_native = before_native + 2_500;
        let correction = sleep_correction_us(840_000, 2_500);
        assert_eq!(after_native + correction, 900_000_000);
        // Another 60 seconds awake, then 840 seconds asleep: 30 minutes total.
        let next_native = after_native + 60_000_000 + 1_000;
        let next_correction = correction + sleep_correction_us(840_000, 1_000);
        assert_eq!(next_native + next_correction, 1_800_000_000);
    }
    #[test]
    fn aon_rounding_cannot_subtract_from_uptime() {
        assert_eq!(sleep_correction_us(1, 1_100), 0);
    }
}
