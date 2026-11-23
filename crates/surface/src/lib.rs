//! The host's hardware abstraction: every bus is owned here, by one process.
//!
//! A [`Driver`] reads one piece of hardware (a jog's quadrature on GPIO, a
//! button board on I2C, a panel on SPI, a MIDI controller, a deck board on
//! USB) and turns what it reads into ODP [`Input`]s; it takes ODP
//! [`Output`]s (lamps, levels) back to the hardware.  Decks never open a bus:
//! they get ODP, whichever host they run in (Overdeck on a Pi, a DAW on a
//! laptop), so the same deck plugin runs with any surface.
//!
//! Drivers here:
//! - [`Script`]: inputs at set times (tests, demos, a routine).
//! - [`Wire`]: ODP frames from any byte stream (a USB vendor endpoint, a
//!   UART, a socket, a small board on SPI that already speaks ODP).
//!
//! Next, each behind a cargo feature so a build carries only its own:
//! `gpio` (quadrature jog + keys, via the kernel's gpio-cdev), `i2c`
//! (PCF8574 key boards, as freedj's controllers.toml has them), `spi` (the
//! CDJ-3000 sub-CPU and XDJ panel buses from freedj), `midi`.

use odp::{Input, Output};
use std::time::Duration;

/// One piece of hardware.
pub trait Driver: Send {
    /// A name for logs.
    fn name(&self) -> &str;
    /// Append what happened since the last poll.  `now` is the host's clock
    /// (time since it started).  Never blocks.
    fn poll(&mut self, now: Duration, out: &mut Vec<Input>);
    /// Show a deck's output on this hardware, if it can.
    fn show(&mut self, _o: &Output) {}
}

/// Inputs at set times.
pub struct Script {
    name: String,
    steps: Vec<(Duration, Input)>,
    next: usize,
}

impl Script {
    pub fn new(name: impl Into<String>, mut steps: Vec<(Duration, Input)>) -> Script {
        steps.sort_by_key(|s| s.0);
        Script { name: name.into(), steps, next: 0 }
    }
}

impl Driver for Script {
    fn name(&self) -> &str { &self.name }
    fn poll(&mut self, now: Duration, out: &mut Vec<Input>) {
        while let Some((at, i)) = self.steps.get(self.next) {
            if *at > now { break; }
            out.push(*i);
            self.next += 1;
        }
    }
}

/// ODP frames from a byte stream.  `read` fills a buffer without blocking
/// and says how many bytes it put there (0 = nothing now).
pub struct Wire<R: FnMut(&mut [u8]) -> usize + Send> {
    name: String,
    read: R,
    buf: Vec<u8>,
}

impl<R: FnMut(&mut [u8]) -> usize + Send> Wire<R> {
    pub fn new(name: impl Into<String>, read: R) -> Self { Wire { name: name.into(), read, buf: Vec::new() } }
}

impl<R: FnMut(&mut [u8]) -> usize + Send> Driver for Wire<R> {
    fn name(&self) -> &str { &self.name }
    fn poll(&mut self, _now: Duration, out: &mut Vec<Input>) {
        let mut chunk = [0u8; 512];
        loop {
            let n = (self.read)(&mut chunk);
            if n == 0 { break; }
            self.buf.extend_from_slice(&chunk[..n]);
        }
        let mut at = 0;
        while at < self.buf.len() {
            match odp::decode(&self.buf[at..]) {
                Ok((odp::Msg::In(i), n)) => { out.push(i); at += n; }
                Ok((odp::Msg::Out(_), n)) => at += n,                // not ours to act on
                Err(odp::Error::Unknown(_)) => at += self.buf[at] as usize + 1,
                Err(_) => break,                                     // a split frame: wait
            }
        }
        self.buf.drain(..at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use odp::{Control, Msg};

    #[test]
    fn a_script_gives_each_input_once_at_its_time() {
        let i = |d, c| Input { deck: d, control: c };
        let mut s = Script::new("t", vec![
            (Duration::from_millis(20), i(1, Control::Play(true))),
            (Duration::from_millis(10), i(0, Control::Play(true))),
        ]);
        let mut out = Vec::new();
        s.poll(Duration::from_millis(5), &mut out);
        assert!(out.is_empty());
        s.poll(Duration::from_millis(15), &mut out);
        assert_eq!(out, vec![i(0, Control::Play(true))]);
        s.poll(Duration::from_millis(50), &mut out);
        s.poll(Duration::from_millis(60), &mut out);
        assert_eq!(out.len(), 2, "each step once");
    }

    #[test]
    fn a_wire_reassembles_frames_split_across_reads() {
        let mut bytes = Vec::new();
        let mut b = [0u8; 16];
        for c in [Control::Jog(5), Control::Cue(true), Control::Pitch(2.5)] {
            let n = odp::encode(&Msg::In(Input { deck: 2, control: c }), &mut b).unwrap();
            bytes.extend_from_slice(&b[..n]);
        }
        // Dribble it in three bytes at a time, as a slow UART would.
        let mut src = bytes.clone().into_iter();
        let mut w = Wire::new("w", move |buf: &mut [u8]| {
            let mut n = 0;
            for slot in buf.iter_mut().take(3) { match src.next() { Some(x) => { *slot = x; n += 1 } None => break } }
            n
        });
        let mut out = Vec::new();
        w.poll(Duration::ZERO, &mut out);
        assert_eq!(out.iter().map(|i| i.control).collect::<Vec<_>>(), vec![Control::Jog(5), Control::Cue(true), Control::Pitch(2.5)]);
    }
}
