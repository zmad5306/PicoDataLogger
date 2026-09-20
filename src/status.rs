/// Observable application state shown by the Pico 2 W onboard LED.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedStatus {
    Idle,
    Publishing,
    Fault,
}

/// Pure state machine for the LED. Hardware code applies `is_on()` to WL_GPIO0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedStatusModel {
    status: LedStatus,
    fault_phase_on: bool,
}

impl LedStatusModel {
    pub const fn new() -> Self {
        Self {
            status: LedStatus::Idle,
            fault_phase_on: false,
        }
    }

    pub fn status(&self) -> LedStatus {
        self.status
    }

    pub fn transition(&mut self, status: LedStatus) {
        self.status = status;
        self.fault_phase_on = matches!(status, LedStatus::Fault);
    }

    /// Advance one 100 ms phase while fault indication is active.
    pub fn tick(&mut self) {
        if self.status == LedStatus::Fault {
            self.fault_phase_on = !self.fault_phase_on;
        }
    }

    pub fn is_on(&self) -> bool {
        match self.status {
            LedStatus::Idle => false,
            LedStatus::Publishing => true,
            LedStatus::Fault => self.fault_phase_on,
        }
    }
}

impl Default for LedStatusModel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_idle_and_off() {
        let led = LedStatusModel::new();
        assert_eq!(led.status(), LedStatus::Idle);
        assert!(!led.is_on());
    }

    #[test]
    fn publishing_has_priority_and_is_solid() {
        let mut led = LedStatusModel::new();
        led.transition(LedStatus::Fault);
        led.tick();
        led.transition(LedStatus::Publishing);
        assert_eq!(led.status(), LedStatus::Publishing);
        assert!(led.is_on());
        led.tick();
        assert!(led.is_on());
    }

    #[test]
    fn fault_alternates_each_tick() {
        let mut led = LedStatusModel::new();
        led.transition(LedStatus::Fault);
        assert!(led.is_on());
        led.tick();
        assert!(!led.is_on());
        led.tick();
        assert!(led.is_on());
    }

    #[test]
    fn success_returns_to_idle_and_off() {
        let mut led = LedStatusModel::new();
        led.transition(LedStatus::Fault);
        led.transition(LedStatus::Publishing);
        led.transition(LedStatus::Idle);
        assert_eq!(led.status(), LedStatus::Idle);
        assert!(!led.is_on());
    }
}
