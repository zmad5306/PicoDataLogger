const INITIAL_RETRY_DELAY_SECS: u64 = 1;
const MAX_RETRY_DELAY_SECS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryBackoff {
    next_delay_secs: u64,
}

impl RetryBackoff {
    pub const fn new() -> Self {
        Self {
            next_delay_secs: INITIAL_RETRY_DELAY_SECS,
        }
    }

    pub fn next_delay_secs(&mut self) -> u64 {
        let delay_secs = self.next_delay_secs;

        self.next_delay_secs = delay_secs.saturating_mul(2).min(MAX_RETRY_DELAY_SECS);

        delay_secs
    }

    pub fn reset(&mut self) {
        self.next_delay_secs = INITIAL_RETRY_DELAY_SECS;
    }
}

impl Default for RetryBackoff {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_until_the_sixty_second_cap() {
        let mut backoff = RetryBackoff::new();

        for expected in [1, 2, 4, 8, 16, 32, 60, 60] {
            assert_eq!(backoff.next_delay_secs(), expected);
        }
    }

    #[test]
    fn reset_restores_the_initial_delay() {
        let mut backoff = RetryBackoff::new();

        assert_eq!(backoff.next_delay_secs(), 1);
        assert_eq!(backoff.next_delay_secs(), 2);
        assert_eq!(backoff.next_delay_secs(), 4);

        backoff.reset();

        assert_eq!(backoff.next_delay_secs(), 1);
    }
}
