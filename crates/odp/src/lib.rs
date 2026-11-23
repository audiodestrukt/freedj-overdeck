//! ODP, the Overdeck protocol: the one vocabulary between deck hardware and
//! deck software.
//!
//! A surface (jog, buttons, faders, read over SPI, I2C, GPIO, MIDI or USB)
//! speaks [`Input`]s; a deck answers with [`Output`]s (LEDs, a screen's
//! frames, its audio's level).  The same messages travel every way a deck
//! can be reached, so a deck never knows what carried them:
//!
//! - in-process: the host turns them into a CLAP plugin's parameter events
//!   today, and later into calls on the `com.audiodestrukt.odp` CLAP
//!   extension;
//! - between processes or boards: [`encode`] / [`decode`] frame them as
//!   bytes, for shared memory, a USB vendor endpoint, SPI or a socket.
//!
//! No std beyond `alloc`-free slices: the codec builds for a microcontroller
//! (`#![no_std]`), so a small board's firmware speaks the same frames.
#![no_std]

/// A deck in the host: 0-based, in the order the config lists them.
pub type DeckId = u8;

/// One control's change, normalized: what it means, not how it was read.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Control {
    /// PLAY / PAUSE, pressed (true) or released.
    Play(bool),
    /// CUE, pressed or released.
    Cue(bool),
    /// The platter's top touched (VINYL mode holds the record).
    JogTouch(bool),
    /// The platter turned, in ticks (signed; one tick is [`JOG_SECS_PER_TICK`]
    /// of record at 0 % pitch, as on the deck).
    Jog(i32),
    /// The tempo fader, in percent (−100..100; the deck clamps to its range).
    Pitch(f32),
    /// Any other key, by the deck's own number for it: see [`Key`].
    Key(Key, bool),
}

/// Keys past the transport, numbered for the wire.  Unknown numbers are kept
/// (`Other`) so an old host passes a newer surface's keys through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Sync,
    Master,
    HotCue(u8),
    LoopIn,
    LoopOut,
    Reloop,
    Back,
    Browse,
    Load,
    Shift,
    Other(u16),
}

impl Key {
    pub fn code(self) -> u16 {
        match self {
            Key::Sync => 1, Key::Master => 2, Key::LoopIn => 3, Key::LoopOut => 4, Key::Reloop => 5,
            Key::Back => 6, Key::Browse => 7, Key::Load => 8, Key::Shift => 9,
            Key::HotCue(n) => 0x100 + u16::from(n),
            Key::Other(c) => c,
        }
    }
    pub fn from_code(c: u16) -> Key {
        match c {
            1 => Key::Sync, 2 => Key::Master, 3 => Key::LoopIn, 4 => Key::LoopOut, 5 => Key::Reloop,
            6 => Key::Back, 7 => Key::Browse, 8 => Key::Load, 9 => Key::Shift,
            0x100..=0x1ff => Key::HotCue((c - 0x100) as u8),
            c => Key::Other(c),
        }
    }
}

/// The jog's resolution, shared with freedj (`opendeck_app::JOG_SECS_PER_TICK`).
pub const JOG_SECS_PER_TICK: f64 = 0.020;

/// A surface's message to the host: which deck, which control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Input {
    pub deck: DeckId,
    pub control: Control,
}

/// A deck's message back to its surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Output {
    /// A lamp: which key's, and its level (0 off, 255 full).
    Lamp { deck: DeckId, key: u16, level: u8 },
    /// The deck's output level, 0..=255 (for a meter on the surface).
    Level { deck: DeckId, level: u8 },
    /// A screen's frame is ready in the shared framebuffer `screen`
    /// (frames move by reference, never through this codec).
    Frame { deck: DeckId, screen: u8, seq: u32 },
}

// ── wire codec ───────────────────────────────────────────────────────────
//
// A frame: [len u8][kind u8][deck u8][payload…], little-endian.  `len` counts
// everything after itself.  Small and fixed so an MCU parses it without a
// heap; one USB bulk packet or SPI transfer carries many.

const K_PLAY: u8 = 1;
const K_CUE: u8 = 2;
const K_TOUCH: u8 = 3;
const K_JOG: u8 = 4;
const K_PITCH: u8 = 5;
const K_KEY: u8 = 6;
const K_LAMP: u8 = 0x81;
const K_LEVEL: u8 = 0x82;
const K_FRAME: u8 = 0x83;

/// Why a frame didn't decode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The buffer ends mid-frame: wait for more bytes.
    Short,
    /// A kind this side doesn't know: skip `len` bytes and go on.
    Unknown(u8),
    /// Too small a buffer to encode into.
    Full,
}

/// Either way a frame can go.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Msg {
    In(Input),
    Out(Output),
}

/// Encode one message into `buf`; the bytes written.
pub fn encode(m: &Msg, buf: &mut [u8]) -> Result<usize, Error> {
    let mut p = [0u8; 12];
    let (kind, deck, n) = match *m {
        Msg::In(Input { deck, control }) => match control {
            Control::Play(b) => { p[0] = b as u8; (K_PLAY, deck, 1) }
            Control::Cue(b) => { p[0] = b as u8; (K_CUE, deck, 1) }
            Control::JogTouch(b) => { p[0] = b as u8; (K_TOUCH, deck, 1) }
            Control::Jog(t) => { p[..4].copy_from_slice(&t.to_le_bytes()); (K_JOG, deck, 4) }
            Control::Pitch(v) => { p[..4].copy_from_slice(&v.to_le_bytes()); (K_PITCH, deck, 4) }
            Control::Key(k, b) => { p[..2].copy_from_slice(&k.code().to_le_bytes()); p[2] = b as u8; (K_KEY, deck, 3) }
        },
        Msg::Out(o) => match o {
            Output::Lamp { deck, key, level } => { p[..2].copy_from_slice(&key.to_le_bytes()); p[2] = level; (K_LAMP, deck, 3) }
            Output::Level { deck, level } => { p[0] = level; (K_LEVEL, deck, 1) }
            Output::Frame { deck, screen, seq } => { p[0] = screen; p[1..5].copy_from_slice(&seq.to_le_bytes()); (K_FRAME, deck, 5) }
        },
    };
    let total = 3 + n;
    if buf.len() < total { return Err(Error::Full); }
    buf[0] = (total - 1) as u8;
    buf[1] = kind;
    buf[2] = deck;
    buf[3..total].copy_from_slice(&p[..n]);
    Ok(total)
}

/// Decode the frame at the start of `buf`: the message and the bytes it took.
/// On `Unknown`, skip `buf[0] + 1` bytes.
pub fn decode(buf: &[u8]) -> Result<(Msg, usize), Error> {
    let len = *buf.first().ok_or(Error::Short)? as usize;
    if buf.len() < len + 1 || len < 2 { return Err(Error::Short); }
    let (kind, deck, p) = (buf[1], buf[2], &buf[3..len + 1]);
    let need = |n: usize| if p.len() >= n { Ok(()) } else { Err(Error::Short) };
    let i32_at = |i: usize| i32::from_le_bytes([p[i], p[i + 1], p[i + 2], p[i + 3]]);
    let u16_at = |i: usize| u16::from_le_bytes([p[i], p[i + 1]]);
    let input = |control| Msg::In(Input { deck, control });
    let m = match kind {
        K_PLAY => { need(1)?; input(Control::Play(p[0] != 0)) }
        K_CUE => { need(1)?; input(Control::Cue(p[0] != 0)) }
        K_TOUCH => { need(1)?; input(Control::JogTouch(p[0] != 0)) }
        K_JOG => { need(4)?; input(Control::Jog(i32_at(0))) }
        K_PITCH => { need(4)?; input(Control::Pitch(f32::from_bits(i32_at(0) as u32))) }
        K_KEY => { need(3)?; input(Control::Key(Key::from_code(u16_at(0)), p[2] != 0)) }
        K_LAMP => { need(3)?; Msg::Out(Output::Lamp { deck, key: u16_at(0), level: p[2] }) }
        K_LEVEL => { need(1)?; Msg::Out(Output::Level { deck, level: p[0] }) }
        K_FRAME => { need(5)?; Msg::Out(Output::Frame { deck, screen: p[0], seq: i32_at(1) as u32 }) }
        k => return Err(Error::Unknown(k)),
    };
    Ok((m, len + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: &[Msg] = &[
        Msg::In(Input { deck: 0, control: Control::Play(true) }),
        Msg::In(Input { deck: 1, control: Control::Cue(false) }),
        Msg::In(Input { deck: 2, control: Control::JogTouch(true) }),
        Msg::In(Input { deck: 3, control: Control::Jog(-12345) }),
        Msg::In(Input { deck: 0, control: Control::Pitch(-6.25) }),
        Msg::In(Input { deck: 0, control: Control::Key(Key::HotCue(7), true) }),
        Msg::In(Input { deck: 0, control: Control::Key(Key::Other(0x7777), false) }),
        Msg::Out(Output::Lamp { deck: 1, key: 2, level: 200 }),
        Msg::Out(Output::Level { deck: 1, level: 99 }),
        Msg::Out(Output::Frame { deck: 3, screen: 1, seq: 0xdead_beef }),
    ];

    #[test]
    fn every_message_survives_the_wire() {
        let mut buf = [0u8; 256];
        let mut n = 0;
        for m in ALL { n += encode(m, &mut buf[n..]).unwrap(); }
        let mut at = 0;
        for m in ALL {
            let (got, used) = decode(&buf[at..n]).unwrap();
            assert_eq!(&got, m);
            at += used;
        }
        assert_eq!(at, n, "a packet of frames decodes to its end");
    }

    #[test]
    fn a_split_frame_waits_and_an_unknown_one_is_skipped() {
        let mut buf = [0u8; 16];
        let n = encode(&ALL[3], &mut buf).unwrap();
        assert_eq!(decode(&buf[..n - 1]), Err(Error::Short));
        let unknown = [3u8, 0x42, 0, 9];
        assert_eq!(decode(&unknown), Err(Error::Unknown(0x42)));
        assert_eq!(unknown[0] as usize + 1, unknown.len(), "and says how far to skip");
    }
}
