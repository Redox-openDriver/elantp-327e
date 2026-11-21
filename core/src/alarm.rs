use alloc::vec::Vec;

pub const SLACK: u64 = 1_000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Alarm {
    pending: Vec<u64>,
}

impl Alarm {
    pub fn schedule(&mut self, wake: u64) -> bool {
        if self.pending.iter().any(|at| *at <= wake) {
            return false;
        }
        self.pending.push(wake);
        true
    }

    pub fn fired(&mut self, now: u64) {
        self.pending.retain(|at| *at > now.saturating_add(SLACK));
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timeout_is_registered_only_when_none_comes_sooner() {
        let mut alarm = Alarm::default();
        assert!(alarm.schedule(1_000_000));
        assert!(!alarm.schedule(1_000_000));
        assert!(!alarm.schedule(2_000_000));
        assert!(alarm.schedule(500_000));
        assert_eq!(alarm.pending(), 2);
        alarm.fired(499_500);
        assert_eq!(alarm.pending(), 1);
        alarm.fired(1_000_000);
        assert_eq!(alarm.pending(), 0);
    }

    #[test]
    fn a_deadline_moving_on_every_frame_does_not_pile_up_timeouts() {
        let mut alarm = Alarm::default();
        let mut registered = 0;
        for frame in 0..1_000u64 {
            let now = frame * 8_000;
            if frame % 5 == 0 {
                alarm.fired(now);
            }
            registered += usize::from(alarm.schedule(now + 8_000));
            assert!(alarm.pending() <= 2, "{frame}");
        }
        assert!(registered <= 250, "{registered}");
    }
}
