//! The display contract: how a deck shows its own screen inside the host's.
//!
//! The host allocates one screen per deck: a file in shared memory (under
//! `$XDG_RUNTIME_DIR`) holding a header, a small ring of touches, and one
//! RGBA frame.  It sizes it (the deck draws its layout to fit), hands the
//! deck one end and the unit's screen the other.  The deck draws each frame
//! with its own renderer and copies it in; the screen shows it wherever it
//! likes and writes the touches that land on it into the ring, in the
//! frame's own coordinates (0..1), for the deck to play back as its own
//! pointer.  Nothing else crosses: the deck never learns how it is shown.
//!
//! A plugin host passes the path in `OVERDECK_DISPLAY`, a template where
//! `{n}` is the deck's number (1, 2, …, in the order the decks start).
//!
//! Layout (little-endian, every field 4 bytes):
//!
//! ```text
//!    0  magic "ODSC"     4  version (1)     8  width      12  height
//!   16  seq: odd while a frame is being written, +2 a frame
//!   20  touch head (the screen writes)      24  touch tail (the deck reads)
//!   28  the deck's heartbeat (frames drawn; 0 = not attached yet)
//!   64  touches: RING × {kind, x (f32), y (f32), 0}
//! 1088  pixels: width × height × RGBA, rows packed
//! ```
//!
//! One writer per field, so no locks: the frame is a seqlock (a reader that
//! sees `seq` move under it drops the copy), the touches a single-producer
//! single-consumer ring.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

const MAGIC: &[u8; 4] = b"ODSC";
const VERSION: u32 = 1;
const RING: u32 = 64;
const TOUCHES: usize = 64;
const PIXELS: usize = TOUCHES + RING as usize * 16;

/// A touch on the deck's screen, in its frame's coordinates (0..1 across, 0..1 down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Touch {
    Down(f32, f32),
    Move(f32, f32),
    Up(f32, f32),
}

impl Touch {
    fn code(self) -> (u32, f32, f32) {
        match self { Touch::Down(x, y) => (1, x, y), Touch::Move(x, y) => (2, x, y), Touch::Up(x, y) => (3, x, y) }
    }
    fn from_code(k: u32, x: f32, y: f32) -> Option<Touch> {
        match k { 1 => Some(Touch::Down(x, y)), 2 => Some(Touch::Move(x, y)), 3 => Some(Touch::Up(x, y)), _ => None }
    }
}

/// One deck's screen: either end of it.
pub struct Screen {
    map: memmap2::MmapMut,
    pub path: PathBuf,
    width: u32,
    height: u32,
}

#[derive(Debug)]
pub struct Error(pub String);
impl std::fmt::Display for Error { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { f.write_str(&self.0) } }
impl std::error::Error for Error {}
fn err(s: impl Into<String>) -> Error { Error(s.into()) }

impl Screen {
    /// The host: make a `width` × `height` screen at `path` (replacing any).
    pub fn create(path: &Path, width: u32, height: u32) -> Result<Screen, Error> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 { return Err(err(format!("screen size {width}x{height}"))); }
        let len = PIXELS + width as usize * height as usize * 4;
        let f = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path)
            .map_err(|e| err(format!("{}: {e}", path.display())))?;
        f.set_len(len as u64).map_err(|e| err(format!("{}: {e}", path.display())))?;
        let mut map = unsafe { memmap2::MmapMut::map_mut(&f) }.map_err(|e| err(format!("map {}: {e}", path.display())))?;
        map[0..4].copy_from_slice(MAGIC);
        map[4..8].copy_from_slice(&VERSION.to_le_bytes());
        map[8..12].copy_from_slice(&width.to_le_bytes());
        map[12..16].copy_from_slice(&height.to_le_bytes());
        Ok(Screen { map, path: path.to_owned(), width, height })
    }

    /// The deck or the screen: attach to a screen the host made.
    pub fn open(path: &Path) -> Result<Screen, Error> {
        let f = OpenOptions::new().read(true).write(true).open(path).map_err(|e| err(format!("{}: {e}", path.display())))?;
        let map = unsafe { memmap2::MmapMut::map_mut(&f) }.map_err(|e| err(format!("map {}: {e}", path.display())))?;
        if map.len() < PIXELS || &map[0..4] != MAGIC { return Err(err(format!("{}: not an Overdeck screen", path.display()))); }
        let word = |o: usize| u32::from_le_bytes(map[o..o + 4].try_into().unwrap());
        if word(4) != VERSION { return Err(err(format!("{}: screen version {}, want {VERSION}", path.display(), word(4)))); }
        let (width, height) = (word(8), word(12));
        if map.len() < PIXELS + width as usize * height as usize * 4 { return Err(err(format!("{}: short", path.display()))); }
        Ok(Screen { map, path: path.to_owned(), width, height })
    }

    /// A deck's screen path from the `OVERDECK_DISPLAY` template and its number.
    pub fn path_for(template: &str, n: u32) -> PathBuf { PathBuf::from(template.replace("{n}", &n.to_string())) }

    pub fn size(&self) -> (u32, u32) { (self.width, self.height) }

    fn atomic(&self, offset: usize) -> &AtomicU32 {
        // In bounds, 4-aligned (the map is page-aligned), and only ever touched atomically.
        unsafe { &*(self.map.as_ptr().add(offset) as *const AtomicU32) }
    }

    /// The deck: write a frame.  `fill` gets the pixel rows (RGBA, packed,
    /// `width × 4` bytes each) to write into.
    pub fn write_frame(&mut self, fill: impl FnOnce(&mut [u8])) {
        let seq = self.atomic(16).load(Ordering::Relaxed);
        self.atomic(16).store(seq.wrapping_add(1) | 1, Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Release);
        let n = self.width as usize * self.height as usize * 4;
        fill(&mut self.map[PIXELS..PIXELS + n]);
        self.atomic(16).store((seq | 1).wrapping_add(1), Ordering::Release);
        self.atomic(28).fetch_add(1, Ordering::Relaxed);
    }

    /// The frame counter (`seq / 2`): changes when there is a new frame.
    pub fn frame(&self) -> u32 { self.atomic(16).load(Ordering::Acquire) / 2 }

    /// Whether a deck has drawn into this screen yet.
    pub fn attached(&self) -> bool { self.atomic(28).load(Ordering::Relaxed) > 0 }

    /// The screen: copy the latest whole frame into `out` (RGBA, packed).
    /// None while one is half-written (try again next time).
    pub fn read_frame(&self, out: &mut Vec<u8>) -> Option<u32> {
        let a = self.atomic(16).load(Ordering::Acquire);
        if a & 1 == 1 { return None; }
        let n = self.width as usize * self.height as usize * 4;
        out.clear();
        out.extend_from_slice(&self.map[PIXELS..PIXELS + n]);
        std::sync::atomic::fence(Ordering::Acquire);
        (self.atomic(16).load(Ordering::Relaxed) == a).then_some(a / 2)
    }

    /// The screen: a touch for the deck.  Dropped if the deck has fallen
    /// a whole ring behind (it isn't drawing).
    pub fn push_touch(&mut self, t: Touch) -> bool {
        let head = self.atomic(20).load(Ordering::Relaxed);
        let tail = self.atomic(24).load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= RING { return false; }
        let (k, x, y) = t.code();
        let o = TOUCHES + (head % RING) as usize * 16;
        self.map[o..o + 4].copy_from_slice(&k.to_le_bytes());
        self.map[o + 4..o + 8].copy_from_slice(&x.to_le_bytes());
        self.map[o + 8..o + 12].copy_from_slice(&y.to_le_bytes());
        self.atomic(20).store(head.wrapping_add(1), Ordering::Release);
        true
    }

    /// The screen: whether touches sent are still waiting for the deck.
    pub fn push_touch_pending(&self) -> bool {
        self.atomic(20).load(Ordering::Acquire) != self.atomic(24).load(Ordering::Acquire)
    }

    /// The deck: the next touch, if any.
    pub fn pop_touch(&mut self) -> Option<Touch> {
        let tail = self.atomic(24).load(Ordering::Relaxed);
        let head = self.atomic(20).load(Ordering::Acquire);
        if tail == head { return None; }
        let o = TOUCHES + (tail % RING) as usize * 16;
        let f = |o: usize| f32::from_le_bytes(self.map[o..o + 4].try_into().unwrap());
        let t = Touch::from_code(u32::from_le_bytes(self.map[o..o + 4].try_into().unwrap()), f(o + 4), f(o + 8));
        self.atomic(24).store(tail.wrapping_add(1), Ordering::Release);
        t.or_else(|| self.pop_touch())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        d.join(format!("odsc-{}-{name}", std::process::id()))
    }

    #[test]
    fn a_frame_the_deck_writes_is_the_frame_the_screen_reads() {
        let p = tmp("frame");
        let host = Screen::create(&p, 4, 2).unwrap();
        let mut deck = Screen::open(&p).unwrap();
        let screen = Screen::open(&p).unwrap();
        assert_eq!(deck.size(), (4, 2));
        assert!(!screen.attached());
        let mut out = Vec::new();
        let before = screen.read_frame(&mut out).unwrap();
        deck.write_frame(|px| for (i, b) in px.iter_mut().enumerate() { *b = i as u8; });
        let after = screen.read_frame(&mut out).unwrap();
        assert_eq!(after, before + 1);
        assert_eq!(out.len(), 4 * 2 * 4);
        assert_eq!(out[5], 5);
        assert!(screen.attached());
        drop(host);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn touches_arrive_in_order_and_a_full_ring_drops_the_newest() {
        let p = tmp("touch");
        let _host = Screen::create(&p, 2, 2).unwrap();
        let mut deck = Screen::open(&p).unwrap();
        let mut screen = Screen::open(&p).unwrap();
        assert!(screen.push_touch(Touch::Down(0.25, 0.5)));
        assert!(screen.push_touch(Touch::Up(0.25, 0.5)));
        assert_eq!(deck.pop_touch(), Some(Touch::Down(0.25, 0.5)));
        assert_eq!(deck.pop_touch(), Some(Touch::Up(0.25, 0.5)));
        assert_eq!(deck.pop_touch(), None);
        for _ in 0..RING { assert!(screen.push_touch(Touch::Move(0.1, 0.1))); }
        assert!(!screen.push_touch(Touch::Move(0.9, 0.9)));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_file_that_is_not_a_screen_is_refused() {
        let p = tmp("bad");
        std::fs::write(&p, vec![0u8; 4096]).unwrap();
        assert!(Screen::open(&p).is_err());
        let _ = std::fs::remove_file(p);
        assert_eq!(Screen::path_for("/run/x-deck{n}.screen", 2), PathBuf::from("/run/x-deck2.screen"));
    }
}
