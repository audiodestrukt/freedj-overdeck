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

pub struct Engine {
    procs: Vec<Processor>,
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
    pub fn new(procs: Vec<Processor>, routes: Vec<Option<Route>>, mixer: Option<MixerStage>, channels: usize, max_block: usize) -> (Engine, Sender) {
        let n = procs.len();
        let rings = n + usize::from(mixer.is_some());
        let (tx, rx): (Vec<_>, Vec<_>) = (0..rings).map(|_| rtrb::RingBuffer::new(1024)).unzip();
        (Engine {
            procs, routes, mixer, rx, channels,
            ev: Vec::with_capacity(256),
            last: (0..n).map(|_| (vec![0.0; max_block], vec![0.0; max_block])).collect(),
            mix: (vec![0.0; max_block], vec![0.0; max_block]),
        }, Sender(tx))
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
