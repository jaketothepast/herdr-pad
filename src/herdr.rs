//! Talks to the Herdr window (`herdr-app`) and herdr-core (`hc`) through their CLIs.
//! Each call is a few milliseconds, which is fine at button-press rates.

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const BUNDLE_ID: &str = "dev.herdr.app";
pub const MOD_NAME: &str = "controller";

#[derive(Debug, Default, Deserialize)]
pub struct Status {
    pub space: Option<String>,
    pub tab: Option<String>,
    pub focused_pane: Option<String>,
    #[serde(default)]
    pub overlays: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Ls {
    #[serde(default)]
    pub spaces: Vec<Space>,
    /// Spaces closed lately, newest first.
    #[serde(default)]
    pub recent: Vec<Recent>,
}

#[derive(Debug, Deserialize)]
pub struct Recent {
    pub folder: String,
}

#[derive(Debug, Deserialize)]
pub struct Space {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub tabs: Vec<Tab>,
}

#[derive(Debug, Deserialize)]
pub struct Tab {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub terminals: Vec<Term>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Term {
    pub terminal: String,
    pub pane: Option<String>,
    pub status: Option<String>,
    pub name: Option<String>,
}

/// A terminal with where it lives, flattened out of `ls`.
#[derive(Debug, Clone)]
pub struct Placed {
    pub term: Term,
    pub space_name: String,
    pub tab_name: String,
}

impl Ls {
    pub fn terminals(&self) -> Vec<Placed> {
        let mut out = vec![];
        for s in &self.spaces {
            for t in &s.tabs {
                for term in &t.terminals {
                    out.push(Placed { term: term.clone(), space_name: s.name.clone(), tab_name: t.name.clone() });
                }
            }
        }
        out
    }

    pub fn by_pane(&self, pane: &str) -> Option<Placed> {
        self.terminals().into_iter().find(|p| p.term.pane.as_deref() == Some(pane))
    }
}

pub struct Herdr {
    app: PathBuf,
    hc: PathBuf,
    front_cache: Option<(Instant, bool)>,
}

impl Herdr {
    pub fn locate() -> Result<Herdr> {
        let app = find_program("herdr-app", &["/Applications/Herdr.app/Contents/MacOS/Herdr", "~/Applications/Herdr.app/Contents/MacOS/Herdr"])
            .ok_or_else(|| anyhow!("can't find herdr-app; open Herdr once so it links ~/.local/bin/herdr-app"))?;
        let hc = find_program("hc", &[]).ok_or_else(|| anyhow!("can't find herdr-core (hc)"))?;
        Ok(Herdr { app, hc, front_cache: None })
    }

    pub fn app_path(&self) -> &Path {
        &self.app
    }

    /// Runs an app command (`herdr-app do <name> <json>`), returning its JSON reply.
    pub fn run(&self, name: &str, args: &Value) -> Result<Value> {
        let mut cmd = Command::new(&self.app);
        cmd.arg("do").arg(name);
        if args.as_object().is_some_and(|o| !o.is_empty()) {
            cmd.arg(args.to_string());
        }
        let v = json_out(cmd).with_context(|| format!("herdr-app do {name}"))?;
        if let Some(e) = v.get("error") {
            bail!("{name}: {}", e.as_str().unwrap_or("failed"));
        }
        Ok(v)
    }

    pub fn status(&self) -> Result<Status> {
        let mut cmd = Command::new(&self.app);
        cmd.arg("status");
        Ok(serde_json::from_value(json_out(cmd)?)?)
    }

    pub fn ls(&self) -> Result<Ls> {
        let mut cmd = Command::new(&self.app);
        cmd.arg("ls");
        Ok(serde_json::from_value(json_out(cmd)?)?)
    }

    /// Presses named keys in a terminal (`hc keys t3 enter`).
    pub fn keys(&self, terminal: &str, keys: &[String]) -> Result<()> {
        let out = Command::new(&self.hc).arg("keys").arg(terminal).args(keys).stdin(Stdio::null()).output()?;
        if !out.status.success() {
            bail!("hc keys: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    }

    /// Is the Herdr window the frontmost app? Cached briefly; a failed check counts as yes
    /// so a broken `lsappinfo` never makes the controller dead.
    pub fn frontmost(&mut self) -> bool {
        if let Some((at, v)) = self.front_cache {
            if at.elapsed() < Duration::from_millis(250) {
                return v;
            }
        }
        let v = frontmost_bundle().map(|b| b == BUNDLE_ID).unwrap_or(true);
        self.front_cache = Some((Instant::now(), v));
        v
    }
}

fn frontmost_bundle() -> Option<String> {
    let asn = Command::new("lsappinfo").arg("front").output().ok()?;
    let asn = String::from_utf8_lossy(&asn.stdout).trim().to_string();
    if asn.is_empty() {
        return None;
    }
    let info = Command::new("lsappinfo").args(["info", "-only", "bundleid", &asn]).output().ok()?;
    let s = String::from_utf8_lossy(&info.stdout);
    // "CFBundleIdentifier"="dev.herdr.app"
    s.split('=').nth(1).map(|v| v.trim().trim_matches('"').to_string())
}

fn json_out(mut cmd: Command) -> Result<Value> {
    let out = cmd.stdin(Stdio::null()).output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(text.trim()).map_err(|_| {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow!("{}", if err.trim().is_empty() { text.trim().to_string() } else { err.trim().to_string() })
    })?;
    Ok(v)
}

/// Finds a program on PATH, then where Herdr installs things (launchd gives a bare PATH).
pub fn find_program(name: &str, extra: &[&str]) -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    for d in ["~/.local/bin", "/opt/homebrew/bin", "/usr/local/bin", "~/.cargo/bin"] {
        dirs.push(PathBuf::from(d.replacen('~', &home, 1)));
    }
    dirs.into_iter()
        .map(|d| d.join(name))
        .chain(extra.iter().map(|p| PathBuf::from(p.replacen('~', &home, 1))))
        .find(|p| p.is_file())
}
