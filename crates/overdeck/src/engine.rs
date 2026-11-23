//! The audio side: every deck's processor, run block by block, then the
//! mixer (if there is one) fed with the decks' outputs, all routed to device
//! channels.  Runs on the audio thread (a cpal callback, or the render
//! loop); takes parameter events from the main thread through one lock-free
//! ring per plugin, so it never waits on anything.

use clap_host::{ParamEvent, Processor};

pub struct Route {
    pub out: [usize; 2],
    pub gain: f32,
}

/// The mixer stage: its processor, which deck feeds each of its input
/// ports, and where its output goes.
pub struct MixerStage {
    pub proc: Processor,
    pub inputs: Vec<usize>,
    pub route: Route,
}

/// Each deck's low / mid / high level, a column per ~10 ms: the screen's
/// scrolling waveforms are drawn from what the decks actually play.
pub type Bands = [f32; 3];

/// Splits a deck's output into three bands (one-pole filters: a picture,
/// not a crossover) and sums each band's level over a column.
struct BandMeter { lp: f32, lp2: f32, hp_x: f32, hp_y: f32, acc: [f32; 3], n: u32, k_lo: f32, k_hi: f32, per: u32 }

impl BandMeter {
    fn new(rate: f32) -> BandMeter {
        let k = |hz: f32| 1.0 - (-2.0 * std::f32::consts::PI * hz / rate).exp();
        BandMeter { lp: 0.0, lp2: 0.0, hp_x: 0.0, hp_y: 0.0, acc: [0.0; 3], n: 0, k_lo: k(200.0), k_hi: (-2.0 * std::f32::consts::PI * 2500.0 / rate).exp(), per: (rate / 100.0) as u32 }
    }
    fn run(&mut self, l: &[f32], r: &[f32], tx: &mut rtrb::Producer<Bands>) {
        for (a, b) in l.iter().zip(r) {
            let x = 0.5 * (a + b);
            self.lp += self.k_lo * (x - self.lp);
            self.lp2 += self.k_lo * (self.lp - self.lp2);
            let hy = self.k_hi * (self.hp_y + x - self.hp_x);   // one-pole high-pass
            self.hp_x = x;
            self.hp_y = hy;
            let mid = x - self.lp2 - hy;
            self.acc[0] += self.lp2.abs();
            self.acc[1] += mid.abs();
            self.acc[2] += hy.abs();
            self.n += 1;
            if self.n >= self.per {
                let n = self.n as f32;
                let _ = tx.push([self.acc[0] / n, self.acc[1] / n, self.acc[2] / n]);   // full: the screen is behind; drop
                self.acc = [0.0; 3];
                self.n = 0;
            }
        }
    }
}

pub struct Engine {
    procs: Vec<Processor>,
    meters: Vec<(BandMeter, rtrb::Producer<Bands>)>,
    /// A deck's own route to the device (None: it only feeds the mixer).
    routes: Vec<Option<Route>>,
    mixer: Option<MixerStage>,
    /// One ring per deck, then the mixer's.
    rx: Vec<rtrb::Consumer<ParamEvent>>,
    channels: usize,
    ev: Vec<ParamEvent>,
    /// Each deck's last block (stems, meters, the mixer's inputs).
    pub last: Vec<(Vec<f32>, Vec<f32>)>,
    /// The mixer's last block.
    pub mix: (Vec<f32>, Vec<f32>),
}

/// The main thread's end: push events to a plugin (decks 0.., then the mixer).
pub struct Sender(pub Vec<rtrb::Producer<ParamEvent>>);

impl Sender {
    pub fn send(&mut self, target: usize, e: ParamEvent) {
        if let Some(tx) = self.0.get_mut(target) {
            if tx.push(e).is_err() { log::warn!("engine: plugin {target}'s event ring is full; dropped an event"); }
        }
    }
}

impl Engine {
    /// The engine, the main thread's sender, and each deck's band columns.
    pub fn new(procs: Vec<Processor>, routes: Vec<Option<Route>>, mixer: Option<MixerStage>, channels: usize, max_block: usize, rate: u32) -> (Engine, Sender, Vec<rtrb::Consumer<Bands>>) {
        let n = procs.len();
        let (meters, bands): (Vec<_>, Vec<_>) = (0..n).map(|_| {
            let (tx, rx) = rtrb::RingBuffer::new(512);
            ((BandMeter::new(rate as f32), tx), rx)
        }).unzip();
        let rings = n + usize::from(mixer.is_some());
        let (tx, rx): (Vec<_>, Vec<_>) = (0..rings).map(|_| rtrb::RingBuffer::new(1024)).unzip();
        (Engine {
            procs, meters, routes, mixer, rx, channels,
            ev: Vec::with_capacity(256),
            last: (0..n).map(|_| (vec![0.0; max_block], vec![0.0; max_block])).collect(),
            mix: (vec![0.0; max_block], vec![0.0; max_block]),
        }, Sender(tx), bands)
    }

    pub fn channels(&self) -> usize { self.channels }

    fn events(&mut self, ring: usize) {
        self.ev.clear();
        while let Ok(e) = self.rx[ring].pop() { self.ev.push(e); }
    }

    /// Fill `out` (interleaved, `channels` wide) with one block.
    pub fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let frames = out.len() / self.channels;
        for (l, r) in self.last.iter_mut().chain(std::iter::once(&mut self.mix)) {
            if l.len() < frames { l.resize(frames, 0.0); r.resize(frames, 0.0); }   // a device breaking its block size: once
        }
        for i in 0..self.procs.len() {
            self.events(i);
            let (l, r) = &mut self.last[i];
            let (l, r) = (&mut l[..frames], &mut r[..frames]);
            if let Err(e) = self.procs[i].process(l, r, &self.ev) {
                log::error!("engine: deck {i}: {e:#}");
                l.fill(0.0);
                r.fill(0.0);
            }
            if let Some(route) = &self.routes[i] { add(out, self.channels, route, l, r); }
            let (m, tx) = &mut self.meters[i];
            m.run(l, r, tx);
        }
        if self.mixer.is_some() {
            self.events(self.procs.len());
            let m = self.mixer.as_mut().unwrap();
            let mut ins: [[&[f32]; 2]; clap_host::MAX_INPUTS] = [[&[], &[]]; clap_host::MAX_INPUTS];
            let n = m.inputs.len().min(clap_host::MAX_INPUTS);
            for (slot, &d) in ins.iter_mut().zip(&m.inputs) {
                *slot = [&self.last[d].0[..frames], &self.last[d].1[..frames]];
            }
            let (l, r) = (&mut self.mix.0[..frames], &mut self.mix.1[..frames]);
            if let Err(e) = m.proc.process_with_inputs(&ins[..n], l, r, &self.ev) {
                log::error!("engine: mixer: {e:#}");
                l.fill(0.0);
                r.fill(0.0);
            }
            add(out, self.channels, &m.route, l, r);
        }
    }
}

fn add(out: &mut [f32], channels: usize, route: &Route, l: &[f32], r: &[f32]) {
    for f in 0..l.len() {
        out[f * channels + route.out[0]] += l[f] * route.gain;
        out[f * channels + route.out[1]] += r[f] * route.gain;
    }
}
