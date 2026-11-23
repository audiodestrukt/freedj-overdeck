//! End to end: two real freedj-3000 decks in Overdeck, each driven by its own
//! scripted PLAY, each heard on its own channels.
//!
//! Needs the plugin (`make plugin` in freedj-3000 installs ~/.clap/OpenDeck.clap),
//! a display (the decks open their windows) and two tracks.  Runs only with
//! OVERDECK_E2E=1; TRACK_A / TRACK_B pick the tracks (default: freedj-3000's
//! techno.mp3 and amplificate.mp3 beside this repo).

use std::path::{Path, PathBuf};
use std::process::Command;

fn rms_per_second(path: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).unwrap();
    let spec = r.spec();
    let per = spec.sample_rate as usize * spec.channels as usize;
    let s: Vec<f32> = r.samples::<f32>().map(Result::unwrap).collect();
    s.chunks(per).map(|c| (c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).sqrt()).collect()
}

#[test]
fn each_deck_plays_on_its_own_play_and_its_own_channels() {
    if std::env::var("OVERDECK_E2E").as_deref() != Ok("1") {
        eprintln!("skipped: set OVERDECK_E2E=1 (needs ~/.clap/OpenDeck.clap and a display)");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let freedj = root.join("../freedj-3000");
    let a = std::env::var("TRACK_A").map(PathBuf::from).unwrap_or(freedj.join("techno.mp3"));
    let b = std::env::var("TRACK_B").map(PathBuf::from).unwrap_or(freedj.join("amplificate.mp3"));
    let tmp = root.join("target/tmp/e2e");
    std::fs::create_dir_all(&tmp).unwrap();
    let cfg = std::fs::read_to_string(root.join("tests/fixtures/two-decks.toml")).unwrap()
        .replace("TRACK_A", &a.to_string_lossy()).replace("TRACK_B", &b.to_string_lossy());
    std::fs::write(tmp.join("two.toml"), cfg).unwrap();
    let st = Command::new(env!("CARGO_BIN_EXE_overdeck"))
        .args(["render", tmp.join("two.toml").to_str().unwrap(), "--seconds", "13", "--out", tmp.join("out").to_str().unwrap()])
        .status().unwrap();
    assert!(st.success(), "overdeck render failed");

    let d0 = rms_per_second(&tmp.join("out/deck0.wav"));
    let d1 = rms_per_second(&tmp.join("out/deck1.wav"));
    eprintln!("deck 0 rms/s: {d0:.3?}\ndeck 1 rms/s: {d1:.3?}");
    let quiet = |v: &[f32]| v.iter().all(|&x| x < 0.002);
    let loud = |v: &[f32]| v.iter().all(|&x| x > 0.02);
    // Deck 0: PLAY at 3 s.  Silent through second 2, playing from second 4.
    assert!(quiet(&d0[..3]), "deck 0 made sound before its PLAY: {d0:?}");
    assert!(loud(&d0[4..12]), "deck 0 didn't play after its PLAY: {d0:?}");
    // Deck 1: PLAY at 8 s.  Silent while deck 0 plays, playing from second 9.
    assert!(quiet(&d1[..8]), "deck 1 made sound before its PLAY (deck 0's?): {d1:?}");
    assert!(loud(&d1[9..12]), "deck 1 didn't play after its PLAY: {d1:?}");

    // The mix carries each deck on its own channels: 0/1 = deck 0, 2/3 = deck 1.
    let mut r = hound::WavReader::open(tmp.join("out/mix.wav")).unwrap();
    assert_eq!(r.spec().channels, 4);
    let s: Vec<f32> = r.samples::<f32>().map(Result::unwrap).collect();
    let sec = |c: usize, t: usize| {
        let f = s.chunks(4).skip(t * 48_000).take(48_000).map(|fr| fr[c] * fr[c]).sum::<f32>() / 48_000.0;
        f.sqrt()
    };
    assert!(sec(0, 5) > 0.02 && sec(2, 5) < 0.002, "at 5 s only deck 0's channels sound");
    assert!(sec(0, 10) > 0.02 && sec(2, 10) > 0.02, "at 10 s both pairs sound");
}
