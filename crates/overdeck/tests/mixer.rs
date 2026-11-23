//! End to end: two real freedj decks into the reference mixer, its
//! crossfader and EQ driven by `[[automate]]`, heard in the host's output.
//! OVERDECK_E2E=1 only (needs ~/.clap/OpenDeck.clap, a display, and the
//! mixer built: `cargo build --release -p overdeck-mixer`).

use std::path::{Path, PathBuf};
use std::process::Command;

fn read(path: &Path) -> (usize, Vec<f32>) {
    let mut r = hound::WavReader::open(path).unwrap();
    let ch = r.spec().channels as usize;
    (ch, r.samples::<f32>().map(Result::unwrap).collect())
}

/// Left channel's RMS over second `s`, optionally low-passed (two one-pole
/// stages at ~150 Hz) to measure the bass.
fn rms(ch: usize, s: &[f32], sec: usize, low: bool) -> f32 {
    let k = 1.0 - (-2.0 * std::f32::consts::PI * 150.0 / 48_000.0).exp();
    let (mut y1, mut y2) = (0.0f32, 0.0f32);
    let mut acc = 0.0;
    let mut n = 0;
    for (i, fr) in s.chunks(ch).enumerate().take((sec + 1) * 48_000) {
        let mut x = fr[0];
        if low { y1 += k * (x - y1); y2 += k * (y1 - y2); x = y2; }
        if i >= sec * 48_000 { acc += x * x; n += 1; }
    }
    (acc / n as f32).sqrt()
}

#[test]
fn the_mixer_crossfades_and_kills_bass_on_cue() {
    if std::env::var("OVERDECK_E2E").as_deref() != Ok("1") {
        eprintln!("skipped: set OVERDECK_E2E=1");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let freedj = root.join("../freedj-3000");
    let a = std::env::var("TRACK_A").map(PathBuf::from).unwrap_or(freedj.join("techno.mp3"));
    let b = std::env::var("TRACK_B").map(PathBuf::from).unwrap_or(freedj.join("amplificate.mp3"));
    let mixer = root.join("target/release/liboverdeck_mixer.so");
    assert!(mixer.is_file(), "build the mixer first: cargo build --release -p overdeck-mixer");
    let tmp = root.join("target/tmp/e2e-mixer");
    std::fs::create_dir_all(&tmp).unwrap();
    let cfg = std::fs::read_to_string(root.join("tests/fixtures/mixer.toml")).unwrap()
        .replace("TRACK_A", &a.to_string_lossy()).replace("TRACK_B", &b.to_string_lossy())
        .replace("MIXER", &mixer.to_string_lossy());
    std::fs::write(tmp.join("mixer.toml"), cfg).unwrap();
    let st = Command::new(env!("CARGO_BIN_EXE_overdeck"))
        .args(["render", tmp.join("mixer.toml").to_str().unwrap(), "--seconds", "11", "--out", tmp.join("out").to_str().unwrap()])
        .status().unwrap();
    assert!(st.success(), "overdeck render failed");

    let (cm, m) = read(&tmp.join("out/mix.wav"));
    let (_, da) = read(&tmp.join("out/deck0.wav"));
    let (_, db) = read(&tmp.join("out/deck1.wav"));
    let (a3, b3, m3) = (rms(2, &da, 3, false), rms(2, &db, 3, false), rms(cm, &m, 3, false));
    eprintln!("3 s: A {a3:.3} B {b3:.3} mix {m3:.3}");
    assert!(b3 > 0.05, "deck B is playing at 3 s");
    assert!((m3 / a3 - 1.0).abs() < 0.1, "crossfader on A: the mix is deck A ({m3} vs {a3})");

    let (low3, low6, m6) = (rms(cm, &m, 3, true), rms(cm, &m, 6, true), rms(cm, &m, 6, false));
    eprintln!("bass: 3 s {low3:.3}, 6 s (A Low killed) {low6:.3}; mix at 6 s {m6:.3}");
    assert!(low6 < low3 * 0.25, "A Low kill takes out the bass");
    assert!(m6 > 0.05, "and leaves the rest of deck A");

    let (b10, m10) = (rms(2, &db, 10, false), rms(cm, &m, 10, false));
    eprintln!("10 s: B {b10:.3} mix {m10:.3}");
    assert!((m10 / b10 - 1.0).abs() < 0.1, "crossfader on B: the mix is deck B ({m10} vs {b10})");
}
