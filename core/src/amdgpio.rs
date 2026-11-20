use crate::hw::Registers;

pub const MASTER: usize = 0xfc;
pub const STATUS_LOW: usize = 0x2f8;
pub const STATUS_HIGH: usize = 0x2fc;
pub const END_OF_INTERRUPT: u32 = 1 << 29;
pub const LEVEL_TRIGGER: u32 = 1 << 8;
pub const ACTIVE_LEVEL_SHIFT: u32 = 9;
pub const ACTIVE_LEVEL_MASK: u32 = 3 << ACTIVE_LEVEL_SHIFT;
pub const ACTIVE_LOW: u32 = 1;
pub const INTERRUPT_ENABLE: u32 = 1 << 11;
pub const INTERRUPT_MASK: u32 = 1 << 12;
pub const PIN_STATE: u32 = 1 << 16;
pub const INTERRUPT_STATUS: u32 = 1 << 28;
pub const WAKE_STATUS: u32 = 1 << 29;
pub const PENDING: u32 = INTERRUPT_STATUS | WAKE_STATUS;
pub const PINS: u16 = 184;
const GROUPS: u32 = 46;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trigger {
    pub level: bool,
    pub active_low: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Serviced {
    pub ours: bool,
    pub silenced: u32,
}

fn register(pin: u16) -> usize {
    usize::from(pin) * 4
}

pub fn arm<R: Registers>(registers: &mut R, pin: u16, trigger: Trigger) -> Option<()> {
    if pin >= PINS {
        return None;
    }
    let mut value = registers.read(register(pin)) & !PENDING;
    value = if trigger.level { value | LEVEL_TRIGGER } else { value & !LEVEL_TRIGGER };
    value = (value & !ACTIVE_LEVEL_MASK) | (u32::from(trigger.active_low) * ACTIVE_LOW) << ACTIVE_LEVEL_SHIFT;
    registers.write(register(pin), value | INTERRUPT_STATUS);
    registers.write(register(pin), (value | INTERRUPT_ENABLE | INTERRUPT_MASK) & !PENDING);
    Some(())
}

pub fn disarm<R: Registers>(registers: &mut R, pin: u16) {
    if pin < PINS {
        let value = registers.read(register(pin)) & !PENDING;
        registers.write(register(pin), value & !(INTERRUPT_ENABLE | INTERRUPT_MASK));
    }
}

pub fn asserted<R: Registers>(registers: &mut R, pin: u16, trigger: Trigger) -> bool {
    let high = registers.read(register(pin)) & PIN_STATE != 0;
    high != trigger.active_low
}

pub fn service<R: Registers>(registers: &mut R, ours: u16) -> Serviced {
    let status = u64::from(registers.read(STATUS_HIGH)) << 32 | u64::from(registers.read(STATUS_LOW));
    let mut serviced = Serviced { ours: false, silenced: 0 };
    for group in (0..GROUPS).filter(|group| status >> group & 1 == 1) {
        for pin in (group * 4..group * 4 + 4).map(|pin| pin as u16) {
            let value = registers.read(register(pin));
            if value & PENDING == 0 || value & INTERRUPT_MASK == 0 {
                continue;
            }
            if pin == ours {
                serviced.ours = true;
                registers.write(register(pin), value);
            } else {
                serviced.silenced += 1;
                registers.write(register(pin), value & !INTERRUPT_MASK);
            }
        }
    }
    let master = registers.read(MASTER);
    registers.write(MASTER, master | END_OF_INTERRUPT);
    serviced
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Bank {
        pins: [u32; PINS as usize],
        master: u32,
        ends: usize,
    }

    impl Bank {
        fn new() -> Self {
            Bank { pins: [0; PINS as usize], master: 0, ends: 0 }
        }

        fn raise(&mut self, pin: u16) {
            self.pins[usize::from(pin)] |= INTERRUPT_STATUS;
        }
    }

    impl Registers for Bank {
        fn read(&mut self, offset: usize) -> u32 {
            let grouped = |range: core::ops::Range<u32>, pins: &[u32]| {
                range
                    .filter(|group| (0..4).any(|i| pins.get((group * 4 + i) as usize).is_some_and(|p| p & PENDING != 0)))
                    .fold(0u32, |bits, group| bits | 1 << (group % 32))
            };
            match offset {
                MASTER => self.master,
                STATUS_LOW => grouped(0..32, &self.pins),
                STATUS_HIGH => grouped(32..GROUPS, &self.pins),
                pin => self.pins[pin / 4],
            }
        }

        fn write(&mut self, offset: usize, value: u32) {
            match offset {
                MASTER => {
                    if value & END_OF_INTERRUPT != 0 {
                        self.ends += 1;
                    }
                    self.master = value & !END_OF_INTERRUPT;
                }
                pin => {
                    let old = self.pins[pin / 4];
                    let cleared = old & PENDING & value;
                    self.pins[pin / 4] = (value & !PENDING) | (old & PENDING & !cleared);
                }
            }
        }
    }

    const PAD: Trigger = Trigger { level: true, active_low: true };

    #[test]
    fn arming_sets_a_level_low_interrupt_and_drops_a_stale_status() {
        let mut bank = Bank::new();
        bank.pins[9] = 0x0004_0000 | INTERRUPT_STATUS;
        arm(&mut bank, 9, PAD).unwrap();
        let value = bank.pins[9];
        assert_eq!(value & (LEVEL_TRIGGER | ACTIVE_LEVEL_MASK | INTERRUPT_ENABLE | INTERRUPT_MASK), LEVEL_TRIGGER | 1 << 9 | INTERRUPT_ENABLE | INTERRUPT_MASK);
        assert_eq!(value & PENDING, 0);
        assert_eq!(value & 0x0004_0000, 0x0004_0000);
        assert_eq!(arm(&mut bank, PINS, PAD), None);
        disarm(&mut bank, 9);
        assert_eq!(bank.pins[9] & (INTERRUPT_ENABLE | INTERRUPT_MASK), 0);
    }

    #[test]
    fn servicing_acknowledges_our_pin_silences_strangers_and_ends_the_interrupt() {
        let mut bank = Bank::new();
        arm(&mut bank, 9, PAD).unwrap();
        bank.pins[44] = INTERRUPT_ENABLE | INTERRUPT_MASK;
        bank.pins[58] = INTERRUPT_ENABLE;
        bank.raise(9);
        bank.raise(44);
        bank.raise(58);
        let serviced = service(&mut bank, 9);
        assert_eq!(serviced, Serviced { ours: true, silenced: 1 });
        assert_eq!(bank.pins[9] & PENDING, 0);
        assert_eq!(bank.pins[9] & INTERRUPT_MASK, INTERRUPT_MASK);
        assert_eq!(bank.pins[44] & (PENDING | INTERRUPT_MASK), 0);
        assert_eq!(bank.pins[58] & PENDING, INTERRUPT_STATUS);
        assert_eq!(bank.ends, 1);
        assert_eq!(service(&mut bank, 9), Serviced { ours: false, silenced: 0 });
        assert_eq!(bank.ends, 2);
    }

    #[test]
    fn an_active_low_line_is_asserted_while_it_reads_low() {
        let mut bank = Bank::new();
        assert!(asserted(&mut bank, 9, PAD));
        bank.pins[9] |= PIN_STATE;
        assert!(!asserted(&mut bank, 9, PAD));
        assert!(asserted(&mut bank, 9, Trigger { level: true, active_low: false }));
    }
}
