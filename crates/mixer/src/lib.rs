//! Overdeck Mixer: a reference two-channel DJ mixer, as a CLAP / VST3
//! plugin, for testing decks (in Overdeck, or in any DAW).
//!
//! Inputs: deck A on the main input, deck B on the aux input (stereo each).
//! Per channel: Trim (±12 dB), a three-band isolator EQ (Low / Mid / High,
//! −26 dB = kill .. +6 dB), and a channel Fader.  Then a constant-power
//! Crossfader and a Master level.  Every gain is smoothed (10 ms), so
//! automation doesn't zipper.
//!
//! This lives in Overdeck, not in freedj: freedj is a deck, and a mixer is
//! the host's business.  It's a test instrument, not a product.

pub mod dsp;

use dsp::{crossfader, db_to_gain, fader, Isolator, KILL_DB};
use nih_plug::prelude::*;
use std::num::NonZeroU32;
use std::sync::Arc;

#[derive(Params)]
struct Channel {
    #[id = "trim"]
    trim: FloatParam,
    #[id = "low"]
    low: FloatParam,
    #[id = "mid"]
    mid: FloatParam,
    #[id = "high"]
    high: FloatParam,
    #[id = "fader"]
    fader: FloatParam,
}

fn eq(name: &str) -> FloatParam {
    FloatParam::new(name, 0.0, FloatRange::Linear { min: KILL_DB, max: 6.0 })
        .with_step_size(0.1)
        .with_unit(" dB")
        .with_smoother(SmoothingStyle::Linear(10.0))
        .with_value_to_string(Arc::new(|v| if v <= KILL_DB { "kill".into() } else { format!("{v:+.1}") }))
}

impl Channel {
    fn new(ch: &str) -> Channel {
        Channel {
            trim: FloatParam::new(format!("{ch} Trim"), 0.0, FloatRange::Linear { min: -12.0, max: 12.0 })
                .with_step_size(0.1).with_unit(" dB").with_smoother(SmoothingStyle::Linear(10.0)),
            low: eq(&format!("{ch} Low")),
            mid: eq(&format!("{ch} Mid")),
            high: eq(&format!("{ch} High")),
            fader: FloatParam::new(format!("{ch} Fader"), 1.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_smoother(SmoothingStyle::Linear(10.0)),
        }
    }
}

#[derive(Params)]
struct MixerParams {
    #[nested(id_prefix = "a", group = "A")]
    a: Channel,
    #[nested(id_prefix = "b", group = "B")]
    b: Channel,
    #[id = "xfade"]
    crossfader: FloatParam,
    #[id = "master"]
    master: FloatParam,
}

impl Default for MixerParams {
    fn default() -> Self {
        MixerParams {
            a: Channel::new("A"),
            b: Channel::new("B"),
            crossfader: FloatParam::new("Crossfader", 0.5, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_smoother(SmoothingStyle::Linear(10.0)),
            master: FloatParam::new("Master", 0.0, FloatRange::Linear { min: -24.0, max: 6.0 })
                .with_step_size(0.1).with_unit(" dB").with_smoother(SmoothingStyle::Linear(10.0)),
        }
    }
}

pub struct Mixer {
    params: Arc<MixerParams>,
    /// [channel][left/right]
    iso: [[Isolator; 2]; 2],
}

impl Default for Mixer {
    fn default() -> Self { Mixer { params: Arc::new(MixerParams::default()), iso: [[Isolator::new(48_000.0); 2]; 2] } }
}

/// One channel's gains for this sample (advancing its smoothers).
fn gains(c: &Channel) -> (f32, f32, f32, f32) {
    let pre = db_to_gain(c.trim.smoothed.next()) * fader(c.fader.smoothed.next());
    (pre, db_to_gain(c.low.smoothed.next()), db_to_gain(c.mid.smoothed.next()), db_to_gain(c.high.smoothed.next()))
}

impl Plugin for Mixer {
    const NAME: &'static str = "Overdeck Mixer";
    const VENDOR: &'static str = "Audiodestrukt";
    const URL: &'static str = "https://github.com/audiodestrukt/freedj-overdeck";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        aux_input_ports: &[new_nonzero_u32(2)],
        names: PortNames { main_input: Some("Deck A"), aux_inputs: &["Deck B"], ..PortNames::const_default() },
        ..AudioIOLayout::const_default()
    }];

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> { self.params.clone() }

    fn initialize(&mut self, _: &AudioIOLayout, cfg: &BufferConfig, _: &mut impl InitContext<Self>) -> bool {
        self.iso = [[Isolator::new(cfg.sample_rate); 2]; 2];
        true
    }

    fn process(&mut self, buffer: &mut Buffer, aux: &mut AuxiliaryBuffers, _: &mut impl ProcessContext<Self>) -> ProcessStatus {
        let p = &self.params;
        let b_in = aux.inputs.first_mut().map(|b| b.as_slice_immutable());
        let main = buffer.as_slice();
        let frames = main.first().map_or(0, |c| c.len());
        for i in 0..frames {
            let (pa, la, ma, ha) = gains(&p.a);
            let (pb, lb, mb, hb) = gains(&p.b);
            let (xa, xb) = crossfader(p.crossfader.smoothed.next());
            let master = db_to_gain(p.master.smoothed.next());
            for ch in 0..main.len().min(2) {
                let a = main[ch][i];
                let b = b_in.as_ref().and_then(|s| s.get(ch)).map_or(0.0, |c| c[i]);
                let a = self.iso[0][ch].run(a * pa, la, ma, ha) * xa;
                let b = self.iso[1][ch].run(b * pb, lb, mb, hb) * xb;
                main[ch][i] = (a + b) * master;
            }
        }
        ProcessStatus::Normal
    }
}

impl ClapPlugin for Mixer {
    const CLAP_ID: &'static str = "com.audiodestrukt.overdeck-mixer";
    const CLAP_DESCRIPTION: Option<&'static str> = Some("A reference two-channel DJ mixer for testing decks");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[ClapFeature::AudioEffect, ClapFeature::Mixing, ClapFeature::Stereo];
}

impl Vst3Plugin for Mixer {
    const VST3_CLASS_ID: [u8; 16] = *b"OverdeckMixer001";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[Vst3SubCategory::Fx, Vst3SubCategory::Tools];
}

nih_export_clap!(Mixer);
nih_export_vst3!(Mixer);
