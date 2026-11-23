//! The audio side: every deck's processor, run block by block, routed to
//! device channels.  Runs on the audio thread (a cpal callback, or the
//! render loop); takes parameter events from the main thread through one
//! lock-free ring per deck, so it never waits on anything.

use clap_host::{ParamEvent, Processor};

pub struct Route {
    pub out: [usize; 2],
    pub gain: f32,
}

pub struct Engine {
    procs: Vec<Processor>,
    routes: Vec<Route>,
    rx: Vec<rtrb::Consumer<ParamEvent>>,
    channels: usize,
    // Scratch, sized up front so a block allocates nothing.
    l: Vec<f32>,
    r: Vec<f32>,
    ev: Vec<ParamEvent>,
    /// Each deck's last block, kept for stems and meters.
    pub last: Vec<(Vec<f32>, Vec<f32>)>,
}

/// The main thread's end: push events to a deck.
pub struct Sender(pub Vec<rtrb::Producer<ParamEvent>>);

impl Sender {
    pub fn send(&mut self, deck: usize, e: ParamEvent) {
        if let Some(tx) = self.0.get_mut(deck) {
            if tx.push(e).is_err() { log::warn!("engine: deck {deck}'s event ring is full; dropped an event"); }
        }
    }
}

impl Engine {
    pub fn new(procs: Vec<Processor>, routes: Vec<Route>, channels: usize, max_block: usize) -> (Engine, Sender) {
        let (tx, rx): (Vec<_>, Vec<_>) = (0..procs.len()).map(|_| rtrb::RingBuffer::new(1024)).unzip();
        let n = procs.len();
        (Engine {
            procs, routes, rx, channels,
            l: vec![0.0; max_block], r: vec![0.0; max_block],
            ev: Vec::with_capacity(256),
            last: (0..n).map(|_| (vec![0.0; max_block], vec![0.0; max_block])).collect(),
        }, Sender(tx))
    }

    pub fn channels(&self) -> usize { self.channels }

    /// Fill `out` (interleaved, `channels` wide) with one block.
    pub fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let frames = out.len() / self.channels;
        if self.l.len() < frames {
            // A device asking for more than it said: grow (allocates, once).
            self.l.resize(frames, 0.0);
            self.r.resize(frames, 0.0);
        }
        for (i, p) in self.procs.iter_mut().enumerate() {
            self.ev.clear();
            while let Ok(e) = self.rx[i].pop() { self.ev.push(e); }
            let (l, r) = (&mut self.l[..frames], &mut self.r[..frames]);
            if let Err(e) = p.process(l, r, &self.ev) {
                log::error!("engine: deck {i}: {e:#}");
                l.fill(0.0);
                r.fill(0.0);
            }
            let route = &self.routes[i];
            for f in 0..frames {
                out[f * self.channels + route.out[0]] += l[f] * route.gain;
                out[f * self.channels + route.out[1]] += r[f] * route.gain;
            }
            let (sl, sr) = &mut self.last[i];
            if sl.len() < frames { sl.resize(frames, 0.0); sr.resize(frames, 0.0); }
            sl[..frames].copy_from_slice(l);
            sr[..frames].copy_from_slice(r);
        }
    }
}
