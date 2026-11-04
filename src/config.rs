//! The person's mapping: `~/.config/herdr-pad/config.toml`, defaults built in.

use crate::input::Btn;
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;

pub const DEFAULT_TOML: &str = include_str!("../config/default.toml");

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    None,
    Keys(Vec<String>),
    Cmd(String, Value),
    Back,
    Urgent,
    PreviousPane,
    Menu,
    Legend,
    Layer(String),
    Dictation,
    NewSpace,
    Erase,
    ClearInput,
}

impl Action {
    pub fn parse(s: &str) -> Result<Action> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix("keys:") {
            let keys: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
            if keys.is_empty() {
                bail!("\"{s}\": keys: needs at least one key");
            }
            return Ok(Action::Keys(keys));
        }
        if let Some(rest) = s.strip_prefix("cmd:") {
            let rest = rest.trim();
            let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if name.is_empty() {
                bail!("\"{s}\": cmd: needs a command name");
            }
            return Ok(Action::Cmd(name.to_string(), parse_args(args)?));
        }
        if let Some(rest) = s.strip_prefix("layer:") {
            return Ok(Action::Layer(rest.trim().to_string()));
        }
        Ok(match s {
            "none" | "" => Action::None,
            "back" => Action::Back,
            "urgent" => Action::Urgent,
            "previous_pane" => Action::PreviousPane,
            "menu" => Action::Menu,
            "legend" => Action::Legend,
            "dictation" => Action::Dictation,
            "new_space" => Action::NewSpace,
            "erase" => Action::Erase,
            "clear_input" => Action::ClearInput,
            _ => bail!("unknown action \"{s}\""),
        })
    }
}

/// `direction=left argv=["claude"]` → {"direction":"left","argv":["claude"]}.
/// Values parse as JSON when they can, else stay text. Values can't contain spaces
/// unless they're JSON (quoted strings and arrays are kept together).
fn parse_args(s: &str) -> Result<Value> {
    let mut map = Map::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let (key, after) = rest.split_once('=').ok_or_else(|| anyhow!("argument \"{rest}\" needs key=value"))?;
        let (value, next) = split_value(after);
        let v = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()));
        map.insert(key.trim().to_string(), v);
        rest = next.trim_start();
    }
    Ok(Value::Object(map))
}

/// Splits off one value, keeping JSON brackets and quotes together.
fn split_value(s: &str) -> (&str, &str) {
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    for (i, c) in s.char_indices() {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '[' | '{' => depth += 1,
            ']' | '}' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => return (&s[..i], &s[i..]),
            _ => {}
        }
    }
    (s, "")
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub action: Action,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: String,
    pub action: Action,
    pub confirm: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Timing {
    pub repeat: Vec<String>,
    pub repeat_delay_ms: u64,
    pub repeat_interval_ms: u64,
    pub repeat_fast_after_ms: u64,
    pub repeat_fast_interval_ms: u64,
    pub interrupt_hold_ms: u64,
    pub confirm_hold_ms: u64,
    pub legend_hold_ms: u64,
    pub a_guard_ms: u64,
    pub esc_gap_ms: u64,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            repeat: vec![],
            repeat_delay_ms: 380,
            repeat_interval_ms: 90,
            repeat_fast_after_ms: 1500,
            repeat_fast_interval_ms: 50,
            interrupt_hold_ms: 500,
            confirm_hold_ms: 800,
            legend_hold_ms: 300,
            a_guard_ms: 250,
            esc_gap_ms: 400,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Dictation {
    /// A key held down for as long as the button is (push to talk): "fn", "f13".."f20".
    pub hold_key: Option<String>,
    pub press: Vec<String>,
    pub release: Vec<String>,
}

impl Dictation {
    pub fn is_set_up(&self) -> bool {
        self.hold_key.as_deref().is_some_and(|k| !k.is_empty()) || !self.press.is_empty() || !self.release.is_empty()
    }
}

/// Where "New space…" looks for folders, and what a new space starts.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NewSpace {
    pub roots: Vec<String>,
    pub agent: Vec<String>,
}

impl Default for NewSpace {
    fn default() -> Self {
        NewSpace { roots: vec!["~/Projects".into()], agent: vec!["claude".into()] }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub only_when_focused: bool,
    pub buttons: HashMap<Btn, Binding>,
    pub layers: HashMap<String, HashMap<Btn, Binding>>,
    pub dictation: Dictation,
    pub new_space: NewSpace,
    pub timing: Timing,
    pub repeat: Vec<Btn>,
    pub menu: Vec<MenuItem>,
}

// ---- file format ----

#[derive(Deserialize)]
#[serde(untagged)]
enum RawBinding {
    Plain(String),
    Full { action: String, label: Option<String> },
}

#[derive(Deserialize)]
struct RawMenuItem {
    label: String,
    action: String,
    #[serde(default)]
    confirm: bool,
}

#[derive(Deserialize)]
struct RawConfig {
    #[serde(default = "yes")]
    only_when_focused: bool,
    #[serde(default)]
    buttons: HashMap<String, RawBinding>,
    #[serde(default)]
    layers: HashMap<String, HashMap<String, RawBinding>>,
    #[serde(default)]
    dictation: Dictation,
    #[serde(default)]
    new_space: NewSpace,
    #[serde(default)]
    timing: Timing,
    #[serde(default)]
    menu: Vec<RawMenuItem>,
}

fn yes() -> bool {
    true
}

fn bindings(raw: HashMap<String, RawBinding>, section: &str) -> Result<HashMap<Btn, Binding>> {
    let mut out = HashMap::new();
    for (name, rb) in raw {
        let btn = Btn::from_name(&name).ok_or_else(|| anyhow!("[{section}] unknown button \"{name}\""))?;
        let (action, label) = match rb {
            RawBinding::Plain(a) => (a, None),
            RawBinding::Full { action, label } => (action, label),
        };
        let parsed = Action::parse(&action).with_context(|| format!("[{section}] {name}"))?;
        let label = label.unwrap_or_else(|| action.clone());
        out.insert(btn, Binding { action: parsed, label });
    }
    Ok(out)
}

impl Config {
    pub fn parse(text: &str) -> Result<Config> {
        let raw: RawConfig = toml::from_str(text)?;
        let buttons = bindings(raw.buttons, "buttons")?;
        let mut layers = HashMap::new();
        for (name, b) in raw.layers {
            let section = format!("layers.{name}");
            layers.insert(name, bindings(b, &section)?);
        }
        for b in buttons.values() {
            if let Action::Layer(l) = &b.action {
                if !layers.contains_key(l) {
                    bail!("layer \"{l}\" is used but there's no [layers.{l}]");
                }
            }
        }
        if let Some(k) = raw.dictation.hold_key.as_deref() {
            if !k.is_empty() && !crate::keys::valid(k) {
                bail!("[dictation] hold_key \"{k}\": use \"fn\" or \"f13\" to \"f20\"");
            }
        }
        let repeat = raw.timing.repeat.iter()
            .map(|n| Btn::from_name(n).ok_or_else(|| anyhow!("[timing] repeat: unknown button \"{n}\"")))
            .collect::<Result<Vec<_>>>()?;
        let menu = raw.menu.into_iter()
            .map(|m| Ok(MenuItem { action: Action::parse(&m.action).with_context(|| format!("[[menu]] {}", m.label))?, label: m.label, confirm: m.confirm }))
            .collect::<Result<Vec<_>>>()?;
        Ok(Config { only_when_focused: raw.only_when_focused, buttons, layers, dictation: raw.dictation, new_space: raw.new_space, timing: raw.timing, repeat, menu })
    }

    #[cfg(test)]
    pub fn default_config() -> Config {
        Config::parse(DEFAULT_TOML).expect("built-in config is valid")
    }
}

pub fn config_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".config/herdr-pad")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Writes the defaults on first run so the person has a file to edit.
pub fn ensure_file() -> Result<PathBuf> {
    let p = config_path();
    if !p.exists() {
        std::fs::create_dir_all(config_dir())?;
        std::fs::write(&p, DEFAULT_TOML)?;
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_parses() {
        let c = Config::default_config();
        assert_eq!(c.buttons[&Btn::A].action, Action::Keys(vec!["enter".into()]));
        assert_eq!(c.layers["spaces"][&Btn::Rb].action, Action::Cmd("space.next".into(), json!({})));
        assert_eq!(c.menu.len(), 9);
        assert_eq!(c.new_space.agent, vec!["claude".to_string()]);
        assert!(c.repeat.contains(&Btn::Up));
    }

    #[test]
    fn cmd_args() {
        assert_eq!(
            Action::parse(r#"cmd:tab.new argv=["claude", "--x"] name="fix login" n=2"#).unwrap(),
            Action::Cmd("tab.new".into(), json!({"argv": ["claude", "--x"], "name": "fix login", "n": 2}))
        );
        assert_eq!(Action::parse("cmd:pane.go direction=left").unwrap(), Action::Cmd("pane.go".into(), json!({"direction": "left"})));
        assert!(Action::parse("bogus").is_err());
    }

    #[test]
    fn missing_layer_is_an_error() {
        assert!(Config::parse("[buttons]\nlt = \"layer:nope\"").is_err());
    }
}
