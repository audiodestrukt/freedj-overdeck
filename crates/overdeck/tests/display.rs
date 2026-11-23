//! End to end: two real freedj-3000 decks draw their own screens into the
//! screens Overdeck made for them (the display contract), and a touch on a
//! deck's screen reaches that deck.
//!
//! Needs the plugin (~/.clap/OpenDeck.clap, freedj-3000's
//! tools/bundle-plugin.sh) and a GPU; no window opens.  Runs only with
//! OVERDECK_E2E=1; TRACK_A / TRACK_B pick the tracks.

use display::{Screen, Touch};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn decks_draw_into_the_hosts_screens_and_take_its_touches() {
    if std::env::var("OVERDECK_E2E").as_deref() != Ok("1") {
        eprintln!("skipped: set OVERDECK_E2E=1 (needs ~/.clap/OpenDeck.clap and a GPU)");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let freedj = root.join("../freedj-3000");
    let a = std::env::var("TRACK_A").map(PathBuf::from).unwrap_or(freedj.join("techno.mp3"));
    let b = std::env::var("TRACK_B").map(PathBuf::from).unwrap_or(freedj.join("amplificate.mp3"));
    let tmp = root.join("target/tmp/e2e-display");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let cfg = std::fs::read_to_string(root.join("tests/fixtures/two-decks.toml")).unwrap()
        .replace("TRACK_A", &a.to_string_lossy()).replace("TRACK_B", &b.to_string_lossy());
    std::fs::write(tmp.join("two.toml"), format!("display = true\n{cfg}")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_overdeck"))
        .args(["render", tmp.join("two.toml").to_str().unwrap(), "--seconds", "10", "--out", tmp.join("out").to_str().unwrap()])
        .spawn().unwrap();
    // Where the host makes them (as the deck loop needs the real runtime dir for its window system).
    let run = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let template = run.join(format!("overdeck-{}-deck{{n}}.screen", child.id())).to_string_lossy().into_owned();

    // Both screens fill with frames.
    let t0 = Instant::now();
    let mut screens: Vec<Option<Screen>> = vec![None, None];
    loop {
        for (i, s) in screens.iter_mut().enumerate() {
            if s.is_none() { *s = Screen::open(&Screen::path_for(&template, i as u32 + 1)).ok(); }
        }
        if screens.iter().all(|s| s.as_ref().is_some_and(|s| s.frame() > 30)) { break; }
        assert!(t0.elapsed() < Duration::from_secs(20), "the decks didn't draw into their screens");
        std::thread::sleep(Duration::from_millis(100));
    }
    for (i, s) in screens.iter_mut().enumerate() {
        let s = s.as_mut().unwrap();
        let (w, h) = s.size();
        let mut px = Vec::new();
        let f = (0..50).find_map(|_| { let f = s.read_frame(&mut px); if f.is_none() { std::thread::sleep(Duration::from_millis(5)); } f }).unwrap();
        let lit = px.chunks(4).filter(|p| u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]) > 120).count();
        eprintln!("deck {i}: {w}x{h}, frame {f}, {lit} lit pixels");
        assert!(lit > (w * h / 20) as usize, "deck {i}'s screen is blank");
        // A tap: the deck takes it (the ring empties) as it draws on.
        let before = s.frame();
        assert!(s.push_touch(Touch::Down(0.5, 0.5)));
        assert!(s.push_touch(Touch::Up(0.5, 0.5)));
        let t1 = Instant::now();
        while s.push_touch_pending() {
            assert!(t1.elapsed() < Duration::from_secs(2), "deck {i} never took the tap");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(s.frame() > before, "deck {i} stopped drawing");
    }
    assert!(child.wait().unwrap().success(), "overdeck render failed");
    assert!(!Path::new(&Screen::path_for(&template, 1)).exists(), "the screens are removed when the run ends");
}
