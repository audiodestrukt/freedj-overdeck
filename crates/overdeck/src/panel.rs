//! The panel link: a local socket a panel (`overdeck-panel`) connects to.
//! The host sends what there is to show; the panel sends moves back.  One
//! JSON object a line, both ways:
//!
//! host → panel
//!   `{"kind":"params","params":[{"id":1,"name":"A Low","value":0.81,"text":"+0.0","default":0.81}]}`  the mixer's, on connect
//!   `{"kind":"values","values":[[1,0.81,"+0.0"], …]}`                                           ~30 times a second
//!   `{"kind":"decks","decks":[{"title":"…","playing":true,"pitch":"+2.40","elapsed":61.2,"cols":[[l,m,h],…]}]}`
//!                                                                    ~30 times a second; `cols` = waveform columns since the last
//!   `{"kind":"library","tracks":[{"path":"…","title":"…"}]}`          on connect
//! panel → host
//!   `{"set":{"id":1,"value":0.5}}`               a mixer parameter (CLAP value)
//!   `{"deck":{"deck":0,"play":true}}`            a deck control: play / cue / touch (bool), pitch (%), jog (ticks)
//!   `{"load":{"deck":0,"path":"/music/a.mp3"}}`  load a track onto a deck
//!
//! A panel is a control surface like any other; it doesn't have to be on
//! this machine's screen (the socket could as well be TCP, for a tablet).

use crate::engine::Bands;
use clap_host::Instance;
use odp::Control;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// What the screen shows of a deck.
#[derive(Serialize, Clone, Default)]
pub struct DeckView {
    pub title: String,
    pub playing: bool,
    pub pitch: String,
    /// Seconds into the track, as the host reckons it (play time × pitch);
    /// the deck's own clock comes with the ODP extension.
    pub elapsed: f64,
    /// Waveform columns not yet sent.
    pub cols: Vec<Bands>,
}

#[derive(Serialize)]
struct ParamInfo<'a> { id: u32, name: &'a str, value: f64, text: String, default: f64 }

#[derive(Deserialize)]
struct Set { id: u32, value: f64 }

#[derive(Deserialize)]
struct DeckMsg { deck: u8, play: Option<bool>, cue: Option<bool>, touch: Option<bool>, pitch: Option<f32>, jog: Option<i32> }

#[derive(Deserialize)]
struct Load { deck: u8, path: String }

#[derive(Deserialize)]
struct FromPanel { set: Option<Set>, deck: Option<DeckMsg>, load: Option<Load> }

/// What a panel asked for.
pub enum Cmd {
    Set(u32, f64),
    Deck(u8, Control),
    Load(u8, PathBuf),
}

pub struct Panel {
    listener: UnixListener,
    pub path: PathBuf,
    client: Option<(UnixStream, BufReader<UnixStream>)>,
    last_sent: Instant,
    /// A panel came and went (its window closed).
    pub ended: bool,
}

impl Panel {
    pub fn listen() -> std::io::Result<Panel> {
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let path = dir.join(format!("overdeck-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        Ok(Panel { listener, path, client: None, last_sent: Instant::now(), ended: false })
    }

    fn send(&mut self, v: &serde_json::Value) {
        let Some((w, _)) = &mut self.client else { return };
        let mut line = v.to_string();
        line.push('\n');
        if w.write_all(line.as_bytes()).is_err() {
            log::info!("panel: disconnected");
            self.client = None;
        }
    }

    /// Accept a panel, take its commands, and send it what's due.  Main thread.
    pub fn poll(&mut self, mixer: Option<&Instance>, decks: &mut [DeckView], library: &[(String, String)], cmds: &mut Vec<Cmd>) {
        if self.client.is_none() {
            if let Ok((s, _)) = self.listener.accept() {
                let _ = s.set_nonblocking(true);
                if let Ok(r) = s.try_clone() {
                    log::info!("panel: connected");
                    self.client = Some((s, BufReader::new(r)));
                    if let Some(m) = mixer {
                        let params: Vec<_> = m.params().iter().map(|p| {
                            let value = m.get_value(p.id).unwrap_or(p.default);
                            ParamInfo { id: p.id, name: &p.name, value, text: m.value_to_text(p.id, value).unwrap_or_default(), default: p.default }
                        }).collect();
                        self.send(&serde_json::json!({ "kind": "params", "params": params }));
                    }
                    let tracks: Vec<_> = library.iter().map(|(p, t)| serde_json::json!({ "path": p, "title": t })).collect();
                    self.send(&serde_json::json!({ "kind": "library", "tracks": tracks }));
                }
            }
        }
        let mut gone = false;
        if let Some((_, r)) = &mut self.client {
            let mut line = String::new();
            loop {
                line.clear();
                match r.read_line(&mut line) {
                    Ok(0) => { gone = true; break }
                    Ok(_) => if let Ok(m) = serde_json::from_str::<FromPanel>(&line) {
                        if let Some(s) = m.set { cmds.push(Cmd::Set(s.id, s.value)); }
                        if let Some(l) = m.load { cmds.push(Cmd::Load(l.deck, PathBuf::from(l.path))); }
                        if let Some(d) = m.deck {
                            let c = match (d.play, d.cue, d.touch, d.pitch, d.jog) {
                                (Some(b), ..) => Some(Control::Play(b)),
                                (_, Some(b), ..) => Some(Control::Cue(b)),
                                (_, _, Some(b), ..) => Some(Control::JogTouch(b)),
                                (_, _, _, Some(p), _) => Some(Control::Pitch(p)),
                                (_, _, _, _, Some(t)) => Some(Control::Jog(t)),
                                _ => None,
                            };
                            if let Some(c) = c { cmds.push(Cmd::Deck(d.deck, c)); }
                        }
                    },
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => { gone = true; break }
                }
            }
        }
        if gone { log::info!("panel: disconnected"); self.client = None; self.ended = true; }
        if self.client.is_none() {
            for d in decks.iter_mut() { d.cols.clear(); }   // nobody to show them to
            return;
        }
        if self.last_sent.elapsed() >= Duration::from_millis(33) {
            self.last_sent = Instant::now();
            if let Some(m) = mixer {
                let values: Vec<_> = m.params().iter().filter_map(|p| {
                    let v = m.get_value(p.id)?;
                    Some(serde_json::json!([p.id, v, m.value_to_text(p.id, v).unwrap_or_default()]))
                }).collect();
                self.send(&serde_json::json!({ "kind": "values", "values": values }));
            }
            self.send(&serde_json::json!({ "kind": "decks", "decks": &*decks }));
            for d in decks.iter_mut() { d.cols.clear(); }
        }
    }
}

impl Drop for Panel {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.path); }
}
