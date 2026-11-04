use super::*;
use crate::config::Config;
use serde_json::json;

/// A pretend Herdr: one space, one tab, terminals t1..tN in panes p1..pN.
struct Fake {
    focused: String,
    statuses: Vec<Option<&'static str>>,
    overlays: Vec<String>,
    front: bool,
    calls: Vec<String>,
    huds: Vec<Value>,
}

impl Fake {
    fn new(statuses: Vec<Option<&'static str>>) -> Fake {
        Fake { focused: "p1".into(), statuses, overlays: vec![], front: true, calls: vec![], huds: vec![] }
    }
    fn last_hud(&self) -> &Value {
        self.huds.last().expect("a hud")
    }
    fn label(&self) -> String {
        self.last_hud()["label"].as_str().unwrap_or("").to_string()
    }
}

impl Host for Fake {
    fn run(&mut self, name: &str, args: &Value) -> Result<Value> {
        self.calls.push(format!("do {name} {args}"));
        match name {
            "agent.focus" | "pane.focus" => {
                let t = args["terminal"].as_str().unwrap();
                self.focused = format!("p{}", &t[1..]);
            }
            "pane.go" => {
                let n: usize = self.focused[1..].parse().unwrap();
                let next = if args["direction"] == "right" { n + 1 } else { n.saturating_sub(1) };
                if next >= 1 && next <= self.statuses.len() {
                    self.focused = format!("p{next}");
                }
            }
            "overlay.close" => {
                self.overlays.pop();
            }
            _ => {}
        }
        Ok(json!({"ok": true}))
    }
    fn status(&mut self) -> Result<Status> {
        Ok(Status { space: Some("s1".into()), tab: Some("tb1".into()), focused_pane: Some(self.focused.clone()), overlays: self.overlays.clone() })
    }
    fn ls(&mut self) -> Result<Ls> {
        let terms: Vec<Value> = self.statuses.iter().enumerate()
            .map(|(i, s)| json!({ "terminal": format!("t{}", i + 1), "pane": format!("p{}", i + 1), "status": s, "name": null }))
            .collect();
        Ok(serde_json::from_value(json!({ "spaces": [{ "id": "s1", "name": "api", "active": true,
            "tabs": [{ "id": "tb1", "name": "main", "active": true, "terminals": terms }] }] }))?)
    }
    fn keys(&mut self, terminal: &str, keys: &[String]) -> Result<()> {
        self.calls.push(format!("keys {terminal} {}", keys.join(" ")));
        Ok(())
    }
    fn frontmost(&mut self) -> bool {
        self.front
    }
    fn exec(&mut self, argv: &[String]) -> Result<()> {
        self.calls.push(format!("exec {}", argv.join(" ")));
        Ok(())
    }
    fn hud(&mut self, view: Option<&Value>) {
        if let Some(v) = view {
            self.huds.push(v.clone());
        }
    }
    fn list_dirs(&mut self, root: &str) -> Vec<String> {
        (1..=20).map(|i| format!("{root}/proj{i}")).collect()
    }
    fn is_dir(&mut self, path: &str) -> bool {
        !path.starts_with("/gone")
    }
    fn hold_key(&mut self, key: &str, down: bool) -> Result<()> {
        if key == "denied" {
            anyhow::bail!("needs Accessibility permission");
        }
        self.calls.push(format!("key {key} {}", if down { "down" } else { "up" }));
        Ok(())
    }
}

fn engine() -> Engine {
    let mut cfg = Config::default_config();
    cfg.timing.interrupt_hold_ms = 40;
    cfg.timing.confirm_hold_ms = 40;
    cfg.timing.legend_hold_ms = 40;
    cfg.timing.esc_gap_ms = 60;
    cfg.timing.a_guard_ms = 60;
    let mut e = Engine::new(cfg);
    e.connected = Some("pad".into());
    e
}

fn tap(e: &mut Engine, f: &mut Fake, b: Btn) {
    e.handle(Input::Press(b), f);
    e.handle(Input::Release(b), f);
}

fn wait(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn hold(e: &mut Engine, f: &mut Fake, b: Btn, ms: u64) {
    e.handle(Input::Press(b), f);
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        e.tick(f);
        wait(5);
    }
    e.handle(Input::Release(b), f);
}

#[test]
fn a_sends_enter_to_focused_terminal() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None, None]));
    tap(&mut e, &mut f, Btn::A);
    assert_eq!(f.calls, vec!["keys t1 enter"]);
}

#[test]
fn b_taps_esc_on_a_shell_or_blocked_agent() {
    let (mut e, mut f) = (engine(), Fake::new(vec![Some("blocked")]));
    tap(&mut e, &mut f, Btn::B);
    assert_eq!(f.calls, vec!["keys t1 esc"]);
}

#[test]
fn b_needs_a_hold_to_interrupt_a_working_agent() {
    let (mut e, mut f) = (engine(), Fake::new(vec![Some("working")]));
    tap(&mut e, &mut f, Btn::B);
    assert!(f.calls.is_empty(), "a tap must not interrupt: {:?}", f.calls);
    assert!(f.label().contains("hold B"), "{}", f.label());
    hold(&mut e, &mut f, Btn::B, 80);
    assert_eq!(f.calls, vec!["keys t1 esc"]);
    assert_eq!(f.label(), "Stopped the agent");
}

#[test]
fn hold_progress_is_shown() {
    let (mut e, mut f) = (engine(), Fake::new(vec![Some("working")]));
    e.handle(Input::Press(Btn::B), &mut f);
    assert_eq!(f.last_hud()["hold"]["ms"], 40);
}

#[test]
fn never_two_escs_in_a_row() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    tap(&mut e, &mut f, Btn::B);
    tap(&mut e, &mut f, Btn::B);
    assert_eq!(f.calls, vec!["keys t1 esc"]);
    wait(70);
    tap(&mut e, &mut f, Btn::B);
    assert_eq!(f.calls.len(), 2);
}

#[test]
fn b_closes_a_dialog_before_anything_else() {
    let (mut e, mut f) = (engine(), Fake::new(vec![Some("working")]));
    f.overlays = vec!["controller".into(), "frame-welcome".into(), "picker".into()];
    tap(&mut e, &mut f, Btn::B);
    assert_eq!(f.calls, vec!["do overlay.close {}"]);
    // Our own overlay and the welcome window don't count as dialogs.
    f.calls.clear();
    f.statuses = vec![None];
    tap(&mut e, &mut f, Btn::B);
    assert_eq!(f.calls, vec!["keys t1 esc"]);
}

#[test]
fn x_goes_blocked_first_then_done_and_cycles() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None, Some("done"), Some("blocked"), Some("working")]));
    tap(&mut e, &mut f, Btn::X);
    assert_eq!(f.focused, "p3");
    assert!(f.label().contains("needs you (1 of 2)"), "{}", f.label());
    assert_eq!(f.last_hud()["hint"], "D-pad choose · A confirm · B cancel");
    tap(&mut e, &mut f, Btn::X);
    assert_eq!(f.focused, "p2");
    tap(&mut e, &mut f, Btn::X);
    assert_eq!(f.focused, "p3");
}

#[test]
fn x_with_nobody_waiting_goes_to_the_next_agent() {
    let (mut e, mut f) = (engine(), Fake::new(vec![Some("working"), None, Some("idle")]));
    tap(&mut e, &mut f, Btn::X);
    assert_eq!(f.focused, "p3");
    assert!(f.label().contains("Nobody needs you"));
}

#[test]
fn a_is_ignored_right_after_a_jump() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None, Some("blocked")]));
    tap(&mut e, &mut f, Btn::X);
    tap(&mut e, &mut f, Btn::A);
    assert!(!f.calls.iter().any(|c| c.starts_with("keys")), "{:?}", f.calls);
    wait(70);
    tap(&mut e, &mut f, Btn::A);
    assert!(f.calls.iter().any(|c| c == "keys t2 enter"));
}

#[test]
fn r3_goes_back_and_forth() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None, Some("blocked")]));
    tap(&mut e, &mut f, Btn::X);
    assert_eq!(f.focused, "p2");
    tap(&mut e, &mut f, Btn::R3);
    assert_eq!(f.focused, "p1");
    tap(&mut e, &mut f, Btn::R3);
    assert_eq!(f.focused, "p2");
}

#[test]
fn stick_moves_panes_and_says_when_there_is_none() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None, None]));
    tap(&mut e, &mut f, Btn::LsRight);
    assert_eq!(f.focused, "p2");
    tap(&mut e, &mut f, Btn::LsRight);
    assert_eq!(f.label(), "No pane to the right");
}

#[test]
fn lt_turns_bumpers_into_space_switching() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    tap(&mut e, &mut f, Btn::Rb);
    assert!(f.calls.iter().any(|c| c.starts_with("do tab.next")));
    e.handle(Input::Press(Btn::Lt), &mut f);
    assert_eq!(f.last_hud()["mode"], "layer");
    tap(&mut e, &mut f, Btn::Rb);
    assert!(f.calls.iter().any(|c| c.starts_with("do space.next")));
    e.handle(Input::Release(Btn::Lt), &mut f);
    assert_eq!(f.last_hud()["mode"], "compact");
    // LT alone does nothing.
    assert!(!f.calls.iter().any(|c| c.contains("lt")));
}

#[test]
fn bumper_held_across_lt_release_keeps_its_meaning() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    e.handle(Input::Press(Btn::Lt), &mut f);
    e.handle(Input::Press(Btn::Rb), &mut f);
    e.handle(Input::Release(Btn::Lt), &mut f);
    e.handle(Input::Release(Btn::Rb), &mut f);
    let n = f.calls.iter().filter(|c| c.starts_with("do space.next")).count();
    assert_eq!(n, 1);
    assert!(!f.calls.iter().any(|c| c.starts_with("do tab.")));
}

#[test]
fn menu_navigates_picks_and_closes() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    tap(&mut e, &mut f, Btn::Menu);
    assert_eq!(f.last_hud()["mode"], "menu");
    assert_eq!(f.last_hud()["menu"]["index"], 0);
    tap(&mut e, &mut f, Btn::Down);
    tap(&mut e, &mut f, Btn::Down);
    tap(&mut e, &mut f, Btn::Down);
    assert_eq!(f.last_hud()["menu"]["index"], 3);
    // The D-pad moves the menu, it doesn't type arrows into the terminal.
    assert!(f.calls.is_empty(), "{:?}", f.calls);
    tap(&mut e, &mut f, Btn::A);
    assert_eq!(f.calls, vec![r#"do pane.split {"direction":"right"}"#]);
    assert_eq!(f.last_hud()["mode"], "compact");
}

#[test]
fn menu_up_wraps_to_the_last_item_and_b_closes() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    tap(&mut e, &mut f, Btn::Menu);
    tap(&mut e, &mut f, Btn::Up);
    assert_eq!(f.last_hud()["menu"]["index"], 8);
    tap(&mut e, &mut f, Btn::B);
    assert_eq!(f.last_hud()["mode"], "compact");
    assert!(f.calls.is_empty(), "B closing the menu must not also send Esc: {:?}", f.calls);
}

#[test]
fn destructive_menu_items_need_a_hold() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    tap(&mut e, &mut f, Btn::Menu);
    tap(&mut e, &mut f, Btn::Up); // Close pane
    tap(&mut e, &mut f, Btn::A);
    assert!(f.calls.is_empty());
    assert_eq!(f.last_hud()["mode"], "menu");
    hold(&mut e, &mut f, Btn::A, 80);
    assert_eq!(f.calls, vec!["do pane.close {}"]);
}

#[test]
fn view_tap_keeps_the_guide_and_hold_peeks() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    tap(&mut e, &mut f, Btn::View);
    assert_eq!(f.last_hud()["mode"], "legend");
    assert!(f.last_hud()["legend"].as_array().unwrap().iter().any(|r| r["button"] == "X"));
    tap(&mut e, &mut f, Btn::View);
    assert_eq!(f.last_hud()["mode"], "compact");
    hold(&mut e, &mut f, Btn::View, 60);
    assert_eq!(f.last_hud()["mode"], "compact");
}

#[test]
fn b_closes_the_guide_without_esc() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    tap(&mut e, &mut f, Btn::View);
    tap(&mut e, &mut f, Btn::B);
    assert_eq!(f.last_hud()["mode"], "compact");
    assert!(f.calls.is_empty());
}

#[test]
fn ignores_the_pad_while_another_app_is_in_front() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    f.front = false;
    e.handle(Input::Press(Btn::A), &mut f);
    f.front = true; // switched back while A was held: its release is still ignored
    e.handle(Input::Release(Btn::A), &mut f);
    assert!(f.calls.is_empty());
}

#[test]
fn dpad_repeats_while_held() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    e.cfg.timing.repeat_delay_ms = 30;
    e.cfg.timing.repeat_interval_ms = 10;
    hold(&mut e, &mut f, Btn::Down, 80);
    let n = f.calls.iter().filter(|c| *c == "keys t1 down").count();
    assert!(n >= 3, "{n} presses: {:?}", f.calls);
}

#[test]
fn dictation_without_setup_says_so_and_with_setup_runs_press_and_release() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    e.cfg.dictation.hold_key = None;
    tap(&mut e, &mut f, Btn::Rt);
    assert!(f.label().contains("isn't set up"));
    e.cfg.dictation.press = vec!["talk".into(), "start".into()];
    e.cfg.dictation.release = vec!["talk".into(), "stop".into()];
    e.handle(Input::Press(Btn::Rt), &mut f);
    assert_eq!(f.last_hud()["dictating"], true);
    e.handle(Input::Release(Btn::Rt), &mut f);
    assert_eq!(f.calls, vec!["exec talk start", "exec talk stop"]);
    assert_eq!(f.last_hud()["dictating"], false);
}

#[test]
fn disconnect_stops_dictation_and_hides() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    e.cfg.dictation.hold_key = None;
    e.cfg.dictation.press = vec!["talk".into()];
    e.cfg.dictation.release = vec!["stop".into()];
    e.handle(Input::Press(Btn::Rt), &mut f);
    e.handle(Input::Disconnected, &mut f);
    assert_eq!(f.calls, vec!["exec talk", "exec stop"]);
}

fn open_picker(e: &mut Engine, f: &mut Fake) {
    e.cfg.new_space.roots = vec!["/p".into()];
    tap(e, f, Btn::Menu);
    tap(e, f, Btn::Down);
    tap(e, f, Btn::Down);
    assert_eq!(f.last_hud()["menu"]["items"][2]["label"], "New space…");
    tap(e, f, Btn::A);
    assert_eq!(f.last_hud()["mode"], "picker");
}

#[test]
fn new_space_with_claude_on_a() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    open_picker(&mut e, &mut f);
    assert_eq!(f.last_hud()["picker"]["items"][0]["name"], "proj1");
    tap(&mut e, &mut f, Btn::Down);
    tap(&mut e, &mut f, Btn::A);
    assert_eq!(f.calls, vec![r#"do space.new {"argv":["claude"],"folder":"/p/proj2"}"#]);
    assert_eq!(f.label(), "New space · proj2 + claude");
    assert_eq!(f.last_hud()["mode"], "compact");
}

#[test]
fn new_space_with_a_shell_on_y_and_paging() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    open_picker(&mut e, &mut f);
    tap(&mut e, &mut f, Btn::Rb);
    tap(&mut e, &mut f, Btn::Rb);
    tap(&mut e, &mut f, Btn::Rb);
    assert_eq!(f.last_hud()["picker"]["index"], 19); // clamps at the end
    tap(&mut e, &mut f, Btn::Lb);
    assert_eq!(f.last_hud()["picker"]["index"], 11);
    tap(&mut e, &mut f, Btn::Y);
    assert_eq!(f.calls, vec![r#"do space.new {"folder":"/p/proj12"}"#]);
}

#[test]
fn b_in_the_picker_goes_back_to_the_menu() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    open_picker(&mut e, &mut f);
    tap(&mut e, &mut f, Btn::B);
    assert_eq!(f.last_hud()["mode"], "menu");
    assert_eq!(f.last_hud()["menu"]["index"], 2);
    assert!(f.calls.is_empty());
}

#[test]
fn rt_holds_fn_for_wispr_flow() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    e.handle(Input::Press(Btn::Rt), &mut f);
    assert_eq!(f.last_hud()["dictating"], true);
    e.handle(Input::Release(Btn::Rt), &mut f);
    assert_eq!(f.calls, vec!["key fn down", "key fn up"]);
    assert_eq!(f.last_hud()["dictating"], false);
}

#[test]
fn dictation_without_permission_says_so_and_never_sticks() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    e.cfg.dictation.hold_key = Some("denied".into());
    e.handle(Input::Press(Btn::Rt), &mut f);
    assert!(f.label().contains("Accessibility"), "{}", f.label());
    assert_eq!(f.last_hud()["dictating"], false);
    e.handle(Input::Release(Btn::Rt), &mut f);
    assert!(f.calls.is_empty());
}

#[test]
fn right_stick_left_erases_then_goes_word_by_word() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    tap(&mut e, &mut f, Btn::RsLeft);
    assert_eq!(f.calls, vec!["keys t1 backspace"]);
    assert_eq!(f.label(), "Erased 1 character");
    f.calls.clear();
    hold(&mut e, &mut f, Btn::RsLeft, 1500);
    let chars = f.calls.iter().filter(|c| *c == "keys t1 backspace").count();
    let words = f.calls.iter().filter(|c| *c == "keys t1 ctrl+w").count();
    assert!(chars >= 8, "{chars} backspaces");
    assert!(words >= 1, "{words} words: {:?}", f.calls);
    assert!(f.label().contains("word"), "{}", f.label());
    // Stops when the stick comes back.
    let n = f.calls.len();
    e.tick(&mut f);
    wait(250);
    e.tick(&mut f);
    assert_eq!(f.calls.len(), n);
}

#[test]
fn clearing_needs_a_hold() {
    let (mut e, mut f) = (engine(), Fake::new(vec![Some("idle")]));
    tap(&mut e, &mut f, Btn::RsDown);
    assert!(f.calls.is_empty());
    assert_eq!(f.label(), "Not cleared");
    hold(&mut e, &mut f, Btn::RsDown, 80);
    assert_eq!(f.calls.len(), 1);
    assert!(f.calls[0].starts_with("keys t1 ctrl+e ctrl+u backspace ctrl+u"), "{}", f.calls[0]);
    assert_eq!(f.label(), "Input cleared");
}

#[test]
fn after_dictation_the_panel_says_how_to_send_or_fix_it() {
    let (mut e, mut f) = (engine(), Fake::new(vec![None]));
    tap(&mut e, &mut f, Btn::Rt);
    assert_eq!(f.label(), "Dictated");
    assert_eq!(f.last_hud()["hint"], "A to send · R-stick ← erase · hold R-stick ↓ to clear");
}
