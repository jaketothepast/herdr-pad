//! Button logic: what each press does, given what Herdr shows right now.
//!
//! The engine talks to Herdr only through `Host`, so tests drive it with a fake.

use crate::config::{Action, Binding, Config, MenuItem};
use crate::herdr::{Ls, Status};
use crate::input::{Btn, Input};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

pub trait Host {
    fn run(&mut self, name: &str, args: &Value) -> Result<Value>;
    fn status(&mut self) -> Result<Status>;
    fn ls(&mut self) -> Result<Ls>;
    fn keys(&mut self, terminal: &str, keys: &[String]) -> Result<()>;
    fn frontmost(&mut self) -> bool;
    fn exec(&mut self, argv: &[String]) -> Result<()>;
    /// Sends the on-screen view (None: hide it).
    fn hud(&mut self, view: Option<&Value>);
    /// Folders inside `root` (absolute paths), most recently changed first. Hidden ones left out.
    fn list_dirs(&mut self, root: &str) -> Vec<String>;
    fn is_dir(&mut self, path: &str) -> bool;
    /// Holds a keyboard key down (`down`) or lets it go: push-to-talk for dictation apps.
    fn hold_key(&mut self, key: &str, down: bool) -> Result<()>;
}

/// A folder the space picker offers.
#[derive(Debug, Clone)]
struct Folder {
    name: String,
    path: String,
    detail: String,
}

const PAGE: usize = 8;

// Erasing: one Backspace per flick; held, it repeats, then goes word by word.
const ERASE_DELAY_MS: u64 = 400;
const ERASE_CHAR_MS: u64 = 70;
const ERASE_WORDS_AFTER_MS: u64 = 1200;
const ERASE_WORD_MS: u64 = 200;
/// Lines "clear" walks up through (Claude Code's input box can hold several).
const CLEAR_LINES: usize = 20;

/// Overlays that B must not try to close (ours, and windows overlay.close can't close).
const OWN_OVERLAY: &str = crate::herdr::MOD_NAME;
const IGNORED_OVERLAYS: [&str; 2] = [OWN_OVERLAY, "frame-welcome"];

#[derive(Debug, Clone, Copy, PartialEq)]
enum HoldKind {
    Interrupt,
    Confirm,
    Clear,
}

#[derive(Debug)]
struct Hold {
    btn: Btn,
    kind: HoldKind,
    started: Instant,
    ms: u64,
    terminal: Option<String>,
}

#[derive(Debug)]
struct Repeat {
    btn: Btn,
    action: Action,
    label: String,
    pressed: Instant,
    next: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum Kind {
    #[default]
    Ok,
    Warn,
    Info,
}

pub struct Engine {
    pub cfg: Config,
    connected: Option<String>,
    /// Buttons pressed while Herdr wasn't in front: their releases are ignored too.
    swallowed: HashSet<Btn>,
    /// What a held button resolved to on press (so a layer change mid-hold is harmless).
    pressed: HashMap<Btn, (Binding, Instant)>,
    layer: Option<(Btn, String)>,
    menu: Option<usize>,
    /// New space…: the folders and which one is selected.
    picker: Option<(Vec<Folder>, usize)>,
    legend: Option<Instant>,
    hold: Option<Hold>,
    repeat: Option<Repeat>,
    dictating: bool,
    label: Option<(String, Kind)>,
    /// A second line under the label: what to press next.
    hint: Option<String>,
    seq: u64,
    prev_terminal: Option<String>,
    focus_moved: Option<Instant>,
    last_esc: Option<Instant>,
    /// Characters and words erased in the current hold of the erase input.
    erased: (u32, u32),
    dirty: bool,
}

impl Engine {
    pub fn new(cfg: Config) -> Engine {
        Engine {
            cfg,
            connected: None,
            swallowed: HashSet::new(),
            pressed: HashMap::new(),
            layer: None,
            menu: None,
            picker: None,
            legend: None,
            hold: None,
            repeat: None,
            dictating: false,
            label: None,
            hint: None,
            seq: 0,
            prev_terminal: None,
            focus_moved: None,
            last_esc: None,
            erased: (0, 0),
            dirty: false,
        }
    }

    pub fn set_config(&mut self, cfg: Config, host: &mut dyn Host) {
        self.cfg = cfg;
        self.menu = None;
        self.picker = None;
        self.layer = None;
        self.say("Controller settings reloaded", Kind::Info);
        self.flush(host);
    }

    pub fn handle(&mut self, input: Input, host: &mut dyn Host) {
        match input {
            Input::Connected(name) => {
                self.connected = Some(name);
                self.say("Controller connected · View shows the buttons", Kind::Info);
            }
            Input::Disconnected => {
                self.release_all(host);
                self.connected = None;
                host.hud(None);
                return;
            }
            Input::Press(b) => self.press(b, host),
            Input::Raw(_) => return,
            Input::Release(b) => self.release(b, host),
        }
        self.flush(host);
    }

    /// Timers: key repeat and holds. Call often (every ~15 ms).
    pub fn tick(&mut self, host: &mut dyn Host) {
        let now = Instant::now();
        if let Some(h) = &self.hold {
            if now.duration_since(h.started) >= Duration::from_millis(h.ms) {
                let h = self.hold.take().unwrap();
                self.finish_hold(h, host);
                self.dirty = true;
            }
        }
        if let Some(r) = &mut self.repeat {
            if now >= r.next && r.action == Action::Erase {
                // Like holding Backspace on a phone: characters, then whole words.
                let by_word = now.duration_since(r.pressed) >= Duration::from_millis(ERASE_WORDS_AFTER_MS);
                r.next = now + Duration::from_millis(if by_word { ERASE_WORD_MS } else { ERASE_CHAR_MS });
                self.erase(by_word, host);
                self.dirty = true;
            } else if now >= r.next {
                let t = &self.cfg.timing;
                let fast = now.duration_since(r.pressed) >= Duration::from_millis(t.repeat_fast_after_ms);
                r.next = now + Duration::from_millis(if fast { t.repeat_fast_interval_ms } else { t.repeat_interval_ms });
                let (btn, action, label) = (r.btn, r.action.clone(), r.label.clone());
                if self.picker.is_some() {
                    self.picker_nav(btn);
                } else if self.menu.is_some() {
                    self.menu_nav(btn);
                } else {
                    self.act(btn, &action, &label, host);
                }
                self.dirty = true;
            }
        }
        if self.dirty {
            self.flush(host);
        }
    }

    // ---- presses ----

    fn press(&mut self, b: Btn, host: &mut dyn Host) {
        if self.connected.is_none() {
            self.connected = Some("Controller".into());
        }
        if self.cfg.only_when_focused && !host.frontmost() {
            self.swallowed.insert(b);
            return;
        }
        // A tap-opened guide closes on the next press; B and View only close it.
        if self.legend.is_some() {
            self.legend = None;
            self.dirty = true;
            if matches!(b, Btn::B | Btn::View) {
                self.swallowed.insert(b);
                return;
            }
        }
        if self.picker.is_some() {
            self.picker_press(b, host);
            return;
        }
        if self.menu.is_some() {
            self.menu_press(b, host);
            return;
        }
        let binding = self.binding(b);
        let Some(binding) = binding else { return };
        self.pressed.insert(b, (binding.clone(), Instant::now()));
        match &binding.action {
            Action::Layer(name) => {
                self.layer = Some((b, name.clone()));
                self.dirty = true;
            }
            Action::Legend => {
                self.legend = Some(Instant::now());
                self.dirty = true;
            }
            Action::Dictation => self.dictation(true, host),
            Action::Back => self.back(b, host),
            Action::Erase => {
                self.erased = (0, 0);
                self.erase(false, host);
                let now = Instant::now();
                self.repeat = Some(Repeat { btn: b, action: Action::Erase, label: String::new(), pressed: now, next: now + Duration::from_millis(ERASE_DELAY_MS) });
            }
            Action::ClearInput => match self.focused_terminal(host) {
                Ok(Some(t)) => {
                    self.hold = Some(Hold { btn: b, kind: HoldKind::Clear, started: Instant::now(), ms: self.cfg.timing.interrupt_hold_ms, terminal: Some(t.term.terminal) });
                    self.dirty = true;
                }
                Ok(None) => self.say("No terminal is focused", Kind::Warn),
                Err(e) => self.say(&format!("Clear: {e:#}"), Kind::Warn),
            },
            action => {
                if b == Btn::A && self.guarded() {
                    self.say("A ignored for a moment after a jump, so you see the prompt first", Kind::Info);
                    return;
                }
                self.act(b, action, &binding.label, host);
                if self.cfg.repeat.contains(&b) {
                    let now = Instant::now();
                    self.repeat = Some(Repeat {
                        btn: b,
                        action: action.clone(),
                        label: binding.label.clone(),
                        pressed: now,
                        next: now + Duration::from_millis(self.cfg.timing.repeat_delay_ms),
                    });
                }
            }
        }
    }

    fn release(&mut self, b: Btn, host: &mut dyn Host) {
        if self.swallowed.remove(&b) {
            return;
        }
        if self.repeat.as_ref().is_some_and(|r| r.btn == b) {
            self.repeat = None;
        }
        if let Some(h) = &self.hold {
            if h.btn == b {
                let kind = h.kind;
                self.hold = None;
                self.say(
                    match kind {
                        HoldKind::Interrupt => "Not interrupted · hold B to interrupt a working agent",
                        HoldKind::Confirm => "Cancelled · hold A to confirm",
                        HoldKind::Clear => "Not cleared",
                    },
                    Kind::Info,
                );
            }
        }
        let Some((binding, at)) = self.pressed.remove(&b) else { return };
        match binding.action {
            Action::Layer(_) => {
                if self.layer.as_ref().is_some_and(|(lb, _)| *lb == b) {
                    self.layer = None;
                    self.dirty = true;
                }
            }
            Action::Legend => {
                // Held: a peek, gone on release. Tapped: stays until the next press.
                if at.elapsed() >= Duration::from_millis(self.cfg.timing.legend_hold_ms) {
                    self.legend = None;
                    self.dirty = true;
                }
            }
            Action::Dictation => self.dictation(false, host),
            _ => {}
        }
    }

    fn release_all(&mut self, host: &mut dyn Host) {
        if self.dictating {
            self.dictation(false, host);
        }
        self.pressed.clear();
        self.swallowed.clear();
        self.layer = None;
        self.hold = None;
        self.repeat = None;
        self.menu = None;
        self.picker = None;
        self.legend = None;
    }

    fn binding(&self, b: Btn) -> Option<Binding> {
        if let Some((_, layer)) = &self.layer {
            if let Some(binding) = self.cfg.layers.get(layer).and_then(|l| l.get(&b)) {
                return Some(binding.clone());
            }
        }
        self.cfg.buttons.get(&b).cloned()
    }

    fn guarded(&self) -> bool {
        self.focus_moved.is_some_and(|t| t.elapsed() < Duration::from_millis(self.cfg.timing.a_guard_ms))
    }

    // ---- the menu ----

    fn menu_press(&mut self, b: Btn, host: &mut dyn Host) {
        self.dirty = true;
        match b {
            Btn::Up | Btn::Down | Btn::LsUp | Btn::LsDown => {
                self.menu_nav(b);
                if b.is_dpad() {
                    let now = Instant::now();
                    self.repeat = Some(Repeat {
                        btn: b,
                        action: Action::None,
                        label: String::new(),
                        pressed: now,
                        next: now + Duration::from_millis(self.cfg.timing.repeat_delay_ms),
                    });
                }
            }
            Btn::A => {
                let Some(i) = self.menu else { return };
                let Some(item) = self.cfg.menu.get(i).cloned() else { return };
                if item.confirm {
                    self.hold = Some(Hold { btn: b, kind: HoldKind::Confirm, started: Instant::now(), ms: self.cfg.timing.confirm_hold_ms, terminal: None });
                } else {
                    self.menu = None;
                    self.act(b, &item.action, &item.label, host);
                }
            }
            Btn::B | Btn::Menu => {
                self.menu = None;
                self.swallowed.insert(b);
            }
            _ => {}
        }
    }

    // ---- New space… ----

    fn open_picker(&mut self, host: &mut dyn Host) {
        let home = std::env::var("HOME").unwrap_or_default();
        let tilde = |p: &str| if !home.is_empty() && p.starts_with(&home) { p.replacen(&home, "~", 1) } else { p.to_string() };
        let name_of = |p: &str| p.trim_end_matches('/').rsplit('/').next().unwrap_or(p).to_string();
        let mut items: Vec<Folder> = vec![];
        let mut seen = HashSet::new();
        if let Ok(ls) = host.ls() {
            for r in ls.recent {
                let temp = ["/tmp", "/private/tmp", "/var/folders", "/private/var"].iter().any(|t| r.folder.starts_with(t));
                if !temp && seen.insert(r.folder.clone()) && host.is_dir(&r.folder) {
                    items.push(Folder { name: name_of(&r.folder), detail: format!("recent · {}", tilde(&r.folder)), path: r.folder });
                }
            }
        }
        for root in self.cfg.new_space.roots.clone() {
            let root = root.replacen('~', &home, 1);
            for path in host.list_dirs(&root) {
                if seen.insert(path.clone()) {
                    let parent = path.rsplit_once('/').map(|(p, _)| tilde(p)).unwrap_or_default();
                    items.push(Folder { name: name_of(&path), detail: parent, path });
                }
            }
        }
        if items.is_empty() {
            self.say("No folders to pick: set roots in [new_space] in the config", Kind::Warn);
            return;
        }
        self.menu = None;
        self.picker = Some((items, 0));
        self.dirty = true;
    }

    fn picker_press(&mut self, b: Btn, host: &mut dyn Host) {
        self.dirty = true;
        match b {
            Btn::Up | Btn::Down | Btn::LsUp | Btn::LsDown | Btn::Lb | Btn::Rb => {
                self.picker_nav(b);
                if b.is_dpad() {
                    let now = Instant::now();
                    self.repeat = Some(Repeat { btn: b, action: Action::None, label: String::new(), pressed: now, next: now + Duration::from_millis(self.cfg.timing.repeat_delay_ms) });
                }
            }
            Btn::A | Btn::Y => {
                let Some((items, i)) = self.picker.take() else { return };
                let f = items[i].clone();
                let agent = self.cfg.new_space.agent.clone();
                let mut args = json!({ "folder": f.path });
                let what = if b == Btn::A && !agent.is_empty() {
                    args["argv"] = json!(agent);
                    format!(" + {}", agent[0])
                } else {
                    String::new()
                };
                match host.run("space.new", &args) {
                    Ok(_) => {
                        self.moved(None);
                        self.say(&format!("New space · {}{what}", f.name), Kind::Ok);
                    }
                    Err(e) => self.say(&format!("New space: {e:#}"), Kind::Warn),
                }
            }
            Btn::B => {
                // Back to the menu, on the item that opened the picker.
                self.picker = None;
                self.menu = Some(self.cfg.menu.iter().position(|m| m.action == Action::NewSpace).unwrap_or(0));
                self.swallowed.insert(b);
            }
            Btn::Menu => {
                self.picker = None;
                self.swallowed.insert(b);
            }
            _ => {}
        }
    }

    fn picker_nav(&mut self, b: Btn) {
        let Some((items, i)) = &mut self.picker else { return };
        let n = items.len();
        *i = match b {
            Btn::Up | Btn::LsUp => (*i + n - 1) % n,
            Btn::Down | Btn::LsDown => (*i + 1) % n,
            Btn::Lb => i.saturating_sub(PAGE),
            Btn::Rb => (*i + PAGE).min(n - 1),
            _ => *i,
        };
    }

    fn menu_nav(&mut self, b: Btn) {
        let n = self.cfg.menu.len();
        if n == 0 {
            return;
        }
        let i = self.menu.unwrap_or(0);
        self.menu = Some(match b {
            Btn::Up | Btn::LsUp => (i + n - 1) % n,
            _ => (i + 1) % n,
        });
    }

    // ---- actions ----

    fn act(&mut self, _b: Btn, action: &Action, label: &str, host: &mut dyn Host) {
        let r = match action {
            Action::None => Ok(()),
            Action::Keys(keys) => self.keys(keys, label, host),
            Action::Cmd(name, args) => self.cmd(name, args, label, host),
            Action::Urgent => self.urgent(host),
            Action::PreviousPane => self.previous_pane(host),
            Action::Menu => {
                if self.cfg.menu.is_empty() {
                    self.say("The menu is empty: add [[menu]] items to the config", Kind::Warn);
                } else {
                    self.menu = Some(0);
                    self.dirty = true;
                }
                Ok(())
            }
            // Handled on press/release.
            Action::NewSpace => {
                self.open_picker(host);
                Ok(())
            }
            Action::Back | Action::Legend | Action::Layer(_) | Action::Dictation | Action::Erase | Action::ClearInput => Ok(()),
        };
        if let Err(e) = r {
            self.say(&format!("{label}: {e:#}"), Kind::Warn);
        }
    }

    fn focused_terminal(&self, host: &mut dyn Host) -> Result<Option<crate::herdr::Placed>> {
        let st = host.status()?;
        let Some(pane) = st.focused_pane else { return Ok(None) };
        Ok(host.ls()?.by_pane(&pane))
    }

    fn keys(&mut self, keys: &[String], label: &str, host: &mut dyn Host) -> Result<()> {
        let Some(t) = self.focused_terminal(host)? else {
            self.say("No terminal is focused", Kind::Warn);
            return Ok(());
        };
        if keys.iter().any(|k| k == "esc") && !self.esc_allowed() {
            return Ok(());
        }
        host.keys(&t.term.terminal, keys)?;
        self.say(label, Kind::Ok);
        Ok(())
    }

    fn esc_allowed(&mut self) -> bool {
        let gap = Duration::from_millis(self.cfg.timing.esc_gap_ms);
        if self.last_esc.is_some_and(|t| t.elapsed() < gap) {
            // Two quick Escs open Claude Code's rewind menu: never send them.
            self.say("Esc skipped: too soon after the last one", Kind::Info);
            return false;
        }
        self.last_esc = Some(Instant::now());
        true
    }

    fn cmd(&mut self, name: &str, args: &Value, label: &str, host: &mut dyn Host) -> Result<()> {
        let moves_focus = name.starts_with("pane.go") || name.starts_with("tab.") || name.starts_with("space.") || name.starts_with("agent.");
        if !moves_focus {
            host.run(name, args)?;
            self.say(label, Kind::Ok);
            return Ok(());
        }
        let before = host.status()?;
        let before_term = before.focused_pane.as_deref().and_then(|p| host.ls().ok()?.by_pane(p)).map(|t| t.term.terminal);
        host.run(name, args)?;
        let after = host.status()?;
        if after.focused_pane == before.focused_pane && after.tab == before.tab && after.space == before.space {
            let why = match name {
                "pane.go" => format!("No pane {}", direction_word(args)),
                n if n.starts_with("tab.") => "This space has one tab".into(),
                n if n.starts_with("space.") => "There's only one space".into(),
                _ => "Nothing to go to".into(),
            };
            self.say(&why, Kind::Info);
            return Ok(());
        }
        self.moved(before_term);
        let place = self.place(&after, host);
        self.say(&format!("{label}{place}"), Kind::Ok);
        Ok(())
    }

    /// " · space / tab" for the label after a jump.
    fn place(&self, st: &Status, host: &mut dyn Host) -> String {
        let Ok(ls) = host.ls() else { return String::new() };
        let Some(space) = ls.spaces.iter().find(|s| Some(&s.id) == st.space.as_ref()) else { return String::new() };
        let tab = space.tabs.iter().find(|t| Some(&t.id) == st.tab.as_ref()).map(|t| t.name.as_str()).unwrap_or("");
        format!(" · {} / {}", space.name, tab)
    }

    fn moved(&mut self, from_terminal: Option<String>) {
        if from_terminal.is_some() {
            self.prev_terminal = from_terminal;
        }
        self.focus_moved = Some(Instant::now());
    }

    /// X: blocked agents first, then done ones; again: the next. None waiting: the next agent.
    fn urgent(&mut self, host: &mut dyn Host) -> Result<()> {
        let st = host.status()?;
        let ls = host.ls()?;
        let all = ls.terminals();
        let current = st.focused_pane.as_deref().and_then(|p| ls.by_pane(p)).map(|t| t.term.terminal);
        let mut queue: Vec<_> = all.iter().filter(|t| t.term.status.as_deref() == Some("blocked")).cloned().collect();
        queue.extend(all.iter().filter(|t| t.term.status.as_deref() == Some("done")).cloned());
        if queue.is_empty() {
            let agents: Vec<_> = all.iter().filter(|t| t.term.status.is_some()).collect();
            if agents.is_empty() {
                self.say("No agents yet", Kind::Info);
                return Ok(());
            }
            let i = agents.iter().position(|t| Some(&t.term.terminal) == current.as_ref()).map(|i| (i + 1) % agents.len()).unwrap_or(0);
            let target = agents[i].clone();
            return self.jump(&target, current, "Nobody needs you · next agent", host);
        }
        let i = queue.iter().position(|t| Some(&t.term.terminal) == current.as_ref()).map(|i| i + 1).unwrap_or(0);
        if i >= queue.len() && queue.len() == 1 {
            self.say("You're here · answer with D-pad + A", Kind::Info);
            return Ok(());
        }
        let target = queue[i % queue.len()].clone();
        let n = queue.len();
        let why = format!("{} ({} of {n})", target.term.status.as_deref().map(status_word).unwrap_or(""), i % n + 1);
        self.jump(&target, current, &why, host)
    }

    fn jump(&mut self, target: &crate::herdr::Placed, from: Option<String>, why: &str, host: &mut dyn Host) -> Result<()> {
        host.run("agent.focus", &json!({ "terminal": target.term.terminal }))?;
        self.moved(from);
        // A named agent leads with its name; otherwise the place says enough (a "t7" address doesn't).
        let who = target.term.name.as_ref().map(|n| format!("{n} · ")).unwrap_or_default();
        self.say(&format!("{who}{} / {} · {why}", target.space_name, target.tab_name), Kind::Ok);
        if target.term.status.as_deref() == Some("blocked") {
            self.hint = Some("D-pad choose · A confirm · B cancel".into());
        }
        Ok(())
    }

    fn previous_pane(&mut self, host: &mut dyn Host) -> Result<()> {
        let Some(prev) = self.prev_terminal.clone() else {
            self.say("Nowhere to go back to yet", Kind::Info);
            return Ok(());
        };
        let current = self.focused_terminal(host)?.map(|t| t.term.terminal);
        if host.run("pane.focus", &json!({ "terminal": prev })).is_err() {
            self.prev_terminal = None;
            self.say("That pane is gone", Kind::Info);
            return Ok(());
        }
        // Pressing again bounces back: the two most recent places.
        self.moved(current);
        let st = host.status()?;
        let place = self.place(&st, host);
        self.say(&format!("Back{place}"), Kind::Ok);
        Ok(())
    }

    /// B: closes the controller menu or a dialog first; then Esc, but a working agent
    /// needs a hold so a reflex "back" never interrupts it.
    fn back(&mut self, b: Btn, host: &mut dyn Host) {
        let r = (|| -> Result<()> {
            let st = host.status()?;
            if st.overlays.iter().any(|o| !IGNORED_OVERLAYS.contains(&o.as_str())) {
                host.run("overlay.close", &json!({}))?;
                self.say("Closed", Kind::Ok);
                return Ok(());
            }
            let Some(t) = st.focused_pane.as_deref().and_then(|p| host.ls().ok()?.by_pane(p)) else {
                self.say("No terminal is focused", Kind::Warn);
                return Ok(());
            };
            if t.term.status.as_deref() == Some("working") {
                self.hold = Some(Hold { btn: b, kind: HoldKind::Interrupt, started: Instant::now(), ms: self.cfg.timing.interrupt_hold_ms, terminal: Some(t.term.terminal) });
                self.dirty = true;
                return Ok(());
            }
            if self.esc_allowed() {
                host.keys(&t.term.terminal, &["esc".into()])?;
                self.say("Esc", Kind::Ok);
            }
            Ok(())
        })();
        if let Err(e) = r {
            self.say(&format!("Back: {e:#}"), Kind::Warn);
        }
    }

    fn finish_hold(&mut self, h: Hold, host: &mut dyn Host) {
        match h.kind {
            HoldKind::Interrupt => {
                let Some(t) = h.terminal else { return };
                // The hold already made it deliberate; still never two Escs back to back.
                if !self.esc_allowed() {
                    return;
                }
                match host.keys(&t, &["esc".into()]) {
                    Ok(()) => self.say("Stopped the agent", Kind::Ok),
                    Err(e) => self.say(&format!("Interrupt: {e:#}"), Kind::Warn),
                }
            }
            HoldKind::Clear => {
                let Some(t) = h.terminal else { return };
                // End of the line, then line-by-line: Ctrl+U empties a line and Backspace joins
                // it to the one above. Works for Claude Code's multi-line box and for a shell
                // prompt; on an empty input the extra keys do nothing.
                let mut keys = vec!["ctrl+e".to_string()];
                for _ in 0..CLEAR_LINES {
                    keys.push("ctrl+u".into());
                    keys.push("backspace".into());
                }
                match host.keys(&t, &keys) {
                    Ok(()) => self.say("Input cleared", Kind::Ok),
                    Err(e) => self.say(&format!("Clear: {e:#}"), Kind::Warn),
                }
            }
            HoldKind::Confirm => {
                let Some(i) = self.menu.take() else { return };
                let Some(item): Option<MenuItem> = self.cfg.menu.get(i).cloned() else { return };
                self.act(h.btn, &item.action, &item.label, host);
            }
        }
    }

    fn dictation(&mut self, down: bool, host: &mut dyn Host) {
        let d = self.cfg.dictation.clone();
        if !d.is_set_up() {
            if down {
                self.say("Dictation isn't set up yet: see [dictation] in the config", Kind::Info);
            }
            return;
        }
        if !down && !self.dictating {
            return; // it never started (e.g. no permission): nothing to let go of
        }
        let argv = if down { d.press } else { d.release };
        let mut r = Ok(());
        if let Some(key) = d.hold_key.as_deref().filter(|k| !k.is_empty()) {
            r = host.hold_key(key, down);
        }
        if r.is_ok() && !argv.is_empty() {
            r = host.exec(&argv);
        }
        match r {
            Ok(()) => {
                self.dictating = down;
                if !down {
                    self.say("Dictated", Kind::Ok);
                    self.hint = Some(self.after_dictation_hint());
                }
            }
            Err(e) => {
                self.dictating = false;
                self.say(&format!("Dictation: {e:#}"), Kind::Warn);
            }
        }
        self.dirty = true;
    }

    /// One step of erasing in the focused terminal.
    fn erase(&mut self, by_word: bool, host: &mut dyn Host) {
        let key = if by_word { "ctrl+w" } else { "backspace" };
        let r = self.focused_terminal(host).and_then(|t| match t {
            Some(t) => host.keys(&t.term.terminal, &[key.to_string()]).map(|_| true),
            None => Ok(false),
        });
        match r {
            Ok(true) => {
                if by_word { self.erased.1 += 1 } else { self.erased.0 += 1 }
                let (c, w) = self.erased;
                let plural = |n: u32, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
                let text = match (c, w) {
                    (c, 0) => format!("Erased {}", plural(c, "character")),
                    (0, w) => format!("Erased {}", plural(w, "word")),
                    (c, w) => format!("Erased {}, {}", plural(c, "character"), plural(w, "word")),
                };
                self.say(&text, Kind::Ok);
            }
            Ok(false) => self.say("No terminal is focused", Kind::Warn),
            Err(e) => {
                self.repeat = None;
                self.say(&format!("Erase: {e:#}"), Kind::Warn);
            }
        }
    }

    /// "A to send · R-stick ← erase · hold R-stick ↓ to clear", from the actual bindings.
    fn after_dictation_hint(&self) -> String {
        let find = |a: &Action| self.cfg.buttons.iter().find(|(_, b)| &b.action == a).map(|(btn, _)| btn.glyph());
        let mut parts = vec![];
        if self.cfg.buttons.get(&Btn::A).is_some_and(|b| b.action == Action::Keys(vec!["enter".into()])) {
            parts.push("A to send".to_string());
        }
        if let Some(g) = find(&Action::Erase) {
            parts.push(format!("{g} erase"));
        }
        if let Some(g) = find(&Action::ClearInput) {
            parts.push(format!("hold {g} to clear"));
        }
        parts.join(" · ")
    }

    // ---- the on-screen view ----

    fn say(&mut self, text: &str, kind: Kind) {
        self.label = Some((text.to_string(), kind));
        self.hint = None;
        self.seq += 1;
        self.dirty = true;
    }

    fn flush(&mut self, host: &mut dyn Host) {
        self.dirty = false;
        if self.connected.is_none() {
            return;
        }
        let view = self.view();
        host.hud(Some(&view));
    }

    pub fn view(&self) -> Value {
        let mode = if self.picker.is_some() {
            "picker"
        } else if self.menu.is_some() {
            "menu"
        } else if self.legend.is_some() {
            "legend"
        } else if self.layer.is_some() {
            "layer"
        } else {
            "compact"
        };
        let (label, kind) = self.label.clone().unwrap_or_default();
        json!({
            "mode": mode,
            "pad": self.connected,
            "label": label,
            "hint": self.hint,
            "kind": match kind { Kind::Ok => "ok", Kind::Warn => "warn", Kind::Info => "info" },
            "seq": self.seq,
            "picker": self.picker.as_ref().map(|(items, i)| json!({
                "index": i,
                "items": items.iter().map(|f| json!({ "name": f.name, "detail": f.detail })).collect::<Vec<_>>(),
                "agent": self.cfg.new_space.agent.first(),
            })),
            "menu": self.menu.map(|i| json!({
                "index": i,
                "items": self.cfg.menu.iter().map(|m| json!({ "label": m.label, "confirm": m.confirm })).collect::<Vec<_>>(),
            })),
            "layer": self.layer.as_ref().map(|(b, name)| json!({
                "name": name,
                "button": b.glyph(),
                "legend": self.legend_rows(self.cfg.layers.get(name)),
            })),
            "legend": if mode == "legend" { json!(self.legend_rows(Some(&self.cfg.buttons))) } else { Value::Null },
            "hold": self.hold.as_ref().map(|h| json!({
                "label": match h.kind {
                    HoldKind::Interrupt => "Keep holding B to stop the agent".to_string(),
                    HoldKind::Clear => "Keep holding to clear the input".to_string(),
                    HoldKind::Confirm => {
                        let what = self.menu.and_then(|i| self.cfg.menu.get(i)).map(|m| m.label.to_lowercase()).unwrap_or_default();
                        format!("Keep holding A to {what}")
                    }
                },
                "ms": h.ms,
                "seq": self.seq,
            })),
            "dictating": self.dictating,
        })
    }

    /// The guide's rows, most used first. Directions and bumper pairs share a row.
    fn legend_rows(&self, map: Option<&HashMap<Btn, Binding>>) -> Vec<Value> {
        let Some(map) = map else { return vec![] };
        let bound = |b: &Btn| map.get(b).filter(|x| x.action != Action::None);
        let no_dictation = !self.cfg.dictation.is_set_up();
        let mut rows = vec![];
        let mut row = |button: &str, buttons: &[Btn], summary: Option<&str>| {
            let labels: Vec<&str> = buttons.iter().filter_map(|b| bound(b)).map(|x| x.label.as_str()).collect();
            if labels.is_empty() {
                return;
            }
            let mut label = summary.map(str::to_string).unwrap_or_else(|| labels.join(" / "));
            let off = buttons.iter().any(|b| bound(b).is_some_and(|x| x.action == Action::Dictation)) && no_dictation;
            if off {
                label.push_str(" (not set up)");
            }
            rows.push(json!({ "button": button, "label": label, "dim": off }));
        };
        let dirs = |a, b, c, d| [a, b, c, d];
        // Defaults get a short summary; a remapped group lists its own labels.
        let all_cmd = |bs: &[Btn], cmd: &str| bs.iter().all(|b| matches!(map.get(b).map(|x| &x.action), Some(Action::Cmd(n, _)) if n == cmd));
        for b in [Btn::A, Btn::B, Btn::X, Btn::Y] {
            row(b.glyph(), &[b], None);
        }
        let dpad = dirs(Btn::Up, Btn::Down, Btn::Left, Btn::Right);
        row("D-pad", &dpad, (map.get(&Btn::Up).is_some_and(|x| x.action == Action::Keys(vec!["up".into()]))).then_some("Arrow keys (menus, history)"));
        let ls = dirs(Btn::LsUp, Btn::LsDown, Btn::LsLeft, Btn::LsRight);
        row("Left stick", &ls, all_cmd(&ls, "pane.go").then_some("Flick: move to the pane that way"));
        for b in [Btn::RsLeft, Btn::RsDown, Btn::RsUp, Btn::RsRight] {
            row(b.glyph(), &[b], None);
        }
        row("LB / RB", &[Btn::Lb, Btn::Rb], None);
        for b in [Btn::Lt, Btn::Rt, Btn::R3, Btn::L3, Btn::Menu, Btn::View, Btn::Share, Btn::Guide] {
            row(b.glyph(), &[b], None);
        }
        rows
    }
}

fn direction_word(args: &Value) -> String {
    match args.get("direction").and_then(Value::as_str) {
        Some("left") => "to the left".into(),
        Some("right") => "to the right".into(),
        Some("up") => "above".into(),
        Some("down") => "below".into(),
        _ => "there".into(),
    }
}

fn status_word(s: &str) -> &'static str {
    match s {
        "blocked" => "needs you",
        "done" => "finished",
        "working" => "working",
        _ => "idle",
    }
}

#[cfg(test)]
mod tests;
