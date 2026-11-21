#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Expect {
    Travel(f32),
    Taps(usize),
    Presses(usize),
    Drag,
    Fingers { count: usize, frames: usize },
    MultiTaps(usize),
    Palm(usize),
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scenario {
    pub name: &'static str,
    pub title: &'static str,
    pub how: &'static str,
    pub hint: &'static str,
    pub seconds: u64,
    pub expect: Expect,
}

pub const SCENARIOS: [Scenario; 14] = [
    Scenario { name: "move", title: "session-move", how: "session-move-how", hint: "session-move-hint", seconds: 20, expect: Expect::Travel(150.0) },
    Scenario { name: "tap", title: "session-tap", how: "session-tap-how", hint: "session-tap-hint", seconds: 20, expect: Expect::Taps(5) },
    Scenario { name: "click", title: "session-click", how: "session-click-how", hint: "session-click-hint", seconds: 20, expect: Expect::Presses(4) },
    Scenario { name: "drag", title: "session-drag", how: "session-drag-how", hint: "session-drag-hint", seconds: 25, expect: Expect::Drag },
    Scenario {
        name: "scroll",
        title: "session-scroll",
        how: "session-scroll-how",
        hint: "session-scroll-hint",
        seconds: 20,
        expect: Expect::Fingers { count: 2, frames: 60 },
    },
    Scenario { name: "tap2", title: "session-tap2", how: "session-tap2-how", hint: "session-tap2-hint", seconds: 20, expect: Expect::MultiTaps(3) },
    Scenario {
        name: "swipe",
        title: "session-swipe",
        how: "session-swipe-how",
        hint: "session-swipe-hint",
        seconds: 25,
        expect: Expect::Fingers { count: 3, frames: 40 },
    },
    Scenario {
        name: "thumb",
        title: "session-thumb",
        how: "session-thumb-how",
        hint: "session-thumb-hint",
        seconds: 20,
        expect: Expect::Fingers { count: 2, frames: 60 },
    },
    Scenario { name: "palm", title: "session-palm", how: "session-palm-how", hint: "session-palm-hint", seconds: 30, expect: Expect::Nothing },
    Scenario { name: "cover", title: "session-cover", how: "session-cover-how", hint: "session-cover-hint", seconds: 20, expect: Expect::Palm(40) },
    Scenario {
        name: "rotate",
        title: "session-rotate",
        how: "session-rotate-how",
        hint: "session-rotate-hint",
        seconds: 20,
        expect: Expect::Fingers { count: 2, frames: 80 },
    },
    Scenario {
        name: "pinch",
        title: "session-pinch",
        how: "session-pinch-how",
        hint: "session-pinch-hint",
        seconds: 20,
        expect: Expect::Fingers { count: 2, frames: 80 },
    },
    Scenario {
        name: "rails",
        title: "session-rails",
        how: "session-rails-how",
        hint: "session-rails-hint",
        seconds: 25,
        expect: Expect::Fingers { count: 2, frames: 80 },
    },
    Scenario {
        name: "symbols",
        title: "session-symbols",
        how: "session-symbols-how",
        hint: "session-symbols-hint",
        seconds: 30,
        expect: Expect::Travel(300.0),
    },
];

pub const MAX_FINGERS: usize = 5;
const TAP_TIME: u64 = 300_000;
const TAP_TRAVEL: f32 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub id: u8,
    pub x: f32,
    pub y: f32,
    pub palm: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub frames: usize,
    pub most: usize,
    pub presses: usize,
    pub taps: usize,
    pub multi_taps: usize,
    pub travel: f32,
    pub drag_travel: f32,
    pub with: [usize; MAX_FINGERS + 1],
    pub palm_frames: usize,
    pressed: bool,
    down_since: Option<u64>,
    touch_most: usize,
    touch_travel: f32,
    touch_pressed: bool,
    last: Vec<Point>,
}

impl Stats {
    pub fn add(&mut self, time: u64, button: bool, points: &[Point]) {
        self.frames += 1;
        let fingers: Vec<Point> = points.iter().copied().filter(|p| !p.palm).collect();
        let count = fingers.len().min(MAX_FINGERS);
        self.with[count] += 1;
        self.most = self.most.max(count);
        if points.iter().any(|p| p.palm) {
            self.palm_frames += 1;
        }
        if button && !self.pressed {
            self.presses += 1;
            self.touch_pressed = true;
        }
        self.pressed = button;
        let moved: f32 = fingers
            .iter()
            .filter_map(|p| self.last.iter().find(|l| l.id == p.id).map(|l| ((p.x - l.x).powi(2) + (p.y - l.y).powi(2)).sqrt()))
            .fold(0.0, f32::max);
        if count == 1 {
            self.travel += moved;
        }
        if button {
            self.drag_travel += moved;
        }
        match (count, self.down_since) {
            (0, Some(since)) => {
                if time.saturating_sub(since) <= TAP_TIME && self.touch_travel <= TAP_TRAVEL && !self.touch_pressed {
                    if self.touch_most == 1 {
                        self.taps += 1;
                    } else {
                        self.multi_taps += 1;
                    }
                }
                self.down_since = None;
            }
            (0, None) => {}
            (_, None) => {
                self.down_since = Some(time);
                self.touch_most = count;
                self.touch_travel = 0.0;
                self.touch_pressed = button;
            }
            (_, Some(_)) => {
                self.touch_most = self.touch_most.max(count);
                self.touch_travel += moved;
            }
        }
        self.last = fingers;
    }

    pub fn done(&self, expect: Expect) -> bool {
        match expect {
            Expect::Travel(millimetres) => self.travel >= millimetres,
            Expect::Taps(count) => self.taps >= count,
            Expect::Presses(count) => self.presses >= count,
            Expect::Drag => self.presses >= 2 && self.drag_travel >= 20.0,
            Expect::Fingers { count, frames } => self.with[count.min(MAX_FINGERS)..].iter().sum::<usize>() >= frames,
            Expect::MultiTaps(count) => self.multi_taps >= count,
            Expect::Palm(frames) => self.palm_frames >= frames,
            Expect::Nothing => true,
        }
    }
}

pub fn draw(columns: usize, rows: usize, size: (f32, f32), points: &[Point], button: bool) -> Vec<String> {
    let mut grid = vec![vec![' '; columns]; rows];
    for (index, point) in points.iter().enumerate() {
        let column = ((point.x / size.0).clamp(0.0, 0.999) * columns as f32) as usize;
        let row = ((point.y / size.1).clamp(0.0, 0.999) * rows as f32) as usize;
        grid[row][column] = if point.palm { 'x' } else { char::from_digit((index as u32 + 1).min(9), 10).unwrap_or('*') };
    }
    let edge = |fill: char| format!("+{}+", fill.to_string().repeat(columns));
    let mut lines = Vec::with_capacity(rows + 2);
    lines.push(edge('-'));
    lines.extend(grid.into_iter().map(|row| format!("|{}|", row.into_iter().collect::<String>())));
    lines.push(edge(if button { '=' } else { '-' }));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(id: u8, x: f32, y: f32) -> Point {
        Point { id, x, y, palm: false }
    }

    #[test]
    fn short_still_touches_count_as_taps_and_long_or_pressed_ones_do_not() {
        let mut stats = Stats::default();
        let mut time = 0;
        let mut feed = |stats: &mut Stats, button: bool, points: &[Point], step: u64| {
            time += step;
            stats.add(time, button, points);
        };
        feed(&mut stats, false, &[at(1, 10.0, 10.0)], 8_000);
        feed(&mut stats, false, &[at(1, 10.5, 10.0)], 8_000);
        feed(&mut stats, false, &[], 8_000);
        feed(&mut stats, false, &[at(2, 10.0, 10.0), at(3, 30.0, 10.0)], 8_000);
        feed(&mut stats, false, &[], 8_000);
        feed(&mut stats, false, &[at(4, 10.0, 10.0)], 8_000);
        feed(&mut stats, false, &[], 900_000);
        feed(&mut stats, false, &[at(5, 10.0, 10.0)], 8_000);
        feed(&mut stats, true, &[at(5, 10.0, 10.0)], 8_000);
        feed(&mut stats, false, &[], 8_000);
        assert_eq!((stats.taps, stats.multi_taps, stats.presses, stats.most), (1, 1, 1, 2));
        assert!(stats.done(Expect::Taps(1)) && !stats.done(Expect::Taps(2)));
        assert!(stats.done(Expect::MultiTaps(1)) && stats.done(Expect::Presses(1)));
    }

    #[test]
    fn travel_counts_one_finger_and_dragging_counts_what_moves_while_pressed() {
        let mut stats = Stats::default();
        for i in 0..11 {
            stats.add(i * 8_000, i >= 5, &[at(1, i as f32 * 3.0, 0.0)]);
        }
        assert!((stats.travel - 30.0).abs() < 1e-3 && (stats.drag_travel - 18.0).abs() < 1e-3);
        assert!(stats.done(Expect::Travel(30.0)) && !stats.done(Expect::Travel(31.0)));
        for _ in 0..3 {
            stats.add(100_000, false, &[at(1, 0.0, 0.0), at(2, 9.0, 9.0), at(3, 5.0, 5.0)]);
        }
        assert!(stats.done(Expect::Fingers { count: 3, frames: 3 }) && stats.done(Expect::Fingers { count: 2, frames: 3 }));
        assert!(!stats.done(Expect::Fingers { count: 3, frames: 4 }));
        assert!(!stats.done(Expect::Palm(1)));
        stats.add(110_000, false, &[Point { id: 4, x: 50.0, y: 40.0, palm: true }]);
        assert!(stats.done(Expect::Palm(1)) && !stats.done(Expect::Palm(2)));
    }

    #[test]
    fn the_pad_is_drawn_with_fingers_palms_and_a_pressed_edge() {
        let picture = draw(10, 3, (100.0, 30.0), &[at(1, 0.0, 0.0), Point { id: 2, x: 99.0, y: 29.0, palm: true }], true);
        assert_eq!(picture, ["+----------+", "|1         |", "|          |", "|         x|", "+==========+"]);
    }

    #[test]
    fn every_scenario_has_its_own_name_and_messages() {
        let names: std::collections::BTreeSet<&str> = SCENARIOS.iter().map(|s| s.name).collect();
        assert_eq!(names.len(), SCENARIOS.len());
        for scenario in SCENARIOS {
            assert!(scenario.title.starts_with("session-") && scenario.how.ends_with("-how") && scenario.hint.ends_with("-hint"));
        }
    }
}
