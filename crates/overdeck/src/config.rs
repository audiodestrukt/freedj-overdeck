//! `overdeck.toml`: which decks, where their audio goes, which hardware.
//!
//! ```toml
//! sample_rate = 48000
//! block = 256
//!
//! [[deck]]
//! plugin = "~/.clap/OpenDeck.clap"
//! track  = "~/Music/a.mp3"
//! out    = [0, 1]            # device channels (left, right)
//!
//! [[deck]]
//! plugin = "~/.clap/OpenDeck.clap"
//! track  = "~/Music/b.mp3"
//! out    = [2, 3]
//!
//! [[step]]                   # a scripted input (tests, demos)
//! at = 4.0                   # seconds after start
//! deck = 0
//! play = true                # or cue / touch (bool), jog (ticks), pitch (%)
//! ```

use anyhow::{bail, Context, Result};
use odp::{Control, Input};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_rate")]
    pub sample_rate: u32,
    #[serde(default = "default_block")]
    pub block: u32,
    /// Put the decks' windows side by side (freedj's OPENDECK_TILE).  On a
    /// Wayland desktop the decks go through XWayland for it, since Wayland
    /// doesn't let a client place its windows.  A stopgap until the decks
    /// draw into Overdeck's own window (docs/ARCHITECTURE.md §3).
    #[serde(default)]
    pub dock: bool,
    /// Open the mixer's panel (`overdeck-panel`, beside this binary).
    #[serde(default)]
    pub panel: bool,
    #[serde(rename = "deck", default)]
    pub decks: Vec<DeckCfg>,
    #[serde(rename = "step", default)]
    pub steps: Vec<Step>,
    /// A mixer plugin the decks feed (none: each deck goes to its own `out`).
    pub mixer: Option<MixerCfg>,
    /// Scripted moves of any plugin's parameter (the mixer's, mostly).
    #[serde(rename = "automate", default)]
    pub automation: Vec<Automate>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct MixerCfg {
    pub plugin: String,
    pub id: Option<String>,
    /// The deck feeding each of the mixer's input ports, in port order.
    #[serde(default = "first_two")]
    pub inputs: Vec<usize>,
    #[serde(default = "default_out")]
    pub out: [usize; 2],
}

fn first_two() -> Vec<usize> { vec![0, 1] }

/// A parameter set (`at`) or ramped (`at` .. `to`, from `from` or wherever
/// it was) to `value`, in the parameter's own units (dB, %, 0..1).
#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct Automate {
    pub at: f64,
    pub to: Option<f64>,
    /// "mixer", or a deck's index ("0", "1", …).
    pub target: String,
    pub param: String,
    pub from: Option<f64>,
    pub value: f64,
}

fn default_rate() -> u32 { 48_000 }
fn default_block() -> u32 { 256 }

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct DeckCfg {
    pub plugin: String,
    /// The plugin's CLAP id, if the bundle has more than one.
    pub id: Option<String>,
    pub track: Option<String>,
    /// Device channels for left and right.  Default: [0, 1] with no mixer;
    /// with a mixer, none (the deck only feeds the mixer).
    pub out: Option<[usize; 2]>,
    #[serde(default = "one")]
    pub gain: f32,
}

fn default_out() -> [usize; 2] { [0, 1] }
fn one() -> f32 { 1.0 }

#[derive(Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub at: f64,
    pub deck: u8,
    pub play: Option<bool>,
    pub cue: Option<bool>,
    pub touch: Option<bool>,
    pub jog: Option<i32>,
    pub pitch: Option<f32>,
}

impl Step {
    pub fn input(&self) -> Result<(Duration, Input)> {
        let c = match (self.play, self.cue, self.touch, self.jog, self.pitch) {
            (Some(b), None, None, None, None) => Control::Play(b),
            (None, Some(b), None, None, None) => Control::Cue(b),
            (None, None, Some(b), None, None) => Control::JogTouch(b),
            (None, None, None, Some(t), None) => Control::Jog(t),
            (None, None, None, None, Some(p)) => Control::Pitch(p),
            _ => bail!("a step at {} s needs exactly one of play / cue / touch / jog / pitch", self.at),
        };
        Ok((Duration::from_secs_f64(self.at), Input { deck: self.deck, control: c }))
    }
}

pub fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest),
        None => PathBuf::from(p),
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let c: Config = toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        if c.decks.is_empty() { bail!("{}: no [[deck]]", path.display()); }
        if let Some(m) = &c.mixer {
            if m.inputs.iter().any(|&d| d >= c.decks.len()) { bail!("[mixer] inputs name a deck that isn't there"); }
            if m.inputs.len() > clap_host::MAX_INPUTS { bail!("[mixer] takes at most {} inputs", clap_host::MAX_INPUTS); }
        }
        for a in &c.automation {
            match a.target.as_str() {
                "mixer" if c.mixer.is_none() => bail!("an [[automate]] targets the mixer, but there's no [mixer]"),
                "mixer" => {}
                t => match t.parse::<usize>() {
                    Ok(d) if d < c.decks.len() => {}
                    _ => bail!("[[automate]] target {t:?}: \"mixer\" or a deck's index"),
                },
            }
        }
        for s in &c.steps {
            s.input()?;
            if s.deck as usize >= c.decks.len() { bail!("a step names deck {}, but there are {}", s.deck, c.decks.len()); }
        }
        Ok(c)
    }

    /// A deck's route to the device, if it has one.
    pub fn deck_out(&self, d: &DeckCfg) -> Option<[usize; 2]> {
        d.out.or(if self.mixer.is_none() { Some([0, 1]) } else { None })
    }

    /// Device channels the decks and mixer need.
    pub fn channels(&self) -> usize {
        self.decks.iter().filter_map(|d| self.deck_out(d)).chain(self.mixer.iter().map(|m| m.out))
            .flatten().max().map_or(2, |m| (m + 1).max(2))
    }
}
