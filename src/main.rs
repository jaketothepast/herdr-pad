mod config;
mod engine;
mod herdr;
mod input;
mod keys;

use anyhow::{Context, Result, bail};
use config::Config;
use engine::{Engine, Host};
use herdr::{Herdr, Ls, MOD_NAME, Status};
use input::{Btn, Input, Pad};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};

const MOD_MAIN: &str = include_str!("../mod/main.js");
const MOD_JSON: &str = include_str!("../mod/mod.json");
const MOD_ICON: &str = include_str!("../mod/pad.svg");
const AGENT_LABEL: &str = "dev.herdr-pad";

const USAGE: &str = "herdr-pad: drive Herdr with an Xbox controller

Usage:
  herdr-pad install     put the on-screen mod in Herdr and start at login
  herdr-pad uninstall   stop, remove the login item and the mod
  herdr-pad run         run in this terminal (what the login item runs)
  herdr-pad test [--raw] print each button as you press it (--raw: every event)
  herdr-pad sim <button>[:ms] ...
                        press buttons without a controller (ms: hold that long)
  herdr-pad config      print where the config file is

Config: ~/.config/herdr-pad/config.toml (live: save and it applies)
Log:    ~/Library/Logs/herdr-pad.log";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        None | Some("run") => run(),
        Some("test") => test(args.get(1).is_some_and(|a| a == "--raw")),
        Some("sim") => sim(&args[1..]),
        Some("install") => install(),
        Some("uninstall") => uninstall(),
        Some("config") => config::ensure_file().map(|p| println!("{}", p.display())),
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(anyhow::anyhow!("unknown command \"{other}\"\n\n{USAGE}")),
    };
    if let Err(e) = r {
        eprintln!("herdr-pad: {e:#}");
        std::process::exit(1);
    }
}

fn log(msg: &str) {
    let t = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    eprintln!("[{t}] {msg}");
}

// ---- the real host ----

struct RealHost {
    herdr: Herdr,
    hud: Sender<Option<Value>>,
}

impl Host for RealHost {
    fn run(&mut self, name: &str, args: &Value) -> Result<Value> {
        self.herdr.run(name, args)
    }
    fn status(&mut self) -> Result<Status> {
        self.herdr.status()
    }
    fn ls(&mut self) -> Result<Ls> {
        self.herdr.ls()
    }
    fn keys(&mut self, terminal: &str, keys: &[String]) -> Result<()> {
        self.herdr.keys(terminal, keys)
    }
    fn frontmost(&mut self) -> bool {
        self.herdr.frontmost()
    }
    fn exec(&mut self, argv: &[String]) -> Result<()> {
        let (prog, rest) = argv.split_first().context("empty command")?;
        let prog = herdr::find_program(prog, &[]).unwrap_or_else(|| PathBuf::from(prog));
        Command::new(prog).args(rest).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
        Ok(())
    }
    fn hud(&mut self, view: Option<&Value>) {
        let _ = self.hud.send(view.cloned());
    }
    fn list_dirs(&mut self, root: &str) -> Vec<String> {
        let Ok(rd) = std::fs::read_dir(root) else { return vec![] };
        let mut dirs: Vec<(SystemTime, String)> = rd
            .filter_map(|e| e.ok())
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| (e.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH), e.path().to_string_lossy().into_owned()))
            .collect();
        dirs.sort_by(|a, b| b.0.cmp(&a.0));
        dirs.into_iter().map(|(_, p)| p).collect()
    }
    fn is_dir(&mut self, path: &str) -> bool {
        Path::new(path).is_dir()
    }
    fn hold_key(&mut self, key: &str, down: bool) -> Result<()> {
        let r = keys::hold(key, down);
        log(&format!("dictation: {key} {} (accessibility {}) → {}", if down { "down" } else { "up" },
            if keys::trusted() { "granted" } else { "NOT granted" },
            match &r { Ok(()) => "sent".to_string(), Err(e) => format!("{e:#}") }));
        r
    }
}

/// Keeps the on-screen view up to date off the input thread, sending only the newest view.
fn hud_thread(rx: Receiver<Option<Value>>) {
    let Ok(herdr) = Herdr::locate() else { return };
    let mut warned = false;
    while let Ok(mut view) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            view = newer;
        }
        let r = (|| -> Result<()> {
            let open = herdr.status()?.overlays.iter().any(|o| o == MOD_NAME);
            match &view {
                Some(v) => {
                    if !open {
                        herdr.run("mod.open", &serde_json::json!({ "name": MOD_NAME }))?;
                    }
                    herdr.run(&format!("{MOD_NAME}.state {v}"), &Value::Null)?;
                }
                None if open => {
                    herdr.run("mod.open", &serde_json::json!({ "name": MOD_NAME }))?;
                }
                None => {}
            }
            Ok(())
        })();
        match r {
            Err(e) if !warned => {
                log(&format!("on-screen view: {e:#} (is Herdr open, and the mod installed? `herdr-pad install`)"));
                warned = true;
            }
            Ok(()) => warned = false,
            _ => {}
        }
    }
}

fn host() -> Result<RealHost> {
    let herdr = Herdr::locate()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || hud_thread(rx));
    Ok(RealHost { herdr, hud: tx })
}

fn load_config(path: &Path) -> Result<Config> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Config::parse(&text).with_context(|| format!("in {}", path.display()))
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

// ---- commands ----

fn run() -> Result<()> {
    let path = config::ensure_file()?;
    let cfg = load_config(&path)?;
    let mut host = host()?;
    log(&format!("started; herdr-app at {}; config {}", host.herdr.app_path().display(), path.display()));
    if cfg.dictation.hold_key.as_deref().is_some_and(|k| !k.is_empty()) && !keys::request() {
        log("dictation: waiting for Accessibility permission (System Settings → Privacy & Security → Accessibility → herdr-pad)");
    }
    let mut pad = Pad::new()?;
    let mut engine = Engine::new(cfg);
    let mut seen = mtime(&path);
    let mut last_check = Instant::now();
    loop {
        for input in pad.poll(Duration::from_millis(15)) {
            match &input {
                Input::Connected(n) => log(&format!("controller connected: {n}")),
                Input::Disconnected => log("controller disconnected"),
                _ => {}
            }
            engine.handle(input, &mut host);
        }
        engine.tick(&mut host);
        if last_check.elapsed() > Duration::from_secs(1) {
            last_check = Instant::now();
            let now = mtime(&path);
            if now != seen {
                seen = now;
                match load_config(&path) {
                    Ok(cfg) => {
                        log("config reloaded");
                        engine.set_config(cfg, &mut host);
                    }
                    Err(e) => log(&format!("config not reloaded, keeping the old one: {e:#}")),
                }
            }
        }
    }
}

fn test(raw: bool) -> Result<()> {
    let mut pad = Pad::new()?;
    pad.raw = raw;
    println!("Press buttons (ctrl+c to stop). Each press prints its config name{}.", if raw { ", plus every raw event" } else { "" });
    let mut last_raw = Instant::now();
    loop {
        for input in pad.poll(Duration::from_millis(100)) {
            match input {
                Input::Press(b) => println!("{:<12} {}", b.name(), b.glyph()),
                Input::Connected(n) => println!("connected: {n}"),
                Input::Disconnected => println!("disconnected"),
                Input::Raw(e) => {
                    last_raw = Instant::now();
                    println!("  raw: {e}");
                }
                Input::Release(_) => {}
            }
        }
        if raw && last_raw.elapsed() > Duration::from_secs(10) {
            last_raw = Instant::now();
            println!("  (no events in 10 s: if you're pressing buttons, macOS may be blocking input; see Input Monitoring)");
        }
    }
}

/// `sim menu down a`, `sim b:600`: drives the real Herdr without a controller.
fn sim(args: &[String]) -> Result<()> {
    if args.is_empty() {
        bail!("sim needs buttons, e.g. `herdr-pad sim menu dpad_down a`");
    }
    let path = config::ensure_file()?;
    let mut cfg = load_config(&path)?;
    cfg.only_when_focused = false;
    let mut host = host()?;
    let mut engine = Engine::new(cfg);
    engine.handle(Input::Connected("sim".into()), &mut host);
    for a in args {
        let (name, ms) = a.split_once(':').map(|(n, ms)| (n, ms.parse().unwrap_or(0))).unwrap_or((a.as_str(), 0u64));
        let b = Btn::from_name(name).with_context(|| format!("unknown button \"{name}\""))?;
        engine.handle(Input::Press(b), &mut host);
        let end = Instant::now() + Duration::from_millis(ms.max(30));
        while Instant::now() < end {
            engine.tick(&mut host);
            std::thread::sleep(Duration::from_millis(10));
        }
        engine.handle(Input::Release(b), &mut host);
        // Past the A guard and Esc gap, so a scripted sequence behaves like a person.
        std::thread::sleep(Duration::from_millis(450));
        println!("{name}: {}", engine.view()["label"].as_str().unwrap_or(""));
    }
    // Let the view thread deliver the last state.
    std::thread::sleep(Duration::from_millis(200));
    Ok(())
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn app_home() -> PathBuf {
    std::env::var_os("HERDR_APP_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".config/herdr-app"))
}

fn plist_path() -> PathBuf {
    home().join(format!("Library/LaunchAgents/{AGENT_LABEL}.plist"))
}

fn install() -> Result<()> {
    // 1. The binary, somewhere stable.
    let bin_dir = home().join(".local/bin");
    std::fs::create_dir_all(&bin_dir)?;
    let bin = bin_dir.join("herdr-pad");
    let me = std::env::current_exe()?;
    if me.canonicalize().ok() != bin.canonicalize().ok() {
        let tmp = bin.with_extension("new");
        std::fs::copy(&me, &tmp)?;
        std::fs::rename(&tmp, &bin)?;
    }
    println!("binary: {}", bin.display());
    sign(&bin);

    // 2. The on-screen mod.
    let dir = app_home().join("mods").join(MOD_NAME);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("main.js"), MOD_MAIN)?;
    std::fs::write(dir.join("mod.json"), MOD_JSON)?;
    std::fs::write(dir.join("pad.svg"), MOD_ICON)?;
    println!("mod: {}", dir.display());
    if let Ok(h) = Herdr::locate() {
        let out = Command::new(h.app_path()).arg("check").arg(&dir).output()?;
        if !out.status.success() {
            bail!("the mod failed Herdr's check:\n{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        }
    }

    // 3. The config, if there isn't one yet.
    println!("config: {}", config::ensure_file()?.display());

    // 4. Start at login, and now.
    let log = home().join("Library/Logs/herdr-pad.log");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{AGENT_LABEL}</string>
  <key>ProgramArguments</key><array><string>{bin}</string><string>run</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        bin = bin.display(),
        log = log.display()
    );
    let p = plist_path();
    std::fs::create_dir_all(p.parent().unwrap())?;
    let _ = launchctl(&["bootout", &format!("gui/{}/{AGENT_LABEL}", uid())]);
    // bootout returns before the old copy is gone; bootstrapping too soon fails with error 5.
    for _ in 0..30 {
        if launchctl(&["print", &format!("gui/{}/{AGENT_LABEL}", uid())]).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::fs::write(&p, plist)?;
    launchctl(&["bootstrap", &format!("gui/{}", uid()), &p.to_string_lossy()])?;
    println!("running now, and at every login (log: {})", log.display());
    println!("\nPress View on the controller to see what every button does.");
    Ok(())
}

/// Signs with the person's Apple Development certificate, if they have one, under a fixed
/// identifier. macOS ties permissions (Accessibility for dictation) to the signature: an
/// unsigned rebuild is a new program to it and loses them; a signed one keeps them.
fn sign(bin: &Path) {
    let ids = Command::new("security").args(["find-identity", "-v", "-p", "codesigning"]).output();
    let Ok(ids) = ids else { return };
    let text = String::from_utf8_lossy(&ids.stdout);
    let Some(hash) = text.lines().find(|l| l.contains("Apple Development") || l.contains("Developer ID Application"))
        .and_then(|l| l.split_whitespace().nth(1)) else {
        println!("signing: no Apple Development certificate, so macOS may ask for Accessibility again after each update");
        return;
    };
    let out = Command::new("codesign").args(["--force", "--sign", hash, "--identifier", AGENT_LABEL]).arg(bin).output();
    match out {
        Ok(o) if o.status.success() => println!("signed: {AGENT_LABEL} (permissions survive updates)"),
        Ok(o) => println!("signing failed: {}", String::from_utf8_lossy(&o.stderr).trim()),
        Err(e) => println!("signing failed: {e}"),
    }
}

fn uninstall() -> Result<()> {
    let _ = launchctl(&["bootout", &format!("gui/{}/{AGENT_LABEL}", uid())]);
    let p = plist_path();
    if p.exists() {
        std::fs::remove_file(&p)?;
    }
    let dir = app_home().join("mods").join(MOD_NAME);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    println!("stopped and removed the login item and the mod.");
    println!("kept: {} and ~/.local/bin/herdr-pad (delete them if you like)", config::config_path().display());
    Ok(())
}

fn uid() -> String {
    let out = Command::new("id").arg("-u").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    out.unwrap_or_else(|_| "501".into())
}

fn launchctl(args: &[&str]) -> Result<()> {
    let out = Command::new("launchctl").args(args).output()?;
    if !out.status.success() {
        bail!("launchctl {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}
