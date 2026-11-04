//! Reads the controller through Apple's GameController framework and turns its
//! state into logical button presses and releases.
//!
//! macOS hands Xbox controllers to its own driver, so raw HID readers (gilrs)
//! see the controller connect but never get its input. GameController is the
//! supported path, and `shouldMonitorBackgroundEvents` lets this background
//! program read it while Herdr is in front.
//!
//! Triggers and stick flicks become ordinary buttons with hysteresis, so the
//! rest of the program only ever sees `Press(btn)` / `Release(btn)`.

use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
use objc2_game_controller::{GCController, GCControllerButtonInput, GCDevice, GCExtendedGamepad};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Btn {
    A, B, X, Y,
    Lb, Rb, Lt, Rt,
    View, Menu, Guide, Share,
    L3, R3,
    Up, Down, Left, Right,
    LsUp, LsDown, LsLeft, LsRight,
    RsUp, RsDown, RsLeft, RsRight,
}

impl Btn {
    pub const ALL: [Btn; 26] = [
        Btn::A, Btn::B, Btn::X, Btn::Y, Btn::Lb, Btn::Rb, Btn::Lt, Btn::Rt,
        Btn::View, Btn::Menu, Btn::Guide, Btn::Share, Btn::L3, Btn::R3,
        Btn::Up, Btn::Down, Btn::Left, Btn::Right,
        Btn::LsUp, Btn::LsDown, Btn::LsLeft, Btn::LsRight,
        Btn::RsUp, Btn::RsDown, Btn::RsLeft, Btn::RsRight,
    ];

    /// The name used in the config file and the `sim` command.
    pub fn name(self) -> &'static str {
        match self {
            Btn::A => "a", Btn::B => "b", Btn::X => "x", Btn::Y => "y",
            Btn::Lb => "lb", Btn::Rb => "rb", Btn::Lt => "lt", Btn::Rt => "rt",
            Btn::View => "view", Btn::Menu => "menu", Btn::Guide => "guide", Btn::Share => "share",
            Btn::L3 => "l3", Btn::R3 => "r3",
            Btn::Up => "dpad_up", Btn::Down => "dpad_down", Btn::Left => "dpad_left", Btn::Right => "dpad_right",
            Btn::LsUp => "ls_up", Btn::LsDown => "ls_down", Btn::LsLeft => "ls_left", Btn::LsRight => "ls_right",
            Btn::RsUp => "rs_up", Btn::RsDown => "rs_down", Btn::RsLeft => "rs_left", Btn::RsRight => "rs_right",
        }
    }

    /// How the button is printed on an Xbox controller, for the on-screen legend.
    pub fn glyph(self) -> &'static str {
        match self {
            Btn::A => "A", Btn::B => "B", Btn::X => "X", Btn::Y => "Y",
            Btn::Lb => "LB", Btn::Rb => "RB", Btn::Lt => "LT", Btn::Rt => "RT",
            Btn::View => "View", Btn::Menu => "Menu", Btn::Guide => "Xbox", Btn::Share => "Share",
            Btn::L3 => "L3", Btn::R3 => "R3",
            Btn::Up => "D-pad ↑", Btn::Down => "D-pad ↓", Btn::Left => "D-pad ←", Btn::Right => "D-pad →",
            Btn::LsUp => "L-stick ↑", Btn::LsDown => "L-stick ↓", Btn::LsLeft => "L-stick ←", Btn::LsRight => "L-stick →",
            Btn::RsUp => "R-stick ↑", Btn::RsDown => "R-stick ↓", Btn::RsLeft => "R-stick ←", Btn::RsRight => "R-stick →",
        }
    }

    pub fn from_name(s: &str) -> Option<Btn> {
        Btn::ALL.into_iter().find(|b| b.name() == s)
    }

    pub fn is_dpad(self) -> bool {
        matches!(self, Btn::Up | Btn::Down | Btn::Left | Btn::Right)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Press(Btn),
    Release(Btn),
    Connected(String),
    Disconnected,
    /// Every gilrs event as-is (`herdr-pad test --raw`), for diagnosing a controller.
    Raw(String),
}

// Stick flicks: a flick counts past 0.65 and must fall back under 0.3 before
// the next one; diagonals within ±10° of 45° are ignored (UX review).
const FLICK_ON: f32 = 0.65;
const FLICK_OFF: f32 = 0.30;
const DIAGONAL_RATIO: f32 = 0.84; // tan(40°): minor/major above this is "diagonal"
// Analog triggers count as pressed past 50 %, released under 30 %.
const TRIGGER_ON: f32 = 0.50;
const TRIGGER_OFF: f32 = 0.30;

#[derive(Default)]
struct Stick {
    x: f32,
    y: f32,
    dir: Option<Btn>,
}

impl Stick {
    /// Feeds the new position; returns (released, pressed) direction changes.
    fn update(&mut self, dirs: [Btn; 4]) -> (Option<Btn>, Option<Btn>) {
        let [up, down, left, right] = dirs;
        let mag = (self.x * self.x + self.y * self.y).sqrt();
        match self.dir {
            Some(d) if mag < FLICK_OFF => {
                self.dir = None;
                (Some(d), None)
            }
            Some(_) => (None, None), // no re-aiming mid-flick: return to center first
            None if mag > FLICK_ON => {
                let (ax, ay) = (self.x.abs(), self.y.abs());
                let (major, minor) = if ax > ay { (ax, ay) } else { (ay, ax) };
                if minor / major > DIAGONAL_RATIO {
                    return (None, None);
                }
                let d = if ax > ay {
                    if self.x > 0.0 { right } else { left }
                } else if self.y > 0.0 {
                    up
                } else {
                    down
                };
                self.dir = Some(d);
                (None, Some(d))
            }
            None => (None, None),
        }
    }
}

/// One reading of everything on the controller.
#[derive(Default, Clone)]
struct Snapshot {
    buttons: HashMap<Btn, bool>,
    lx: f32,
    ly: f32,
    rx: f32,
    ry: f32,
    lt: f32,
    rt: f32,
}

pub struct Pad {
    connected: Option<String>,
    last: Snapshot,
    left: Stick,
    right: Stick,
    lt: bool,
    rt: bool,
    pending: Vec<Input>,
    pub raw: bool,
}

impl Pad {
    pub fn new() -> anyhow::Result<Pad> {
        // Keep reading while another app (Herdr) is in front.
        unsafe { GCController::setShouldMonitorBackgroundEvents(true) };
        Ok(Pad { connected: None, last: Snapshot::default(), left: Stick::default(), right: Stick::default(), lt: false, rt: false, pending: vec![], raw: false })
    }

    /// Lets the framework run for `timeout`, then reports what changed.
    /// Must be called on the main thread: GameController delivers there.
    pub fn poll(&mut self, timeout: Duration) -> Vec<Input> {
        CFRunLoop::run_in_mode(unsafe { kCFRunLoopDefaultMode }, timeout.as_secs_f64(), false);
        let pad = first_gamepad();
        match (&self.connected, pad) {
            (None, Some((name, g))) => {
                self.connected = Some(name.clone());
                self.pending.push(Input::Connected(name));
                // Don't fire presses for buttons already held when it connected.
                self.last = read(&g);
            }
            (Some(_), None) => {
                self.connected = None;
                self.reset();
                self.pending.push(Input::Disconnected);
            }
            (Some(_), Some((_, g))) => {
                let now = read(&g);
                self.diff(now);
            }
            (None, None) => {}
        }
        std::mem::take(&mut self.pending)
    }

    fn reset(&mut self) {
        self.last = Snapshot::default();
        self.left = Stick::default();
        self.right = Stick::default();
        self.lt = false;
        self.rt = false;
    }

    fn diff(&mut self, now: Snapshot) {
        for (b, &down) in &now.buttons {
            let was = self.last.buttons.get(b).copied().unwrap_or(false);
            if down != was {
                if self.raw {
                    self.pending.push(Input::Raw(format!("{} {}", b.name(), if down { "down" } else { "up" })));
                }
                self.pending.push(if down { Input::Press(*b) } else { Input::Release(*b) });
            }
        }
        if self.raw {
            for (name, a, b) in [("left stick", (now.lx, now.ly), (self.last.lx, self.last.ly)), ("right stick", (now.rx, now.ry), (self.last.rx, self.last.ry))] {
                if (a.0 - b.0).abs() > 0.2 || (a.1 - b.1).abs() > 0.2 {
                    self.pending.push(Input::Raw(format!("{name} x={:.2} y={:.2}", a.0, a.1)));
                }
            }
            for (name, a, b) in [("lt", now.lt, self.last.lt), ("rt", now.rt, self.last.rt)] {
                if (a - b).abs() > 0.2 {
                    self.pending.push(Input::Raw(format!("{name} {a:.2}")));
                }
            }
        }
        self.left.x = now.lx;
        self.left.y = now.ly;
        let (released, pressed) = self.left.update(LEFT_DIRS);
        self.push_dir(released, pressed);
        self.right.x = now.rx;
        self.right.y = now.ry;
        let (released, pressed) = self.right.update(RIGHT_DIRS);
        self.push_dir(released, pressed);
        self.trigger(Btn::Lt, now.lt);
        self.trigger(Btn::Rt, now.rt);
        // Only keep raw-diff baselines for sticks/triggers when something moved enough to print.
        self.last = now;
    }

    fn push_dir(&mut self, released: Option<Btn>, pressed: Option<Btn>) {
        if let Some(b) = released { self.pending.push(Input::Release(b)); }
        if let Some(b) = pressed { self.pending.push(Input::Press(b)); }
    }

    fn trigger(&mut self, btn: Btn, v: f32) {
        let held = if btn == Btn::Lt { &mut self.lt } else { &mut self.rt };
        if !*held && v > TRIGGER_ON {
            *held = true;
            self.pending.push(Input::Press(btn));
        } else if *held && v < TRIGGER_OFF {
            *held = false;
            self.pending.push(Input::Release(btn));
        }
    }
}

const LEFT_DIRS: [Btn; 4] = [Btn::LsUp, Btn::LsDown, Btn::LsLeft, Btn::LsRight];
const RIGHT_DIRS: [Btn; 4] = [Btn::RsUp, Btn::RsDown, Btn::RsLeft, Btn::RsRight];

fn first_gamepad() -> Option<(String, objc2::rc::Retained<GCExtendedGamepad>)> {
    let list = unsafe { GCController::controllers() };
    list.iter().find_map(|c| {
        let g = unsafe { c.extendedGamepad() }?;
        let name = unsafe { c.vendorName() }.map(|n| n.to_string()).unwrap_or_else(|| "Controller".into());
        Some((name, g))
    })
}

fn read(g: &GCExtendedGamepad) -> Snapshot {
    let pressed = |b: &GCControllerButtonInput| unsafe { b.isPressed() };
    let opt = |b: Option<objc2::rc::Retained<GCControllerButtonInput>>| b.map(|b| pressed(&b)).unwrap_or(false);
    unsafe {
        let dpad = g.dpad();
        let ls = g.leftThumbstick();
        let rs = g.rightThumbstick();
        let buttons = HashMap::from([
            (Btn::A, pressed(&g.buttonA())),
            (Btn::B, pressed(&g.buttonB())),
            (Btn::X, pressed(&g.buttonX())),
            (Btn::Y, pressed(&g.buttonY())),
            (Btn::Lb, pressed(&g.leftShoulder())),
            (Btn::Rb, pressed(&g.rightShoulder())),
            (Btn::Menu, pressed(&g.buttonMenu())),
            (Btn::View, opt(g.buttonOptions())),
            (Btn::Guide, opt(g.buttonHome())),
            (Btn::L3, opt(g.leftThumbstickButton())),
            (Btn::R3, opt(g.rightThumbstickButton())),
            (Btn::Up, pressed(&dpad.up())),
            (Btn::Down, pressed(&dpad.down())),
            (Btn::Left, pressed(&dpad.left())),
            (Btn::Right, pressed(&dpad.right())),
        ]);
        Snapshot {
            buttons,
            lx: ls.xAxis().value(),
            ly: ls.yAxis().value(),
            rx: rs.xAxis().value(),
            ry: rs.yAxis().value(),
            lt: g.leftTrigger().value(),
            rt: g.rightTrigger().value(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flick(s: &mut Stick, x: f32, y: f32) -> (Option<Btn>, Option<Btn>) {
        s.x = x;
        s.y = y;
        s.update(LEFT_DIRS)
    }

    #[test]
    fn stick_flick_needs_return_to_center() {
        let mut s = Stick::default();
        assert_eq!(flick(&mut s, 0.5, 0.0), (None, None)); // under threshold
        assert_eq!(flick(&mut s, 0.9, 0.0), (None, Some(Btn::LsRight)));
        assert_eq!(flick(&mut s, 0.0, 0.9), (None, None)); // re-aim ignored
        assert_eq!(flick(&mut s, 0.1, 0.1), (Some(Btn::LsRight), None));
        assert_eq!(flick(&mut s, 0.0, -0.8), (None, Some(Btn::LsDown)));
    }

    #[test]
    fn stick_ignores_diagonals() {
        let mut s = Stick::default();
        assert_eq!(flick(&mut s, 0.6, 0.6), (None, None));
        assert_eq!(flick(&mut s, 0.8, 0.3), (None, Some(Btn::LsRight)));
    }

    #[test]
    fn names_round_trip() {
        for b in Btn::ALL {
            assert_eq!(Btn::from_name(b.name()), Some(b));
        }
    }
}
