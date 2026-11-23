//! A deck in the host: a plugin instance, and the adapter from ODP to it.
//!
//! Today's freedj-3000 plugin takes its controls as CLAP parameters (Play,
//! Cue, Platter touch, Pitch, Platter) and its track in its saved state, so
//! [`ParamAdapter`] turns ODP into those.  When the plugin grows the
//! `com.audiodestrukt.odp` extension, ODP goes straight in and every key
//! (hot cues, loops, browse) reaches the deck; the adapter stays as the path
//! for plugins without it.

use crate::config::{expand, DeckCfg};
use anyhow::{anyhow, Context, Result};
use clap_host::{Bundle, Instance, ParamEvent};
use odp::{Control, JOG_SECS_PER_TICK};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

pub struct Deck {
    pub cfg: DeckCfg,
    pub inst: Instance,
    pub adapter: ParamAdapter,
}

/// Open each bundle once, however many decks use it.
#[derive(Default)]
pub struct Bundles(HashMap<String, Arc<Bundle>>);

impl Bundles {
    pub fn get(&mut self, path: &str) -> Result<Arc<Bundle>> {
        let p = expand(path);
        let key = p.to_string_lossy().into_owned();
        if let Some(b) = self.0.get(&key) { return Ok(Arc::clone(b)); }
        let b = Arc::new(Bundle::load(&p)?);
        self.0.insert(key, Arc::clone(&b));
        Ok(b)
    }
}

impl Deck {
    /// Make the instance, give it its track, and activate it.
    pub fn new(bundles: &mut Bundles, cfg: DeckCfg, sample_rate: u32, block: u32) -> Result<Deck> {
        let bundle = bundles.get(&cfg.plugin)?;
        let mut inst = bundle.instantiate(cfg.id.as_deref())?;
        if let Some(n) = cfg.follow {
            if !(1..=6).contains(&n) { return Err(anyhow!("follow = {n}: a Link player number is 1-6")); }
            set_field(&inst, "follow", n.to_string()).context("make the deck a mirror")?;
        } else if let Some(t) = &cfg.track {
            set_track(&inst, &expand(t)).with_context(|| format!("give the deck its track {t}"))?;
        }
        inst.activate(f64::from(sample_rate), 1, block.max(4096))?;
        let adapter = ParamAdapter::new(&inst);
        Ok(Deck { cfg, inst, adapter })
    }
}

/// freedj saves its track as a field of its nih-plug state:
/// `{"fields":{"track":"\"/music/a.mp3\""}}` (the value is itself JSON).
/// nih-plug frames the JSON with its length (u64, little-endian) in front.
/// Write the track there and load the state back: the plugin loads it.
pub fn set_track(inst: &Instance, path: &Path) -> Result<()> {
    if !path.is_file() { return Err(anyhow!("{} is not a file", path.display())); }
    set_field(inst, "track", serde_json::to_string(&path.to_string_lossy())?)
}

/// Write one field of freedj's saved state (`value` is the field's JSON:
/// `"\"/music/a.mp3\""` for the track, `"2"` for FOLLOW's player) and load
/// the state back.
pub fn set_field(inst: &Instance, name: &str, value: String) -> Result<()> {
    inst.load_state(&with_field(&inst.save_state()?, name, value)?)
}

fn with_field(bytes: &[u8], name: &str, value: String) -> Result<Vec<u8>> {
    let (json, framed) = unframe(bytes);
    let mut st: serde_json::Value = serde_json::from_slice(json)
        .with_context(|| format!("the plugin's state isn't JSON (not a freedj deck?): {} bytes starting {:02x?}", bytes.len(), &bytes[..bytes.len().min(16)]))?;
    let fields = st.as_object_mut().ok_or_else(|| anyhow!("state is not an object"))?
        .entry("fields").or_insert_with(|| serde_json::json!({}));
    fields[name] = serde_json::Value::String(value);
    let json = serde_json::to_vec(&st)?;
    let mut out = Vec::with_capacity(json.len() + 8);
    if framed { out.extend_from_slice(&(json.len() as u64).to_le_bytes()); }
    out.extend_from_slice(&json);
    Ok(out)
}

/// The JSON in a nih-plug state, and whether it came length-framed.
fn unframe(b: &[u8]) -> (&[u8], bool) {
    if b.len() >= 8 {
        let n = u64::from_le_bytes(b[..8].try_into().unwrap()) as usize;
        if n == b.len() - 8 { return (&b[8..], true); }
    }
    (b, false)
}

/// ODP → the plugin's parameters, normalized (CLAP values are the
/// parameter's plain range).
pub struct ParamAdapter {
    play: Option<u32>,
    cue: Option<u32>,
    touch: Option<u32>,
    pitch: Option<Linear>,
    platter: Option<Linear>,
    /// Where the Platter parameter is, in seconds of record.
    platter_at: f64,
}

impl ParamAdapter {
    pub fn new(inst: &Instance) -> ParamAdapter {
        let id = |n: &str| inst.param(n).map(|p| p.id);
        let ranged = |n: &str| inst.param(n).and_then(|p| Linear::probe(inst, p));
        let a = ParamAdapter {
            play: id("Play"), cue: id("Cue"), touch: id("Platter touch"),
            pitch: ranged("Pitch"), platter: ranged("Platter"), platter_at: 0.0,
        };
        if a.play.is_none() {
            log::warn!("deck: no Play parameter; params are {:?}", inst.params().iter().map(|p| &p.name).collect::<Vec<_>>());
        }
        a
    }

    /// The parameter events for one control, appended to `out`.
    pub fn map(&mut self, c: Control, out: &mut Vec<ParamEvent>) {
        let b = |v: bool| if v { 1.0 } else { 0.0 };
        let ev = |id, value| ParamEvent { frame: 0, id, value };
        match c {
            Control::Play(v) => if let Some(id) = self.play { out.push(ev(id, b(v))) },
            Control::Cue(v) => if let Some(id) = self.cue { out.push(ev(id, b(v))) },
            Control::JogTouch(v) => if let Some(id) = self.touch { out.push(ev(id, b(v))) },
            Control::Pitch(p) => if let Some(l) = &self.pitch { out.push(ev(l.id, l.value(f64::from(p)))) },
            Control::Jog(t) => if let Some(l) = &self.platter {
                let (lo, hi) = l.plain_range();
                // The plugin turns the jog by however far Platter moves.  At
                // its range's end (±8 s of record) Platter can't move on, so
                // it snaps back to 0, which the plugin hears as one big turn
                // back: a known limit of the parameter path, gone with the
                // ODP extension (which sends ticks, not a position).
                let step = f64::from(t) * JOG_SECS_PER_TICK;
                if !(lo..=hi).contains(&(self.platter_at + step)) {
                    self.platter_at = 0.0;
                    log::debug!("deck: Platter recentred");
                }
                self.platter_at += step;
                out.push(ev(l.id, l.value(self.platter_at)));
            },
            Control::Key(k, _) => log::debug!("deck: {k:?} needs the ODP extension; not sent"),
        }
    }
}

/// A linear parameter's plain ↔ CLAP value mapping, learned from the plugin
/// (nih-plug sends CLAP values normalized to 0..1, whatever the plain range).
pub struct Linear {
    id: u32,
    /// CLAP value = a + b × plain.
    a: f64,
    b: f64,
    min: f64,
    max: f64,
}

impl Linear {
    fn probe(inst: &Instance, p: &clap_host::Param) -> Option<Linear> {
        let v0 = inst.text_to_value(p.id, "0")?;
        let v1 = inst.text_to_value(p.id, "1")?;
        let b = v1 - v0;
        if b.abs() < 1e-12 { return None; }
        Some(Linear { id: p.id, a: v0, b, min: p.min, max: p.max })
    }
    /// The CLAP value for a plain value, clamped to the parameter's range.
    fn value(&self, plain: f64) -> f64 { (self.a + self.b * plain).clamp(self.min, self.max) }
    /// The plain values at the ends of the range.
    fn plain_range(&self) -> (f64, f64) {
        let (x, y) = ((self.min - self.a) / self.b, (self.max - self.a) / self.b);
        (x.min(y), x.max(y))
    }
}

#[cfg(test)]
mod tests {
    use super::{unframe, with_field, Linear};

    /// `follow = 2` reaches freedj as `"fields":{"follow":"2"}` (its saved
    /// FOLLOW field), beside whatever else the state holds, framed as it came.
    #[test]
    fn a_mirror_deck_gets_its_player_in_the_saved_state() {
        let json = br#"{"version":"0.1.0","params":{},"fields":{"track":"\"\""}}"#;
        let mut b = (json.len() as u64).to_le_bytes().to_vec();
        b.extend_from_slice(json);
        let out = with_field(&b, "follow", "2".into()).unwrap();
        let (j, framed) = unframe(&out);
        assert!(framed);
        let v: serde_json::Value = serde_json::from_slice(j).unwrap();
        assert_eq!(v["fields"]["follow"], "2");
        assert_eq!(v["fields"]["track"], "\"\"", "the other fields stay");
    }

    #[test]
    fn nih_plug_state_is_length_framed_json() {
        let json = br#"{"version":"0.1.0","params":{},"fields":{}}"#;
        let mut b = (json.len() as u64).to_le_bytes().to_vec();
        b.extend_from_slice(json);
        assert_eq!(unframe(&b), (&json[..], true));
        assert_eq!(unframe(json), (&json[..], false), "bare JSON passes through");
    }

    #[test]
    fn a_normalized_parameter_maps_its_plain_range() {
        // freedj's Pitch: -16..16 % as 0..1.
        let l = Linear { id: 1, a: 0.5, b: 1.0 / 32.0, min: 0.0, max: 1.0 };
        assert!((l.value(6.0) - (0.5 + 6.0 / 32.0)).abs() < 1e-12);
        assert_eq!(l.value(40.0), 1.0, "clamped to the range");
        let (lo, hi) = l.plain_range();
        assert!((lo + 16.0).abs() < 1e-9 && (hi - 16.0).abs() < 1e-9);
    }
}
