//! overdeck: a plugin host for decks.
//!
//!   overdeck list   <bundle.clap>                 what a bundle offers, and its parameters
//!   overdeck run    <overdeck.toml> [--seconds N] decks to the sound card
//!   overdeck render <overdeck.toml> --seconds N --out DIR
//!                                                 decks to WAV (mix + one stem a deck),
//!                                                 paced at real time, as the decks expect
//!
//! The main thread owns the plugins (CLAP's main thread) and the hardware:
//! it polls the surface drivers, maps ODP to each deck, and runs the
//! plugins' main-thread callbacks.  The audio thread owns the processors.

mod config;
mod deck;
mod engine;

use anyhow::{bail, Context, Result};
use config::Config;
use deck::{Bundles, Deck};
use engine::{Engine, Route, Sender};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use surface::Driver;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,wgpu=warn,wgpu_core=warn,wgpu_hal=warn,naga=warn")).init();
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    match args.get(1).map(String::as_str) {
        Some("list") => list(Path::new(args.get(2).context("list <bundle.clap>")?)),
        Some("run") => {
            let secs = flag("--seconds").map(|s| s.parse::<f64>()).transpose()?;
            run(&Config::load(Path::new(args.get(2).context("run <overdeck.toml>")?))?, secs)
        }
        Some("render") => {
            let secs: f64 = flag("--seconds").context("render needs --seconds")?.parse()?;
            let out = PathBuf::from(flag("--out").context("render needs --out DIR")?);
            render(&Config::load(Path::new(args.get(2).context("render <overdeck.toml>")?))?, secs, &out)
        }
        _ => { eprintln!("{}", include_str!("main.rs").lines().skip(2).take(6).map(|l| l.trim_start_matches("//!")).collect::<Vec<_>>().join("\n")); bail!("no command") }
    }
}

fn list(path: &Path) -> Result<()> {
    let b = Arc::new(clap_host::Bundle::load(path)?);
    for d in b.descriptors() {
        println!("{}  {}  {}", d.id, d.name, d.version);
        let inst = b.instantiate(Some(&d.id))?;
        for p in inst.params() {
            println!("    param {:>10}  {:<16} {} .. {} (default {})", p.id, p.name, p.min, p.max, p.default);
        }
    }
    Ok(())
}

/// Make every deck (on this thread, the plugins' main thread).
fn decks(cfg: &Config) -> Result<Vec<Deck>> {
    let mut bundles = Bundles::default();
    cfg.decks.iter().enumerate().map(|(i, d)| {
        let deck = Deck::new(&mut bundles, d.clone(), cfg.sample_rate, cfg.block).with_context(|| format!("deck {i}"))?;
        log::info!("overdeck: deck {i} up ({}{})", d.plugin, d.track.as_deref().map(|t| format!(", {t}")).unwrap_or_default());
        Ok(deck)
    }).collect()
}

fn engine(cfg: &Config, decks: &[Deck]) -> (Engine, Sender) {
    let procs = decks.iter().map(|d| d.inst.processor()).collect();
    let routes = decks.iter().map(|d| Route { out: d.cfg.out, gain: d.cfg.gain }).collect();
    Engine::new(procs, routes, cfg.channels(), cfg.block as usize * 4)
}

fn drivers(cfg: &Config) -> Result<Vec<Box<dyn Driver>>> {
    let steps = cfg.steps.iter().map(|s| s.input()).collect::<Result<Vec<_>>>()?;
    Ok(vec![Box::new(surface::Script::new("script", steps))])
}

/// The main thread's loop until `done`: hardware → ODP → decks; plugin idle.
fn main_loop(decks: &mut [Deck], drivers: &mut [Box<dyn Driver>], tx: &mut Sender, done: &AtomicBool) {
    let t0 = Instant::now();
    let mut inputs = Vec::new();
    let mut evs = Vec::new();
    while !done.load(Ordering::Acquire) {
        let now = t0.elapsed();
        inputs.clear();
        for d in drivers.iter_mut() { d.poll(now, &mut inputs); }
        for i in &inputs {
            let Some(deck) = decks.get_mut(i.deck as usize) else { continue };
            log::info!("overdeck: {:6.2} s deck {} {:?}", now.as_secs_f64(), i.deck, i.control);
            evs.clear();
            deck.adapter.map(i.control, &mut evs);
            for e in &evs { tx.send(i.deck as usize, *e); }
        }
        for d in decks.iter() { d.inst.idle(); }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn run(cfg: &Config, secs: Option<f64>) -> Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let mut decks = decks(cfg)?;
    let (mut eng, mut tx) = engine(cfg, &decks);
    let device = cpal::default_host().default_output_device().context("no audio output device")?;
    let sc = cpal::StreamConfig {
        channels: eng.channels() as u16,
        sample_rate: cpal::SampleRate(cfg.sample_rate),
        buffer_size: cpal::BufferSize::Fixed(cfg.block),
    };
    log::info!("overdeck: {} channels at {} Hz, {}-frame blocks on {}", sc.channels, cfg.sample_rate, cfg.block, device.name().unwrap_or_default());
    let stream = device.build_output_stream(&sc, move |out: &mut [f32], _| eng.render(out), |e| log::error!("audio: {e}"), None)?;
    stream.play()?;
    let done = Arc::new(AtomicBool::new(false));
    if let Some(s) = secs {
        let d = Arc::clone(&done);
        std::thread::spawn(move || { std::thread::sleep(Duration::from_secs_f64(s)); d.store(true, Ordering::Release) });
    }
    main_loop(&mut decks, &mut drivers(cfg)?, &mut tx, &done);
    drop(stream);   // the processors go with the stream, before the instances
    Ok(())
}

fn render(cfg: &Config, secs: f64, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut decks = decks(cfg)?;
    let (mut eng, mut tx) = engine(cfg, &decks);
    let done = Arc::new(AtomicBool::new(false));
    let (rate, block, n) = (cfg.sample_rate, cfg.block as usize, decks.len());
    let total = (secs * f64::from(rate)) as usize;
    let dir2 = dir.to_owned();
    let d2 = Arc::clone(&done);
    let audio = std::thread::Builder::new().name("overdeck-audio".into()).spawn(move || -> Result<()> {
        // However this ends, let the main loop go.
        struct Done(Arc<AtomicBool>);
        impl Drop for Done { fn drop(&mut self) { self.0.store(true, Ordering::Release) } }
        let _done = Done(d2);
        let spec = |ch| hound::WavSpec { channels: ch, sample_rate: rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut mix = hound::WavWriter::create(dir2.join("mix.wav"), spec(eng.channels() as u16))?;
        let mut stems = (0..n).map(|i| hound::WavWriter::create(dir2.join(format!("deck{i}.wav")), spec(2))).collect::<Result<Vec<_>, _>>()?;
        let mut buf = vec![0.0f32; block * eng.channels()];
        let t0 = Instant::now();
        let mut done_frames = 0;
        while done_frames < total {
            eng.render(&mut buf);
            for s in &buf { mix.write_sample(*s)?; }
            for (i, w) in stems.iter_mut().enumerate() {
                let (l, r) = &eng.last[i];
                for f in 0..block { w.write_sample(l[f])?; w.write_sample(r[f])?; }
            }
            done_frames += block;
            // Real time: the decks' engines fill their output at the speed
            // of sound, not of this loop.
            let due = Duration::from_secs_f64(done_frames as f64 / f64::from(rate));
            if let Some(wait) = due.checked_sub(t0.elapsed()) { std::thread::sleep(wait); }
        }
        mix.finalize()?;
        for w in stems { w.finalize()?; }
        drop(eng);   // the processors stop on this thread, as CLAP wants
        Ok(())
    })?;
    main_loop(&mut decks, &mut drivers(cfg)?, &mut tx, &done);
    audio.join().map_err(|_| anyhow::anyhow!("audio thread panicked"))??;
    log::info!("overdeck: wrote {}", dir.display());
    Ok(())
}
