//! overdeck-panel <socket>: the mixer's panel.
//!
//! Connects to a running Overdeck's panel socket, shows the mixer's
//! parameters as a mixer (two channel strips: Trim, High / Mid / Low knobs,
//! a fader; a crossfader and master below) with their live values (the
//! host's automation moves them), and sends the moves you make back.
//!
//! Knobs: drag up / down; double-click for the default.  Faders: drag.

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use serde::Deserialize;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

#[derive(Deserialize, Clone)]
struct Param { id: u32, name: String, value: f64, text: String, default: f64 }

#[derive(Default)]
struct State {
    params: Vec<Param>,
    connected: bool,
}

#[derive(Deserialize)]
struct Msg { kind: String, params: Option<Vec<Param>>, values: Option<Vec<(u32, f64, String)>> }

struct App {
    state: Arc<Mutex<State>>,
    out: UnixStream,
    placed: bool,
    /// While a control is held, its value is ours, not the host's echo.
    held: Option<u32>,
}

fn main() -> eframe::Result<()> {
    let path = std::env::args().nth(1).expect("usage: overdeck-panel <socket>");
    let stream = UnixStream::connect(&path).unwrap_or_else(|e| panic!("connect {path}: {e}"));
    let state = Arc::new(Mutex::new(State { connected: true, ..Default::default() }));
    let out = stream.try_clone().expect("clone socket");
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("Overdeck Mixer").with_inner_size([640.0, 470.0]),
        ..Default::default()
    };
    eframe::run_native("Overdeck Mixer", opts, Box::new(move |cc| {
        let ctx = cc.egui_ctx.clone();
        ctx.set_visuals(egui::Visuals::dark());
        let st = Arc::clone(&state);
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                let Ok(m) = serde_json::from_str::<Msg>(&line) else { continue };
                let mut s = st.lock().unwrap();
                match m.kind.as_str() {
                    "params" => s.params = m.params.unwrap_or_default(),
                    "values" => for (id, v, t) in m.values.unwrap_or_default() {
                        if let Some(p) = s.params.iter_mut().find(|p| p.id == id) { p.value = v; p.text = t; }
                    },
                    _ => {}
                }
                drop(s);
                ctx.request_repaint();
            }
            st.lock().unwrap().connected = false;
            ctx.request_repaint();
        });
        Ok(Box::new(App { state, out, placed: false, held: None }))
    }))
}

const ACCENT: Color32 = Color32::from_rgb(0, 190, 220);
const KILL: Color32 = Color32::from_rgb(230, 60, 50);

impl App {
    fn set(&mut self, id: u32, value: f64) {
        let v = value.clamp(0.0, 1.0);
        let _ = writeln!(self.out, "{}", serde_json::json!({ "set": { "id": id, "value": v } }));
        if let Some(p) = self.state.lock().unwrap().params.iter_mut().find(|p| p.id == id) { p.value = v; }
    }

    fn param(&self, name: &str) -> Option<Param> {
        self.state.lock().unwrap().params.iter().find(|p| p.name == name).cloned()
    }

    /// A rotary knob: drag vertically, double-click for the default.
    fn knob(&mut self, ui: &mut egui::Ui, name: &str, label: &str) {
        let Some(p) = self.param(name) else { return };
        ui.vertical_centered(|ui| {
            let size = 46.0;
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click_and_drag());
            if resp.drag_started() { self.held = Some(p.id); }
            if resp.dragged() { self.set(p.id, p.value - f64::from(resp.drag_delta().y) / 150.0); }
            if resp.drag_stopped() { self.held = None; }
            if resp.double_clicked() { self.set(p.id, p.default); }
            let v = self.param(name).map_or(p.value, |q| q.value) as f32;
            let c = rect.center();
            let r = size / 2.0 - 4.0;
            let painter = ui.painter();
            painter.circle_filled(c, r, Color32::from_gray(40));
            let killed = p.text.contains("kill");
            let col = if killed { KILL } else { ACCENT };
            // An arc from 7 o'clock (0) through 12 to 5 o'clock (1).
            let a0 = std::f32::consts::PI * 0.75;
            let sweep = std::f32::consts::PI * 1.5;
            let pts: Vec<Pos2> = (0..=32).map(|i| {
                let a = a0 + sweep * v * i as f32 / 32.0;
                c + Vec2::angled(a) * (r + 2.0)
            }).collect();
            painter.add(egui::Shape::line(pts, Stroke::new(3.0, col)));
            let a = a0 + sweep * v;
            painter.line_segment([c, c + Vec2::angled(a) * (r - 3.0)], Stroke::new(2.5, Color32::WHITE));
            ui.label(egui::RichText::new(label).small().strong());
            ui.label(egui::RichText::new(&p.text).small().color(if killed { KILL } else { Color32::GRAY }));
        });
    }

    /// A fader (vertical or horizontal), over the parameter's 0..1.
    fn fader(&mut self, ui: &mut egui::Ui, name: &str, size: Vec2, vertical: bool) {
        let Some(p) = self.param(name) else { return };
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let to_v = |pos: Pos2| -> f64 {
            if vertical { f64::from(1.0 - (pos.y - rect.top()) / rect.height()) } else { f64::from((pos.x - rect.left()) / rect.width()) }
        };
        if resp.drag_started() { self.held = Some(p.id); }
        if let (true, Some(pos)) = (resp.dragged() || resp.clicked(), resp.interact_pointer_pos()) { self.set(p.id, to_v(pos)); }
        if resp.drag_stopped() { self.held = None; }
        if resp.double_clicked() { self.set(p.id, p.default); }
        let v = self.param(name).map_or(p.value, |q| q.value) as f32;
        let painter = ui.painter();
        let slot = if vertical { Rect::from_center_size(rect.center(), Vec2::new(6.0, rect.height())) } else { Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 6.0)) };
        painter.rect_filled(slot, 3.0, Color32::from_gray(25));
        // Tick marks, then the cap.
        for i in 0..=10 {
            let t = i as f32 / 10.0;
            let (a, b) = if vertical {
                let y = rect.bottom() - t * rect.height();
                (Pos2::new(rect.left() + 4.0, y), Pos2::new(rect.left() + 12.0, y))
            } else {
                let x = rect.left() + t * rect.width();
                (Pos2::new(x, rect.bottom() - 10.0), Pos2::new(x, rect.bottom() - 4.0))
            };
            painter.line_segment([a, b], Stroke::new(1.0, Color32::from_gray(80)));
        }
        let cap = if vertical {
            Rect::from_center_size(Pos2::new(rect.center().x, rect.bottom() - v * rect.height()), Vec2::new(rect.width() * 0.7, 14.0))
        } else {
            Rect::from_center_size(Pos2::new(rect.left() + v * rect.width(), rect.center().y), Vec2::new(18.0, rect.height() * 0.8))
        };
        painter.rect_filled(cap, 3.0, Color32::from_gray(200));
        if vertical { painter.line_segment([cap.left_center(), cap.right_center()], Stroke::new(2.0, Color32::BLACK)); }
        else { painter.line_segment([cap.center_top(), cap.center_bottom()], Stroke::new(2.0, Color32::BLACK)); }
    }

    fn strip(&mut self, ui: &mut egui::Ui, ch: &str) {
        ui.vertical(|ui| {
            ui.set_width(200.0);
            ui.vertical_centered(|ui| ui.heading(egui::RichText::new(format!("DECK {ch}")).color(ACCENT).strong()));
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(90.0);
                    self.knob(ui, &format!("{ch} Trim"), "TRIM");
                    ui.add_space(4.0);
                    self.knob(ui, &format!("{ch} High"), "HIGH");
                    self.knob(ui, &format!("{ch} Mid"), "MID");
                    self.knob(ui, &format!("{ch} Low"), "LOW");
                });
                ui.vertical(|ui| {
                    ui.add_space(40.0);
                    self.fader(ui, &format!("{ch} Fader"), Vec2::new(60.0, 230.0), true);
                    if let Some(p) = self.param(&format!("{ch} Fader")) {
                        ui.label(egui::RichText::new(format!("{:.0}%", p.value * 100.0)).small().color(Color32::GRAY));
                    }
                });
            });
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        // Docked beside Overdeck's decks: sit at the bottom, in the middle.
        if !self.placed && std::env::var_os("OPENDECK_TILE").is_some() {
            if let Some(m) = ctx.input(|i| i.viewport().monitor_size) {
                let size = Vec2::new(640.0, 470.0);
                ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(Pos2::new((m.x - size.x) / 2.0, m.y - size.y - 60.0)));
                self.placed = true;
            }
        }
        let connected = self.state.lock().unwrap().connected;
        if !connected { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }   // the host stopped: so do we
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.state.lock().unwrap().params.is_empty() {
                ui.label(if connected { "waiting for the mixer…" } else { "Overdeck has stopped." });
                return;
            }
            ui.horizontal(|ui| {
                self.strip(ui, "A");
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_width(150.0);
                    ui.vertical_centered(|ui| ui.label(egui::RichText::new("OVERDECK").strong()));
                    ui.add_space(30.0);
                    self.knob(ui, "Master", "MASTER");
                    ui.add_space(20.0);
                    if !connected { ui.colored_label(KILL, "host gone"); }
                });
                ui.separator();
                self.strip(ui, "B");
            });
            ui.add_space(8.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("CROSSFADER").small().strong());
                self.fader(ui, "Crossfader", Vec2::new(300.0, 34.0), false);
            });
        });
    }
}
