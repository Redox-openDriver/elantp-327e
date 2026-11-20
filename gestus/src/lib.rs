#![no_std]

extern crate alloc;

use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touch {
    pub id: u8,
    pub x: f32,
    pub y: f32,
    pub palm: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub time: u64,
    pub button: bool,
    pub touches: Vec<Touch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Begin,
    Update,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    Motion { dx: f32, dy: f32 },
    Press(Button),
    Release(Button),
    Scroll { dx: f32, dy: f32 },
    Swipe { fingers: u8, phase: Phase, dx: f32, dy: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub speed: f32,
    pub acceleration: f32,
    pub slow: f32,
    pub fast: f32,
    pub tap: bool,
    pub tap_time: u64,
    pub tap_travel: f32,
    pub scroll_speed: f32,
    pub scroll_start: f32,
    pub natural_scroll: bool,
    pub swipe_start: f32,
    pub gap: u64,
}

pub const DEFAULT: Settings = Settings {
    speed: 4.0,
    acceleration: 3.0,
    slow: 30.0,
    fast: 300.0,
    tap: true,
    tap_time: 180_000,
    tap_travel: 1.5,
    scroll_speed: 6.0,
    scroll_start: 1.0,
    natural_scroll: false,
    swipe_start: 5.0,
    gap: 100_000,
};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Finger {
    id: u8,
    x: f32,
    y: f32,
    travelled: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Idle,
    Pointer,
    Scroll,
    Swipe(u8),
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Sequence {
    started: u64,
    most: u8,
    travelled: f32,
    clicked: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gestures {
    settings: Settings,
    fingers: Vec<Finger>,
    mode: Mode,
    pending: (f32, f32),
    held: Option<Button>,
    sequence: Option<Sequence>,
    last: Option<u64>,
}

fn distance(dx: f32, dy: f32) -> f32 {
    let squared = dx * dx + dy * dy;
    if squared <= 0.0 {
        return 0.0;
    }
    let mut root = if squared > 1.0 { squared } else { 1.0 };
    for _ in 0..24 {
        root = 0.5 * (root + squared / root);
    }
    root
}

fn by_fingers(count: usize) -> Button {
    match count {
        0 | 1 => Button::Left,
        2 => Button::Right,
        _ => Button::Middle,
    }
}

impl Gestures {
    pub fn new(settings: Settings) -> Self {
        Gestures { settings, fingers: Vec::new(), mode: Mode::Idle, pending: (0.0, 0.0), held: None, sequence: None, last: None }
    }

    fn gain(&self, millimetres: f32, elapsed: u64) -> f32 {
        let s = &self.settings;
        if elapsed == 0 || elapsed > s.gap {
            return s.speed;
        }
        let velocity = millimetres * 1_000_000.0 / elapsed as f32;
        let ramp = ((velocity - s.slow) / (s.fast - s.slow)).clamp(0.0, 1.0);
        s.speed * (1.0 + (s.acceleration - 1.0) * ramp)
    }

    fn end_mode(&mut self, events: &mut Vec<Event>) {
        if let Mode::Swipe(fingers) = self.mode {
            events.push(Event::Swipe { fingers, phase: Phase::End, dx: 0.0, dy: 0.0 });
        }
        self.mode = Mode::Idle;
        self.pending = (0.0, 0.0);
    }

    pub fn feed(&mut self, input: &Input) -> Vec<Event> {
        let mut events = Vec::new();
        let elapsed = self.last.map_or(0, |last| input.time.saturating_sub(last));
        self.last = Some(input.time);
        let active: Vec<Touch> = input.touches.iter().copied().filter(|touch| !touch.palm).collect();

        let mut moves: Vec<(f32, f32)> = Vec::with_capacity(active.len());
        let mut next = Vec::with_capacity(active.len());
        for touch in &active {
            let previous = self.fingers.iter().find(|finger| finger.id == touch.id);
            let (dx, dy) = previous.map_or((0.0, 0.0), |p| (touch.x - p.x, touch.y - p.y));
            let travelled = previous.map_or(0.0, |p| p.travelled) + distance(dx, dy);
            if previous.is_some() {
                moves.push((dx, dy));
            }
            next.push(Finger { id: touch.id, x: touch.x, y: touch.y, travelled });
        }
        let same_fingers = next.len() == self.fingers.len() && next.iter().all(|f| self.fingers.iter().any(|p| p.id == f.id));
        self.fingers = next;

        if !active.is_empty() {
            let sequence = self.sequence.get_or_insert(Sequence { started: input.time, most: 0, travelled: 0.0, clicked: false });
            sequence.most = sequence.most.max(active.len() as u8);
            sequence.travelled = self.fingers.iter().fold(sequence.travelled, |most, finger| most.max(finger.travelled));
        }

        match (input.button, self.held) {
            (true, None) => {
                let button = by_fingers(active.len());
                self.held = Some(button);
                if let Some(sequence) = self.sequence.as_mut() {
                    sequence.clicked = true;
                }
                events.push(Event::Press(button));
            }
            (false, Some(button)) => {
                self.held = None;
                events.push(Event::Release(button));
            }
            _ => {}
        }

        if !same_fingers {
            self.end_mode(&mut events);
        }

        let count = active.len();
        let mean = if moves.is_empty() {
            (0.0, 0.0)
        } else {
            let n = moves.len() as f32;
            (moves.iter().map(|m| m.0).sum::<f32>() / n, moves.iter().map(|m| m.1).sum::<f32>() / n)
        };
        if same_fingers && !moves.is_empty() {
            if self.held.is_some() || count == 1 {
                let (dx, dy) = moves.iter().copied().fold((0.0f32, 0.0f32), |best, m| if distance(m.0, m.1) > distance(best.0, best.1) { m } else { best });
                let gain = self.gain(distance(dx, dy), elapsed);
                if dx != 0.0 || dy != 0.0 {
                    self.mode = Mode::Pointer;
                    events.push(Event::Motion { dx: dx * gain, dy: dy * gain });
                }
            } else if count == 2 {
                self.pending = (self.pending.0 + mean.0, self.pending.1 + mean.1);
                let started = self.mode == Mode::Scroll || distance(self.pending.0, self.pending.1) >= self.settings.scroll_start;
                if started {
                    let (dx, dy) = if self.mode == Mode::Scroll { mean } else { self.pending };
                    self.mode = Mode::Scroll;
                    let sign = if self.settings.natural_scroll { 1.0 } else { -1.0 };
                    if dx != 0.0 || dy != 0.0 {
                        events.push(Event::Scroll { dx: sign * dx * self.settings.scroll_speed, dy: sign * dy * self.settings.scroll_speed });
                    }
                }
            } else if count >= 3 {
                let fingers = count as u8;
                self.pending = (self.pending.0 + mean.0, self.pending.1 + mean.1);
                match self.mode {
                    Mode::Swipe(_) => events.push(Event::Swipe { fingers, phase: Phase::Update, dx: mean.0, dy: mean.1 }),
                    _ if distance(self.pending.0, self.pending.1) >= self.settings.swipe_start => {
                        self.mode = Mode::Swipe(fingers);
                        events.push(Event::Swipe { fingers, phase: Phase::Begin, dx: self.pending.0, dy: self.pending.1 });
                    }
                    _ => {}
                }
            }
        }

        if active.is_empty() {
            self.end_mode(&mut events);
            if let Some(sequence) = self.sequence.take() {
                let quick = input.time.saturating_sub(sequence.started) <= self.settings.tap_time;
                if self.settings.tap && quick && !sequence.clicked && sequence.travelled <= self.settings.tap_travel && self.held.is_none() {
                    let button = by_fingers(usize::from(sequence.most));
                    events.push(Event::Press(button));
                    events.push(Event::Release(button));
                }
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const STEP: u64 = 8_000;

    fn touch(id: u8, x: f32, y: f32) -> Touch {
        Touch { id, x, y, palm: false }
    }

    fn run(gestures: &mut Gestures, frames: &[(bool, Vec<Touch>)]) -> Vec<Event> {
        frames
            .iter()
            .enumerate()
            .flat_map(|(i, (button, touches))| gestures.feed(&Input { time: 1_000_000 + i as u64 * STEP, button: *button, touches: touches.clone() }))
            .collect()
    }

    fn slide(id: u8, from: (f32, f32), to: (f32, f32), frames: usize) -> Vec<Vec<Touch>> {
        (0..=frames)
            .map(|i| {
                let t = i as f32 / frames as f32;
                vec![touch(id, from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t)]
            })
            .collect()
    }

    fn total(events: &[Event]) -> (f32, f32) {
        events.iter().fold((0.0, 0.0), |sum, event| match event {
            Event::Motion { dx, dy } => (sum.0 + dx, sum.1 + dy),
            _ => sum,
        })
    }

    fn clicks(events: &[Event]) -> Vec<Event> {
        events.iter().copied().filter(|e| matches!(e, Event::Press(_) | Event::Release(_))).collect()
    }

    #[test]
    fn one_finger_moves_the_pointer_and_faster_moves_go_further() {
        let mut slow = Gestures::new(DEFAULT);
        let mut frames: Vec<(bool, Vec<Touch>)> = slide(1, (10.0, 10.0), (20.0, 10.0), 100).into_iter().map(|t| (false, t)).collect();
        frames.push((false, vec![]));
        let slow_events = run(&mut slow, &frames);
        let (dx, dy) = total(&slow_events);
        assert!((dx - 40.0).abs() < 0.5 && dy.abs() < 0.01, "{dx} {dy}");
        assert!(clicks(&slow_events).is_empty());
        let mut fast = Gestures::new(DEFAULT);
        let mut quick: Vec<(bool, Vec<Touch>)> = slide(1, (10.0, 10.0), (20.0, 10.0), 4).into_iter().map(|t| (false, t)).collect();
        quick.push((false, vec![]));
        let (fast_dx, _) = total(&run(&mut fast, &quick));
        assert!(fast_dx > 100.0, "{fast_dx}");
    }

    #[test]
    fn a_quick_still_touch_is_a_click_and_a_long_one_is_not() {
        let mut gestures = Gestures::new(DEFAULT);
        let tap = [(false, vec![touch(1, 30.0, 30.0)]), (false, vec![touch(1, 30.2, 30.1)]), (false, vec![])];
        assert_eq!(clicks(&run(&mut gestures, &tap)), [Event::Press(Button::Left), Event::Release(Button::Left)]);
        let mut held: Vec<(bool, Vec<Touch>)> = (0..40).map(|_| (false, vec![touch(2, 30.0, 30.0)])).collect();
        held.push((false, vec![]));
        assert!(clicks(&run(&mut Gestures::new(DEFAULT), &held)).is_empty());
        let mut off = DEFAULT;
        off.tap = false;
        assert!(clicks(&run(&mut Gestures::new(off), &tap)).is_empty());
    }

    #[test]
    fn two_fingers_tap_for_the_right_button_and_three_for_the_middle() {
        let two = [(false, vec![touch(1, 30.0, 30.0), touch(2, 50.0, 30.0)]), (false, vec![touch(1, 30.0, 30.0), touch(2, 50.0, 30.0)]), (false, vec![])];
        assert_eq!(clicks(&run(&mut Gestures::new(DEFAULT), &two)), [Event::Press(Button::Right), Event::Release(Button::Right)]);
        let three = [(false, vec![touch(1, 30.0, 30.0), touch(2, 50.0, 30.0), touch(3, 70.0, 30.0)]), (false, vec![])];
        assert_eq!(clicks(&run(&mut Gestures::new(DEFAULT), &three)), [Event::Press(Button::Middle), Event::Release(Button::Middle)]);
    }

    #[test]
    fn pressing_the_pad_clicks_by_the_number_of_fingers_and_is_not_also_a_tap() {
        let one = [(false, vec![touch(1, 30.0, 60.0)]), (true, vec![touch(1, 30.0, 60.0)]), (false, vec![touch(1, 30.0, 60.0)]), (false, vec![])];
        assert_eq!(clicks(&run(&mut Gestures::new(DEFAULT), &one)), [Event::Press(Button::Left), Event::Release(Button::Left)]);
        let two = [(true, vec![touch(1, 30.0, 60.0), touch(2, 50.0, 60.0)]), (false, vec![touch(1, 30.0, 60.0), touch(2, 50.0, 60.0)]), (false, vec![])];
        assert_eq!(clicks(&run(&mut Gestures::new(DEFAULT), &two)), [Event::Press(Button::Right), Event::Release(Button::Right)]);
    }

    #[test]
    fn two_fingers_moving_together_scroll_and_do_not_move_the_pointer() {
        let frames: Vec<(bool, Vec<Touch>)> = (0..=30).map(|i| (false, vec![touch(1, 40.0, 20.0 + i as f32), touch(2, 60.0, 20.0 + i as f32)])).collect();
        let events = run(&mut Gestures::new(DEFAULT), &frames);
        assert_eq!(total(&events), (0.0, 0.0));
        let scrolled: f32 = events.iter().map(|e| if let Event::Scroll { dy, .. } = e { *dy } else { 0.0 }).sum();
        assert!((scrolled + 30.0 * DEFAULT.scroll_speed).abs() < 0.5, "{scrolled}");
        let mut natural = DEFAULT;
        natural.natural_scroll = true;
        let flipped: f32 = run(&mut Gestures::new(natural), &frames).iter().map(|e| if let Event::Scroll { dy, .. } = e { *dy } else { 0.0 }).sum();
        assert!((flipped - 30.0 * DEFAULT.scroll_speed).abs() < 0.5, "{flipped}");
    }

    #[test]
    fn three_fingers_swipe_with_a_beginning_and_an_end() {
        let mut frames: Vec<(bool, Vec<Touch>)> =
            (0..=20).map(|i| (false, vec![touch(1, 30.0 + i as f32, 30.0), touch(2, 50.0 + i as f32, 30.0), touch(3, 70.0 + i as f32, 30.0)])).collect();
        frames.push((false, vec![]));
        let events = run(&mut Gestures::new(DEFAULT), &frames);
        let phases: Vec<Phase> = events.iter().filter_map(|e| if let Event::Swipe { phase, fingers: 3, .. } = e { Some(*phase) } else { None }).collect();
        assert_eq!(phases.first(), Some(&Phase::Begin));
        assert_eq!(phases.last(), Some(&Phase::End));
        assert!(phases.iter().filter(|p| **p == Phase::Update).count() > 10);
        assert!(clicks(&events).is_empty() && total(&events) == (0.0, 0.0));
    }

    #[test]
    fn a_palm_does_nothing_and_a_landing_finger_does_not_make_the_pointer_jump() {
        let palm: Vec<(bool, Vec<Touch>)> =
            (0..10).map(|i| (false, vec![Touch { id: 4, x: 80.0 + i as f32, y: 60.0, palm: true }])).chain([(false, vec![])]).collect();
        assert!(run(&mut Gestures::new(DEFAULT), &palm).is_empty());
        let landing = [(false, vec![touch(1, 10.0, 10.0)]), (false, vec![touch(1, 11.0, 10.0)]), (false, vec![touch(1, 11.0, 10.0), touch(2, 90.0, 60.0)])];
        let (dx, dy) = total(&run(&mut Gestures::new(DEFAULT), &landing));
        assert!(dx < 10.0 && dy.abs() < 0.01, "{dx} {dy}");
    }

    #[test]
    fn square_roots_are_close_enough_without_a_math_library() {
        for (dx, dy, expected) in [(3.0f32, 4.0f32, 5.0f32), (0.3, 0.4, 0.5), (0.0, 0.0, 0.0), (100.0, 0.0, 100.0), (0.001, 0.0, 0.001)] {
            assert!((distance(dx, dy) - expected).abs() < expected * 1e-4 + 1e-6, "{dx} {dy}");
        }
    }
}
