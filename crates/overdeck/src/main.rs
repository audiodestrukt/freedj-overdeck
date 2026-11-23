//! overdeck: a plugin host for decks.
//!
//!   overdeck list   <bundle.clap>                 what a bundle offers, and its parameters
//!   overdeck run    <overdeck.toml> [--seconds N] [--aio]  decks to the sound card
//!                                                 (--aio: an all-in-one unit's screen)
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
mod panel;

use anyhow::{bail, Context, Result};
use config::Config;
use deck::{Bundles, Deck};
use engine::{Engine, MixerStage, Route, Sender};
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
            let mut cfg = Config::load(Path::new(args.get(2).context("run <overdeck.toml>")?))?;
            if args.iter().any(|a| a == "--aio") { cfg.panel = true; cfg.screen = Some("aio".into()); cfg.display = true; }
            if args.iter().any(|a| a == "--windows") { cfg.display = false; }
            run(&cfg, secs)
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

/// The decks and the mixer: every plugin instance, on the main thread.
struct Host {
    decks: Vec<Deck>,
    mixer: Option<clap_host::Instance>,
    panel: bool,
    screen: Option<String>,
    /// The decks' screens (the display contract), when they draw into the unit's.
    screens: Option<Screens>,
}

/// One shared-memory screen per deck, made before the decks start; the
/// decks find theirs through `OVERDECK_DISPLAY`, the unit's screen through
/// `--screens`.  Removed when the run ends.
struct Screens {
    template: String,
    made: Vec<display::Screen>,
}

impl Drop for Screens {
    fn drop(&mut self) { for s in &self.made { let _ = std::fs::remove_file(&s.path); } }
}

/// The size each deck's screen is drawn at: its tile of the unit's 1024×600
/// screen, at the deck's own shape (1024:600).  Two decks side by side with
/// the mixer under them; four in a grid.
pub fn deck_screen_size(decks: usize) -> (u32, u32) {
    if decks <= 2 { (496, 291) } else { (457, 268) }
}

fn make_screens(n: usize) -> Result<Screens> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    // A run that was killed leaves its screens behind: clear any whose Overdeck is gone.
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let pid = name.strip_prefix("overdeck-").and_then(|r| r.split_once("-deck")).map(|(p, _)| p.to_owned());
            if let (true, Some(pid)) = (name.ends_with(".screen"), pid) {
                if !Path::new("/proc").join(&pid).exists() { let _ = std::fs::remove_file(e.path()); }
            }
        }
    }
    let template = dir.join(format!("overdeck-{}-deck{{n}}.screen", std::process::id())).to_string_lossy().into_owned();
    let (w, h) = deck_screen_size(n);
    let made = (1..=n as u32).map(|i| display::Screen::create(&display::Screen::path_for(&template, i), w, h)).collect::<Result<Vec<_>, _>>()?;
    log::info!("overdeck: {n} deck screen(s) of {w}x{h} ({template})");
    Ok(Screens { template, made })
}

fn host(cfg: &Config) -> Result<Host> {
    // The decks draw into the unit's screen: make theirs before they start.
    let screens = if cfg.display { Some(make_screens(cfg.decks.len())?) } else { None };
    match &screens {
        Some(s) => std::env::set_var("OVERDECK_DISPLAY", &s.template),
        None => std::env::remove_var("OVERDECK_DISPLAY"),
    }
    if cfg.dock && screens.is_none() {
        // Before any deck starts its window system connection.
        std::env::set_var("OPENDECK_TILE", cfg.decks.len().to_string());
        if std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("DISPLAY").is_some() {
            log::info!("overdeck: dock: the decks use XWayland, so their windows can be placed");
            if let Some(w) = std::env::var_os("WAYLAND_DISPLAY") { std::env::set_var("OVERDECK_WAYLAND_DISPLAY", w); }
            std::env::remove_var("WAYLAND_DISPLAY");
        }
    }
    let decks = decks(cfg)?;
    let mixer = match &cfg.mixer {
        Some(m) => {
            let b = Arc::new(clap_host::Bundle::load(&config::expand(&m.plugin))?);
            let mut inst = b.instantiate(m.id.as_deref())?;
            inst.activate(f64::from(cfg.sample_rate), 1, cfg.block.max(4096))?;
            log::info!("overdeck: mixer up ({}), decks {:?} into its inputs", m.plugin, m.inputs);
            Some(inst)
        }
        None => None,
    };
    Ok(Host { decks, mixer, panel: cfg.panel, screen: cfg.screen.clone(), screens })
}

fn engine(cfg: &Config, h: &Host) -> (Engine, Sender, Vec<rtrb::Consumer<engine::Bands>>) {
    let procs = h.decks.iter().map(|d| d.inst.processor()).collect();
    let routes = h.decks.iter().map(|d| cfg.deck_out(&d.cfg).map(|out| Route { out, gain: d.cfg.gain })).collect();
    let mixer = h.mixer.as_ref().zip(cfg.mixer.as_ref()).map(|(inst, m)| MixerStage {
        proc: inst.processor(), inputs: m.inputs.clone(), route: Route { out: m.out, gain: 1.0 },
    });
    Engine::new(procs, routes, mixer, cfg.channels(), cfg.block as usize * 4, cfg.sample_rate)
}

/// One `[[automate]]`, resolved to a plugin and a parameter.
struct Auto {
    cfg: config::Automate,
    /// The plugin: decks by index, the mixer after them.
    target: usize,
    id: u32,
    done: bool,
}

fn automation(cfg: &Config, h: &Host) -> Result<Vec<Auto>> {
    cfg.automation.iter().map(|a| {
        let (target, inst) = match a.target.as_str() {
            "mixer" => (h.decks.len(), h.mixer.as_ref().context("no mixer")?),
            t => { let d: usize = t.parse()?; (d, &h.decks[d].inst) }
        };
        let p = inst.param(&a.param).with_context(|| format!("{} has no parameter {:?} (it has {:?})", a.target, a.param,
            inst.params().iter().map(|p| &p.name).collect::<Vec<_>>()))?;
        Ok(Auto { cfg: a.clone(), target, id: p.id, done: false })
    }).collect()
}

fn drivers(cfg: &Config) -> Result<Vec<Box<dyn Driver>>> {
    let steps = cfg.steps.iter().map(|s| s.input()).collect::<Result<Vec<_>>>()?;
    Ok(vec![Box::new(surface::Script::new("script", steps))])
}

/// The main thread's loop until `done`: hardware → ODP → decks, scripted
/// parameter moves, and the plugins' main-thread callbacks.
fn main_loop(h: &mut Host, autos: &mut [Auto], drivers: &mut [Box<dyn Driver>], tx: &mut Sender, bands: &mut [rtrb::Consumer<engine::Bands>], done: &AtomicBool) {
    let mut panel = start_panel(h);
    let mut cmds = Vec::new();
    let library = library(h);
    let mut views: Vec<panel::DeckView> = h.decks.iter().map(|d| panel::DeckView {
        title: d.cfg.track.as_deref().map(track_title).unwrap_or_else(|| "(no track)".into()),
        ..Default::default()
    }).collect();
    let mut last_tick = Instant::now();
    let t0 = Instant::now();
    let mut inputs = Vec::new();
    let mut evs = Vec::new();
    let mut last: std::collections::HashMap<(usize, u32), f64> = Default::default();
    let decks = &mut h.decks;
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
        // Scripted parameter moves, in the parameter's own units.
        let t = now.as_secs_f64();
        for a in autos.iter_mut().filter(|a| !a.done && t >= a.cfg.at) {
            let c = &a.cfg;
            let plain = match c.to {
                Some(end) if end > c.at && t < end => {
                    let from = c.from.or_else(|| last.get(&(a.target, a.id)).copied()).unwrap_or(c.value);
                    from + (c.value - from) * (t - c.at) / (end - c.at)
                }
                _ => { a.done = true; c.value }
            };
            if a.done { log::info!("overdeck: {t:6.2} s {} {} = {}", c.target, c.param, c.value); }
            last.insert((a.target, a.id), plain);
            let inst = if a.target < decks.len() { &decks[a.target].inst } else { h.mixer.as_ref().unwrap() };
            match inst.text_to_value(a.id, &format!("{plain}")) {
                Some(v) => tx.send(a.target, clap_host::ParamEvent { frame: 0, id: a.id, value: v }),
                None => { log::warn!("overdeck: {} won't take {plain} for {}", c.target, c.param); a.done = true; }
            }
        }
        // What the screen shows of each deck.
        let dt = last_tick.elapsed().as_secs_f64();
        last_tick = Instant::now();
        for (i, v) in views.iter_mut().enumerate() {
            while let Ok(b) = bands[i].pop() { v.cols.push(b); }
            let inst = &decks[i].inst;
            v.playing = inst.param("Play").and_then(|p| inst.get_value(p.id)).is_some_and(|x| x > 0.5);
            if let Some(p) = inst.param("Pitch") {
                if let Some(x) = inst.get_value(p.id) { v.pitch = inst.value_to_text(p.id, x).unwrap_or_default(); }
            }
            let pct: f64 = v.pitch.trim().trim_end_matches('%').trim().parse().unwrap_or(0.0);
            if v.playing { v.elapsed += dt * (1.0 + pct / 100.0); }
        }
        if let Some(p) = &mut panel {
            cmds.clear();
            p.poll(h.mixer.as_ref(), &mut views, &library, &mut cmds);
            // The unit's screen is the app: closing it ends the run.
            if p.ended && h.screen.as_deref() == Some("aio") { log::info!("overdeck: the screen closed; stopping"); break; }
            for c in cmds.drain(..) {
                match c {
                    panel::Cmd::Set(id, value) => tx.send(decks.len(), clap_host::ParamEvent { frame: 0, id, value }),
                    panel::Cmd::Deck(d, control) => if let Some(deck) = decks.get_mut(d as usize) {
                        log::info!("overdeck: screen: deck {d} {control:?}");
                        if matches!(control, odp::Control::Cue(true)) { views[d as usize].elapsed = 0.0; }
                        evs.clear();
                        deck.adapter.map(control, &mut evs);
                        for e in &evs { tx.send(d as usize, *e); }
                    },
                    panel::Cmd::Load(d, path) => if let Some(deck) = decks.get(d as usize) {
                        match deck::set_track(&deck.inst, &path) {
                            Ok(()) => {
                                log::info!("overdeck: screen: deck {d} loads {}", path.display());
                                let v = &mut views[d as usize];
                                v.title = track_title(&path.to_string_lossy());
                                v.elapsed = 0.0;
                            }
                            Err(e) => log::warn!("overdeck: deck {d} can't load {}: {e:#}", path.display()),
                        }
                    },
                }
            }
        } else {
            for v in views.iter_mut() { v.cols.clear(); }
        }
        for d in decks.iter() { d.inst.idle(); }
        if let Some(m) = &h.mixer { m.idle(); }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The mixer's panel, if asked for: listen, and start `overdeck-panel`.
fn start_panel(h: &Host) -> Option<panel::Panel> {
    let aio = h.screen.as_deref() == Some("aio");
    if !h.panel || (h.mixer.is_none() && !aio) { return None; }
    let p = match panel::Panel::listen() {
        Ok(p) => p,
        Err(e) => { log::warn!("panel: can't listen: {e}"); return None; }
    };
    let exe = std::env::current_exe().ok().and_then(|e| Some(e.parent()?.join("overdeck-panel")));
    let mut args = vec![p.path.to_string_lossy().into_owned()];
    if aio { args.extend(["--screen".into(), "aio".into(), "--decks".into(), h.decks.len().to_string()]); }
    if let Some(s) = &h.screens { args.extend(["--screens".into(), s.template.clone()]); }
    let spawn = |e: std::path::PathBuf| {
        let mut c = std::process::Command::new(e);
        c.args(&args);
        // The unit's screen places nothing, so it keeps native Wayland (smoother frame pacing than XWayland).
        if aio { if let Some(w) = std::env::var_os("OVERDECK_WAYLAND_DISPLAY") { c.env("WAYLAND_DISPLAY", w); } }
        c.spawn()
    };
    match exe.map(spawn) {
        Some(Ok(_)) => log::info!("panel: started ({})", p.path.display()),
        _ => log::warn!("panel: couldn't start overdeck-panel (build it: cargo build --release -p overdeck-panel); connect one to {}", p.path.display()),
    }
    Some(p)
}

/// A track's name for the screen: its file name without the extension.
fn track_title(path: &str) -> String {
    let p = config::expand(path);
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_owned())
}

/// The tracks the screen can browse: audio files beside the first deck's
/// track (a stand-in for the library service the host will run).
fn library(h: &Host) -> Vec<(String, String)> {
    let Some(dir) = h.decks.iter().find_map(|d| d.cfg.track.as_deref()).and_then(|t| config::expand(t).parent().map(Path::to_path_buf)) else { return Vec::new() };
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir).into_iter().flatten().flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| ["mp3", "wav", "flac", "m4a", "aif", "aiff", "ogg"].contains(&e.to_ascii_lowercase().as_str())))
        .map(|p| (p.to_string_lossy().into_owned(), track_title(&p.to_string_lossy())))
        .collect();
    out.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    out.truncate(300);
    out
}

fn run(cfg: &Config, secs: Option<f64>) -> Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let mut h = host(cfg)?;
    let mut autos = automation(cfg, &h)?;
    let (mut eng, mut tx, mut bands) = engine(cfg, &h);
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
    main_loop(&mut h, &mut autos, &mut drivers(cfg)?, &mut tx, &mut bands, &done);
    drop(stream);   // the processors go with the stream, before the instances
    Ok(())
}

fn render(cfg: &Config, secs: f64, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut h = host(cfg)?;
    let mut autos = automation(cfg, &h)?;
    let (mut eng, mut tx, mut bands) = engine(cfg, &h);
    let done = Arc::new(AtomicBool::new(false));
    let (rate, block, n) = (cfg.sample_rate, cfg.block as usize, h.decks.len());
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
    main_loop(&mut h, &mut autos, &mut drivers(cfg)?, &mut tx, &mut bands, &done);
    audio.join().map_err(|_| anyhow::anyhow!("audio thread panicked"))??;
    log::info!("overdeck: wrote {}", dir.display());
    Ok(())
}
