//! The panel link: a local socket a panel (`overdeck-panel`) connects to.
//! The host tells it the mixer's parameters and their live values (about 30
//! times a second, so automation shows); the panel sends moves back.  One
//! JSON object a line, both ways:
//!
//! host → panel  `{"kind":"params","params":[{"id":1,"name":"A Low","value":0.81,"text":"+0.0","default":0.81}]}`
//!               `{"kind":"values","values":[[1,0.81,"+0.0"], …]}`
//! panel → host  `{"set":{"id":1,"value":0.5}}`
//!
//! A panel is a control surface like any other; it doesn't have to be on
//! this machine's screen (the socket could as well be TCP, for a tablet).

use clap_host::Instance;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Serialize)]
struct ParamInfo<'a> { id: u32, name: &'a str, value: f64, text: String, default: f64 }

#[derive(Deserialize)]
struct Set { id: u32, value: f64 }

#[derive(Deserialize)]
struct FromPanel { set: Option<Set> }

pub struct Panel {
    listener: UnixListener,
    pub path: PathBuf,
    client: Option<(UnixStream, BufReader<UnixStream>)>,
    last_sent: Instant,
}

impl Panel {
    pub fn listen() -> std::io::Result<Panel> {
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let path = dir.join(format!("overdeck-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        Ok(Panel { listener, path, client: None, last_sent: Instant::now() })
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

    /// Accept a panel, take its moves (returned as (param id, CLAP value)),
    /// and send it the mixer's values when due.  Main thread.
    pub fn poll(&mut self, mixer: &Instance, moves: &mut Vec<(u32, f64)>) {
        if self.client.is_none() {
            if let Ok((s, _)) = self.listener.accept() {
                let _ = s.set_nonblocking(true);
                if let Ok(r) = s.try_clone() {
                    log::info!("panel: connected");
                    self.client = Some((s, BufReader::new(r)));
                    let params: Vec<_> = mixer.params().iter().map(|p| {
                        let value = mixer.get_value(p.id).unwrap_or(p.default);
                        ParamInfo { id: p.id, name: &p.name, value, text: mixer.value_to_text(p.id, value).unwrap_or_default(), default: p.default }
                    }).collect();
                    self.send(&serde_json::json!({ "kind": "params", "params": params }));
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
                    Ok(_) => {
                        if let Ok(FromPanel { set: Some(s) }) = serde_json::from_str::<FromPanel>(&line) { moves.push((s.id, s.value)); }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => { gone = true; break }
                }
            }
        }
        if gone { log::info!("panel: disconnected"); self.client = None; }
        if self.client.is_some() && self.last_sent.elapsed() >= Duration::from_millis(33) {
            self.last_sent = Instant::now();
            let values: Vec<_> = mixer.params().iter().filter_map(|p| {
                let v = mixer.get_value(p.id)?;
                Some(serde_json::json!([p.id, v, mixer.value_to_text(p.id, v).unwrap_or_default()]))
            }).collect();
            self.send(&serde_json::json!({ "kind": "values", "values": values }));
        }
    }
}

impl Drop for Panel {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.path); }
}
