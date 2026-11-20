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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surface {
    pub width: f32,
    pub height: f32,
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
    pub multi_tap_time: u64,
    pub tap_travel: f32,
    pub scroll_speed: f32,
    pub scroll_start: f32,
    pub natural_scroll: bool,
    pub swipe_start: f32,
    pub gap: u64,
    pub thumb_zone: f32,
    pub rest_time: u64,
    pub rest_travel: f32,
    pub edge: f32,
    pub edge_rest_time: u64,
    pub side_edge: f32,
    pub bottom_edge: f32,
    pub edge_release: f32,
    pub follow: f32,
}

pub const DEFAULT: Settings = Settings {
    speed: 4.0,
    acceleration: 3.0,
    slow: 30.0,
    fast: 300.0,
    tap: true,
    tap_time: 220_000,
    multi_tap_time: 360_000,
    tap_travel: 2.5,
    scroll_speed: 6.0,
    scroll_start: 2.0,
    natural_scroll: false,
    swipe_start: 5.0,
    gap: 100_000,
    thumb_zone: 0.75,
    rest_time: 300_000,
    rest_travel: 3.0,
    edge: 4.0,
    edge_rest_time: 500_000,
    side_edge: 3.0,
    bottom_edge: 3.0,
    edge_release: 8.0,
    follow: 0.45,
};

#[derive(Debug, Clone, Copy, PartialEq)]
struct Finger {
    id: u8,
    x: f32,
    y: f32,
    x0: f32,
    y0: f32,
    landed: u64,
    travelled: f32,
    palm: bool,
    edge: bool,
}

impl Finger {
    fn displacement(&self) -> f32 {
        distance(self.x - self.x0, self.y - self.y0)
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    Button(Button),
    Ignored,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gestures {
    settings: Settings,
    surface: Surface,
    fingers: Vec<Finger>,
    working: Vec<u8>,
    mode: Mode,
    pending: (f32, f32),
    split: [(f32, f32); 2],
    held: Option<Held>,
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
    pub fn new(settings: Settings, surface: Surface) -> Self {
        Gestures {
            settings,
            surface,
            fingers: Vec::new(),
            working: Vec::new(),
            mode: Mode::Idle,
            pending: (0.0, 0.0),
            split: [(0.0, 0.0); 2],
            held: None,
            sequence: None,
            last: None,
        }
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

    fn resting(&self, finger: &Finger, time: u64) -> bool {
        finger.displacement() < self.settings.rest_travel && time.saturating_sub(finger.landed) >= self.settings.rest_time
    }

    fn thumb(&self, finger: &Finger, time: u64, others: usize) -> bool {
        others > 0 && finger.y >= self.surface.height * self.settings.thumb_zone && self.resting(finger, time)
    }

    fn on_edge(&self, finger: &Finger, time: u64) -> bool {
        finger.y >= self.surface.height - self.settings.edge
            && finger.displacement() < self.settings.rest_travel
            && time.saturating_sub(finger.landed) >= self.settings.edge_rest_time
    }

    fn end_mode(&mut self, events: &mut Vec<Event>) {
        if let Mode::Swipe(fingers) = self.mode {
            events.push(Event::Swipe { fingers, phase: Phase::End, dx: 0.0, dy: 0.0 });
        }
        self.mode = Mode::Idle;
        self.pending = (0.0, 0.0);
        self.split = [(0.0, 0.0); 2];
    }

    fn at_edge(&self, x: f32, y: f32) -> bool {
        let s = &self.settings;
        x <= s.side_edge || x >= self.surface.width - s.side_edge || y >= self.surface.height - s.bottom_edge
    }

    fn point(&self, (dx, dy): (f32, f32), elapsed: u64, events: &mut Vec<Event>) {
        let gain = self.gain(distance(dx, dy), elapsed);
        if dx != 0.0 || dy != 0.0 {
            events.push(Event::Motion { dx: dx * gain, dy: dy * gain });
        }
    }

    fn scroll(&self, (dx, dy): (f32, f32), events: &mut Vec<Event>) {
        let sign = if self.settings.natural_scroll { -1.0 } else { 1.0 };
        if dx != 0.0 || dy != 0.0 {
            events.push(Event::Scroll { dx: sign * dx * self.settings.scroll_speed, dy: sign * dy * self.settings.scroll_speed });
        }
    }

    pub fn feed(&mut self, input: &Input) -> Vec<Event> {
        let mut events = Vec::new();
        let time = input.time;
        let elapsed = self.last.map_or(0, |last| time.saturating_sub(last));
        self.last = Some(time);

        let mut next: Vec<Finger> = Vec::with_capacity(input.touches.len());
        let mut moves: Vec<(u8, f32, f32)> = Vec::with_capacity(input.touches.len());
        for touch in &input.touches {
            let finger = match self.fingers.iter().find(|finger| finger.id == touch.id) {
                Some(previous) => {
                    let (dx, dy) = (touch.x - previous.x, touch.y - previous.y);
                    moves.push((touch.id, dx, dy));
                    let mut moved =
                        Finger { x: touch.x, y: touch.y, travelled: previous.travelled + distance(dx, dy), palm: previous.palm || touch.palm, ..*previous };
                    moved.edge &= moved.displacement() < self.settings.edge_release;
                    moved
                }
                None => Finger {
                    id: touch.id,
                    x: touch.x,
                    y: touch.y,
                    x0: touch.x,
                    y0: touch.y,
                    landed: time,
                    travelled: 0.0,
                    palm: touch.palm,
                    edge: self.at_edge(touch.x, touch.y),
                },
            };
            next.push(finger);
        }
        self.fingers = next;

        let fingers: Vec<Finger> = self.fingers.iter().copied().filter(|finger| !finger.palm).collect();
        let candidates: Vec<Finger> = fingers.iter().copied().filter(|finger| !finger.edge).collect();
        let working: Vec<Finger> = candidates.iter().copied().filter(|finger| !self.thumb(finger, time, candidates.len() - 1)).collect();
        let ids: Vec<u8> = working.iter().map(|finger| finger.id).collect();
        let same_fingers = ids == self.working;
        self.working = ids;

        if !fingers.is_empty() {
            let sequence = self.sequence.get_or_insert(Sequence { started: time, most: 0, travelled: 0.0, clicked: false });
            sequence.most = sequence.most.max(working.len().max(1) as u8);
            sequence.travelled = working.iter().fold(sequence.travelled, |most, finger| most.max(finger.travelled));
        }

        match (input.button, self.held) {
            (true, None) => {
                let ignored = fingers.is_empty() || fingers.iter().all(|finger| self.on_edge(finger, time));
                if let Some(sequence) = self.sequence.as_mut() {
                    sequence.clicked = true;
                }
                if ignored {
                    self.held = Some(Held::Ignored);
                } else {
                    let button = by_fingers(working.len());
                    self.held = Some(Held::Button(button));
                    events.push(Event::Press(button));
                }
            }
            (false, Some(held)) => {
                self.held = None;
                if let Held::Button(button) = held {
                    events.push(Event::Release(button));
                }
            }
            _ => {}
        }

        if !same_fingers {
            self.end_mode(&mut events);
        }

        let working_moves: Vec<(f32, f32)> = moves.iter().filter(|(id, _, _)| self.working.contains(id)).map(|(_, dx, dy)| (*dx, *dy)).collect();
        let pressed = matches!(self.held, Some(Held::Button(_)));
        if same_fingers && !working_moves.is_empty() {
            let count = working.len();
            let n = working_moves.len() as f32;
            let mean = (working_moves.iter().map(|m| m.0).sum::<f32>() / n, working_moves.iter().map(|m| m.1).sum::<f32>() / n);
            let largest = |list: &[(f32, f32)]| {
                list.iter().copied().fold((0.0f32, 0.0f32), |best, m| if distance(m.0, m.1) > distance(best.0, best.1) { m } else { best })
            };
            if pressed || count == 1 {
                self.mode = Mode::Pointer;
                self.point(largest(&working_moves), elapsed, &mut events);
            } else if count == 2 {
                let pair: Vec<(f32, f32)> =
                    working.iter().map(|finger| moves.iter().find(|(id, _, _)| *id == finger.id).map_or((0.0, 0.0), |(_, dx, dy)| (*dx, *dy))).collect();
                match self.mode {
                    Mode::Scroll => self.scroll(mean, &mut events),
                    Mode::Pointer => self.point(largest(&pair), elapsed, &mut events),
                    _ => {
                        for (sum, step) in self.split.iter_mut().zip(&pair) {
                            *sum = (sum.0 + step.0, sum.1 + step.1);
                        }
                        let [a, b] = self.split;
                        let (first, second) = (distance(a.0, a.1), distance(b.0, b.1));
                        let most = first.max(second);
                        if most >= self.settings.scroll_start {
                            if first.min(second) >= self.settings.follow && a.0 * b.0 + a.1 * b.1 > 0.0 {
                                self.mode = Mode::Scroll;
                                self.scroll(((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0), &mut events);
                            } else {
                                self.mode = Mode::Pointer;
                                self.point(largest(&self.split), elapsed, &mut events);
                            }
                        }
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

        if fingers.is_empty() {
            self.end_mode(&mut events);
            if let Some(sequence) = self.sequence.take() {
                let limit = if sequence.most > 1 { self.settings.multi_tap_time } else { self.settings.tap_time };
                let quick = time.saturating_sub(sequence.started) <= limit;
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
    const PAD: Surface = Surface { width: 117.0, height: 72.0 };

    fn touch(id: u8, x: f32, y: f32) -> Touch {
        Touch { id, x, y, palm: false }
    }

    fn palm(id: u8, x: f32, y: f32) -> Touch {
        Touch { id, x, y, palm: true }
    }

    fn gestures() -> Gestures {
        Gestures::new(DEFAULT, PAD)
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

    fn scrolled(events: &[Event]) -> (f32, f32) {
        events.iter().fold((0.0, 0.0), |sum, event| match event {
            Event::Scroll { dx, dy } => (sum.0 + dx, sum.1 + dy),
            _ => sum,
        })
    }

    fn clicks(events: &[Event]) -> Vec<Event> {
        events.iter().copied().filter(|e| matches!(e, Event::Press(_) | Event::Release(_))).collect()
    }

    fn still(frames: usize, touches: Vec<Touch>) -> Vec<(bool, Vec<Touch>)> {
        (0..frames).map(|_| (false, touches.clone())).collect()
    }

    #[test]
    fn one_finger_moves_the_pointer_and_faster_moves_go_further() {
        let mut frames: Vec<(bool, Vec<Touch>)> = slide(1, (10.0, 10.0), (20.0, 10.0), 100).into_iter().map(|t| (false, t)).collect();
        frames.push((false, vec![]));
        let slow_events = run(&mut gestures(), &frames);
        let (dx, dy) = total(&slow_events);
        assert!((dx - 40.0).abs() < 0.5 && dy.abs() < 0.01, "{dx} {dy}");
        assert!(clicks(&slow_events).is_empty());
        let mut quick: Vec<(bool, Vec<Touch>)> = slide(1, (10.0, 10.0), (20.0, 10.0), 4).into_iter().map(|t| (false, t)).collect();
        quick.push((false, vec![]));
        let (fast_dx, _) = total(&run(&mut gestures(), &quick));
        assert!(fast_dx > 100.0, "{fast_dx}");
    }

    #[test]
    fn a_quick_still_touch_is_a_click_and_a_long_one_is_not() {
        let tap = [(false, vec![touch(1, 30.0, 30.0)]), (false, vec![touch(1, 30.2, 30.1)]), (false, vec![])];
        assert_eq!(clicks(&run(&mut gestures(), &tap)), [Event::Press(Button::Left), Event::Release(Button::Left)]);
        let mut held = still(40, vec![touch(2, 30.0, 30.0)]);
        held.push((false, vec![]));
        assert!(clicks(&run(&mut gestures(), &held)).is_empty());
        let mut off = DEFAULT;
        off.tap = false;
        assert!(clicks(&run(&mut Gestures::new(off, PAD), &tap)).is_empty());
    }

    #[test]
    fn several_fingers_may_tap_a_little_slower_than_one() {
        let mut three = still(35, vec![touch(1, 30.0, 30.0), touch(2, 50.0, 30.0), touch(3, 70.0, 30.0)]);
        three.push((false, vec![]));
        assert_eq!(clicks(&run(&mut gestures(), &three)), [Event::Press(Button::Middle), Event::Release(Button::Middle)]);
        let mut one = still(35, vec![touch(1, 30.0, 30.0)]);
        one.push((false, vec![]));
        assert!(clicks(&run(&mut gestures(), &one)).is_empty());
        let two = [(false, vec![touch(1, 30.0, 30.0), touch(2, 50.0, 30.0)]), (false, vec![touch(1, 30.0, 30.0), touch(2, 50.0, 30.0)]), (false, vec![])];
        assert_eq!(clicks(&run(&mut gestures(), &two)), [Event::Press(Button::Right), Event::Release(Button::Right)]);
    }

    #[test]
    fn pressing_the_pad_clicks_by_the_number_of_fingers_and_is_not_also_a_tap() {
        let one = [(false, vec![touch(1, 30.0, 60.0)]), (true, vec![touch(1, 30.0, 60.0)]), (false, vec![touch(1, 30.0, 60.0)]), (false, vec![])];
        assert_eq!(clicks(&run(&mut gestures(), &one)), [Event::Press(Button::Left), Event::Release(Button::Left)]);
        let two = [(true, vec![touch(1, 30.0, 40.0), touch(2, 50.0, 40.0)]), (false, vec![touch(1, 30.0, 40.0), touch(2, 50.0, 40.0)]), (false, vec![])];
        assert_eq!(clicks(&run(&mut gestures(), &two)), [Event::Press(Button::Right), Event::Release(Button::Right)]);
    }

    #[test]
    fn two_fingers_moving_together_scroll_and_do_not_move_the_pointer() {
        let frames: Vec<(bool, Vec<Touch>)> = (0..=30).map(|i| (false, vec![touch(1, 40.0, 20.0 + i as f32), touch(2, 60.0, 20.0 + i as f32)])).collect();
        let events = run(&mut gestures(), &frames);
        assert_eq!(total(&events), (0.0, 0.0));
        assert!((scrolled(&events).1 - 30.0 * DEFAULT.scroll_speed).abs() < 0.5, "{:?}", scrolled(&events));
        let mut natural = DEFAULT;
        natural.natural_scroll = true;
        let flipped = scrolled(&run(&mut Gestures::new(natural, PAD), &frames)).1;
        assert!((flipped + 30.0 * DEFAULT.scroll_speed).abs() < 0.5, "{flipped}");
    }

    #[test]
    fn three_fingers_swipe_with_a_beginning_and_an_end() {
        let mut frames: Vec<(bool, Vec<Touch>)> =
            (0..=20).map(|i| (false, vec![touch(1, 30.0 + i as f32, 30.0), touch(2, 50.0 + i as f32, 30.0), touch(3, 70.0 + i as f32, 30.0)])).collect();
        frames.push((false, vec![]));
        let events = run(&mut gestures(), &frames);
        let phases: Vec<Phase> = events.iter().filter_map(|e| if let Event::Swipe { phase, fingers: 3, .. } = e { Some(*phase) } else { None }).collect();
        assert_eq!(phases.first(), Some(&Phase::Begin));
        assert_eq!(phases.last(), Some(&Phase::End));
        assert!(phases.iter().filter(|p| **p == Phase::Update).count() > 10);
        assert!(clicks(&events).is_empty() && total(&events) == (0.0, 0.0));
    }

    #[test]
    fn a_resting_thumb_neither_turns_pointing_into_scrolling_nor_clicks_into_right_clicks() {
        let thumb = touch(9, 24.0, 62.0);
        let mut frames = still(45, vec![thumb]);
        frames.extend((0..=40).map(|i| (false, vec![thumb, touch(1, 30.0 + i as f32 * 0.5, 15.0)])));
        frames.push((true, vec![thumb, touch(1, 50.0, 15.0)]));
        frames.push((false, vec![thumb, touch(1, 50.0, 15.0)]));
        frames.push((false, vec![]));
        let events = run(&mut gestures(), &frames);
        assert_eq!(scrolled(&events), (0.0, 0.0));
        assert!(total(&events).0 > 50.0, "{:?}", total(&events));
        assert_eq!(clicks(&events), [Event::Press(Button::Left), Event::Release(Button::Left)]);
    }

    #[test]
    fn a_palm_never_points_nor_clicks_even_after_it_looks_like_a_finger_again() {
        let mut frames: Vec<(bool, Vec<Touch>)> = (0..10).map(|i| (false, vec![palm(4, 80.0 + i as f32, 36.0)])).collect();
        frames.extend((0..10).map(|i| (false, vec![touch(4, 90.0 + i as f32, 36.0)])));
        frames.push((true, vec![touch(4, 100.0, 36.0)]));
        frames.push((false, vec![touch(4, 100.0, 36.0)]));
        frames.push((false, vec![]));
        assert!(run(&mut gestures(), &frames).is_empty());
    }

    #[test]
    fn a_hand_resting_on_the_bottom_edge_does_not_click_when_it_presses_the_pad() {
        let resting = touch(1, 33.6, 71.8);
        let mut frames = still(80, vec![resting]);
        frames.push((true, vec![resting]));
        frames.push((false, vec![resting]));
        frames.push((false, vec![]));
        assert!(clicks(&run(&mut gestures(), &frames)).is_empty());
        let corner = touch(2, 116.0, 69.4);
        let quick = [(false, vec![corner]), (false, vec![corner]), (true, vec![corner]), (false, vec![corner]), (false, vec![])];
        assert_eq!(clicks(&run(&mut gestures(), &quick)), [Event::Press(Button::Left), Event::Release(Button::Left)]);
    }

    #[test]
    fn a_hand_resting_on_the_edges_does_not_point_until_it_comes_well_inside() {
        let edge_frames: Vec<(bool, Vec<Touch>)> = (0..30).map(|i| (false, vec![touch(5, 116.6, 24.0 + i as f32 * 0.1)])).chain([(false, vec![])]).collect();
        assert!(run(&mut gestures(), &edge_frames).is_empty());
        let bottom: Vec<(bool, Vec<Touch>)> = (0..30).map(|i| (false, vec![touch(6, 33.0 + i as f32 * 0.2, 71.5)])).chain([(false, vec![])]).collect();
        assert!(run(&mut gestures(), &bottom).is_empty());
        let inward: Vec<(bool, Vec<Touch>)> = (0..=40).map(|i| (false, vec![touch(7, 116.0 - i as f32 * 0.5, 30.0)])).collect();
        let (dx, _) = total(&run(&mut gestures(), &inward));
        assert!(dx < -20.0, "{dx}");
    }

    #[test]
    fn one_still_finger_and_one_moving_finger_point_instead_of_scrolling() {
        let frames: Vec<(bool, Vec<Touch>)> = (0..=30).map(|i| (false, vec![touch(1, 40.0, 30.0), touch(2, 70.0, 20.0 + i as f32 * 0.5)])).collect();
        let events = run(&mut gestures(), &frames);
        assert_eq!(scrolled(&events), (0.0, 0.0));
        assert!(total(&events).1 > 30.0, "{:?}", total(&events));
    }

    #[test]
    fn a_landing_finger_does_not_make_the_pointer_jump() {
        let landing = [(false, vec![touch(1, 10.0, 10.0)]), (false, vec![touch(1, 11.0, 10.0)]), (false, vec![touch(1, 11.0, 10.0), touch(2, 90.0, 40.0)])];
        let (dx, dy) = total(&run(&mut gestures(), &landing));
        assert!(dx < 10.0 && dy.abs() < 0.01, "{dx} {dy}");
    }

    #[test]
    fn square_roots_are_close_enough_without_a_math_library() {
        for (dx, dy, expected) in [(3.0f32, 4.0f32, 5.0f32), (0.3, 0.4, 0.5), (0.0, 0.0, 0.0), (100.0, 0.0, 100.0), (0.001, 0.0, 0.001)] {
            assert!((distance(dx, dy) - expected).abs() < expected * 1e-4 + 1e-6, "{dx} {dy}");
        }
    }
}
