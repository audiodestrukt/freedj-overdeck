//! An all-in-one unit's screen (1024×600, the size of Music Tribe's Kick
//! panel; the Anima's isn't published): Overdeck's own UI, live.
//!
//! - PERFORM: a scrolling three-band waveform per deck, drawn from what the
//!   deck is playing right now (the host meters each deck's output; history
//!   only, so it runs up to a playhead with nothing after it), and a card
//!   per deck: title, pitch, time, CUE and PLAY that drive the real deck.
//! - BROWSE: the tracks beside the decks' (until the host runs a library),
//!   LOAD onto any deck.

use crate::{now_s, State, COL_S};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Rounding, Sense, Stroke, Vec2};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

const BG: Color32 = Color32::from_rgb(10, 12, 15);
const BAR: Color32 = Color32::from_rgb(13, 16, 20);
const CARD: Color32 = Color32::from_rgb(17, 21, 26);
const LINE: Color32 = Color32::from_rgb(29, 35, 43);
const DIM: Color32 = Color32::from_rgb(159, 179, 200);
const LOW: Color32 = Color32::from_rgb(47, 107, 255);
const MID: Color32 = Color32::from_rgb(255, 159, 28);
const HIGH: Color32 = Color32::from_rgb(244, 244, 244);
const GO: Color32 = Color32::from_rgb(52, 211, 153);
const CUE: Color32 = Color32::from_rgb(251, 191, 36);
const DECK: [Color32; 4] = [
    Color32::from_rgb(34, 195, 230),
    Color32::from_rgb(245, 158, 11),
    Color32::from_rgb(163, 230, 53),
    Color32::from_rgb(232, 121, 249),
];
const LETTER: [&str; 4] = ["A", "B", "C", "D"];

#[derive(PartialEq, Clone, Copy)]
enum Tab { Decks, Perform, Browse }

pub struct Screen {
    state: Arc<Mutex<State>>,
    out: UnixStream,
    n: usize,
    tab: Tab,
    sel: usize,
    /// The decks' own screens, when they draw into this one.
    decks: Option<crate::decks::Decks>,
}

impl Screen {
    pub fn new(state: Arc<Mutex<State>>, out: UnixStream, n: usize, screens: Option<String>) -> Screen {
        let n = n.clamp(1, 4);
        let decks = screens.map(|t| crate::decks::Decks::new(&t, n));
        let tab = if decks.is_some() { Tab::Decks } else { Tab::Perform };
        Screen { state, out, n, tab, sel: 0, decks }
    }

    fn send(&mut self, v: serde_json::Value) { let _ = writeln!(self.out, "{v}"); }

    fn header(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header").exact_height(40.0)
            .frame(egui::Frame::none().fill(BAR).stroke(Stroke::new(1.0, LINE)).inner_margin(egui::Margin::symmetric(14.0, 0.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(RichText::new("OVERDECK").size(20.0).strong().color(Color32::WHITE));
                    ui.add_space(12.0);
                    let tabs: &[(Tab, &str)] = if self.decks.is_some() { &[(Tab::Decks, "DECKS"), (Tab::Perform, "PERFORM"), (Tab::Browse, "BROWSE")] } else { &[(Tab::Perform, "PERFORM"), (Tab::Browse, "BROWSE")] };
                    for &(t, name) in tabs {
                        let on = self.tab == t;
                        let b = egui::Button::new(RichText::new(name).size(14.0).strong().color(if on { BG } else { DIM }))
                            .fill(if on { Color32::from_gray(232) } else { BAR })
                            .stroke(Stroke::new(1.0, if on { Color32::from_gray(232) } else { LINE }))
                            .min_size(Vec2::new(86.0, 28.0));
                        if ui.add(b).clicked() { self.tab = t; }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let st = self.state.lock().unwrap();
                        let playing = st.decks.iter().filter(|d| d.playing).count();
                        ui.label(RichText::new(format!("{} DECKS · {playing} PLAYING", self.n)).size(13.0).color(DIM));
                        // The mixer's crossfader, if a mixer runs.
                        if let Some(x) = st.params.iter().find(|p| p.name == "Crossfader") {
                            let (r, _) = ui.allocate_exact_size(Vec2::new(90.0, 14.0), Sense::hover());
                            let p = ui.painter();
                            p.rect_filled(r, 3.0, Color32::from_gray(30));
                            let cx = r.left() + r.width() * x.value as f32;
                            p.rect_filled(Rect::from_center_size(Pos2::new(cx, r.center().y), Vec2::new(8.0, 14.0)), 2.0, Color32::from_gray(220));
                            ui.label(RichText::new("XF").size(12.0).color(DIM));
                        }
                    });
                });
            });
    }

    fn perform(&mut self, ui: &mut egui::Ui) {
        // Waveform lanes.
        let n = self.n;
        for i in 0..n {
            let (lane, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 58.0), Sense::hover());
            let chip = Rect::from_min_size(lane.min, Vec2::new(28.0, lane.height()));
            let wave = Rect::from_min_max(Pos2::new(chip.right() + 8.0, lane.top()), lane.max);
            let p = ui.painter();
            p.rect_filled(chip, 4.0, DECK[i]);
            p.text(chip.center(), egui::Align2::CENTER_CENTER, LETTER[i], FontId::proportional(20.0), BG);
            p.rect_filled(wave, 4.0, Color32::from_rgb(15, 19, 24));
            let head = wave.left() + wave.width() * 0.78;
            p.rect_filled(Rect::from_min_max(Pos2::new(head, wave.top()), wave.max), Rounding::ZERO, Color32::from_rgb(12, 15, 19));
            let st = self.state.lock().unwrap();
            if let Some(d) = st.decks.get(i) {
                // Each column drawn where its time puts it, LAG behind now so
                // the newest data has always arrived: it slides every frame.
                const LAG: f64 = 0.060;
                const PX: f32 = 2.0;   // a column's width
                let mid_y = wave.center().y;
                let half = wave.height() / 2.0 - 3.0;
                let mut mesh = egui::Mesh::default();
                if let Some(base) = d.base {
                    let show = now_s() - LAG;
                    let first = d.total - d.wave.len() as u64;
                    for (k, c) in d.wave.iter().enumerate() {
                        let t = base + (first + k as u64) as f64 * COL_S;
                        let x = head - ((show - t) / COL_S) as f32 * PX;
                        if x > head || x < wave.left() { continue; }
                        for (v, gain, col) in [(c[0], 3.2, LOW), (c[1], 5.0, MID), (c[2], 12.0, HIGH)] {
                            let h = (v * gain).min(1.0) * half;
                            if h > 0.3 { mesh.add_colored_rect(Rect::from_min_max(Pos2::new(x - PX, mid_y - h), Pos2::new(x.min(head), mid_y + h)), col); }
                        }
                    }
                }
                p.add(egui::Shape::mesh(mesh));
                if !d.playing && d.wave.is_empty() {
                    p.text(Pos2::new(wave.left() + 12.0, mid_y), egui::Align2::LEFT_CENTER, "not playing", FontId::proportional(12.0), DIM);
                }
            }
            p.line_segment([Pos2::new(head, wave.top()), Pos2::new(head, wave.bottom())], Stroke::new(2.0, Color32::WHITE));
            drop(st);
            ui.add_space(4.0);
        }
        ui.add_space(4.0);
        // Deck cards.
        let gap = 8.0;
        let w = (ui.available_width() - gap * (n as f32 - 1.0)) / n as f32;
        let h = ui.available_height() - 4.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for i in 0..n { self.card(ui, i, Vec2::new(w, h)); }
        });
    }

    fn card(&mut self, ui: &mut egui::Ui, i: usize, size: Vec2) {
        let (title, playing, pitch, elapsed) = {
            let st = self.state.lock().unwrap();
            st.decks.get(i).map(|d| (d.title.clone(), d.playing, d.pitch.clone(), d.elapsed)).unwrap_or_default()
        };
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let p = ui.painter().clone();
        p.rect_filled(rect, 8.0, CARD);
        p.rect_stroke(rect, 8.0, Stroke::new(1.0, LINE));
        p.rect_filled(Rect::from_min_size(rect.min + Vec2::new(8.0, 0.0), Vec2::new(rect.width() - 16.0, 3.0)), 1.0, DECK[i]);
        let x0 = rect.left() + 12.0;
        let mut y = rect.top() + 14.0;
        p.text(Pos2::new(x0, y), egui::Align2::LEFT_TOP, LETTER[i], FontId::proportional(24.0), DECK[i]);
        let state = if playing { ("PLAYING", GO) } else { ("PAUSED", DIM) };
        p.text(Pos2::new(rect.right() - 12.0, y + 6.0), egui::Align2::RIGHT_TOP, state.0, FontId::proportional(11.0), state.1);
        y += 34.0;
        let fit = |s: &str, max: usize| if s.chars().count() > max { format!("{}…", s.chars().take(max - 1).collect::<String>()) } else { s.to_owned() };
        let chars = ((rect.width() - 24.0) / 7.6) as usize;
        p.text(Pos2::new(x0, y), egui::Align2::LEFT_TOP, fit(if title.is_empty() { "(no track)" } else { &title }, chars.max(8)), FontId::proportional(14.0), Color32::WHITE);
        y += 30.0;
        p.text(Pos2::new(x0, y), egui::Align2::LEFT_TOP, if pitch.is_empty() { "+0.00".into() } else { pitch.clone() }, FontId::proportional(30.0), Color32::WHITE);
        p.text(Pos2::new(rect.right() - 12.0, y + 12.0), egui::Align2::RIGHT_TOP, "PITCH %", FontId::proportional(11.0), DIM);
        y += 44.0;
        let t = elapsed.max(0.0) as u64;
        p.text(Pos2::new(x0, y), egui::Align2::LEFT_TOP, format!("{:02}:{:02}", t / 60, t % 60), FontId::proportional(18.0), DIM);
        // CUE (held: a momentary key wants press and release) and PLAY.
        let bw = (rect.width() - 30.0) / 2.0;
        let by = rect.bottom() - 12.0 - 46.0;
        let cue_r = Rect::from_min_size(Pos2::new(x0, by), Vec2::new(bw, 46.0));
        let play_r = Rect::from_min_size(Pos2::new(x0 + bw + 6.0, by), Vec2::new(bw, 46.0));
        let cue = ui.interact(cue_r, ui.id().with(("cue", i)), Sense::drag());
        let play = ui.interact(play_r, ui.id().with(("play", i)), Sense::click());
        if cue.drag_started() { self.send(serde_json::json!({ "deck": { "deck": i, "cue": true } })); }
        if cue.drag_stopped() { self.send(serde_json::json!({ "deck": { "deck": i, "cue": false } })); }
        if play.clicked() { self.send(serde_json::json!({ "deck": { "deck": i, "play": !playing } })); }
        let held = cue.dragged();
        p.rect_filled(cue_r, 6.0, if held { CUE } else { Color32::from_rgb(26, 20, 10) });
        p.rect_stroke(cue_r, 6.0, Stroke::new(1.0, CUE));
        p.text(cue_r.center(), egui::Align2::CENTER_CENTER, "CUE", FontId::proportional(17.0), if held { BG } else { CUE });
        p.rect_filled(play_r, 6.0, if playing { GO } else { Color32::from_rgb(13, 26, 21) });
        p.rect_stroke(play_r, 6.0, Stroke::new(1.0, GO));
        p.text(play_r.center(), egui::Align2::CENTER_CENTER, if playing { "PAUSE" } else { "PLAY" }, FontId::proportional(17.0), if playing { Color32::from_rgb(6, 35, 26) } else { GO });
    }

    fn browse(&mut self, ctx: &egui::Context) {
        let tracks = self.state.lock().unwrap().library.clone();
        egui::TopBottomPanel::bottom("load").exact_height(64.0)
            .frame(egui::Frame::none().fill(BAR).stroke(Stroke::new(1.0, LINE)).inner_margin(egui::Margin::symmetric(12.0, 10.0)))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("SELECTED").size(11.0).color(DIM));
                        let t = tracks.get(self.sel).map_or("—", |t| t.title.as_str());
                        ui.label(RichText::new(t).size(14.0).strong().color(Color32::WHITE));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        for i in (0..self.n).rev() {
                            let b = egui::Button::new(RichText::new(LETTER[i]).size(22.0).strong().color(BG)).fill(DECK[i]).min_size(Vec2::new(56.0, 44.0));
                            if ui.add(b).on_hover_text(format!("Load to deck {}", LETTER[i])).clicked() {
                                if let Some(t) = tracks.get(self.sel) {
                                    let path = t.path.clone();
                                    self.send(serde_json::json!({ "load": { "deck": i, "path": path } }));
                                    self.tab = if self.decks.is_some() { Tab::Decks } else { Tab::Perform };
                                }
                            }
                        }
                        ui.label(RichText::new("LOAD TO").size(12.0).color(DIM));
                    });
                });
            });
        egui::SidePanel::left("sources").exact_width(200.0).resizable(false)
            .frame(egui::Frame::none().fill(Color32::from_rgb(12, 15, 19)).stroke(Stroke::new(1.0, LINE)).inner_margin(10.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("SOURCES").size(11.0).strong().color(DIM));
                ui.add_space(6.0);
                let b = egui::Button::new(RichText::new(format!("This unit   {}", tracks.len())).size(13.0).color(Color32::WHITE))
                    .fill(Color32::from_rgb(27, 34, 43)).min_size(Vec2::new(180.0, 44.0));
                ui.add(b);
                ui.add_space(10.0);
                ui.label(RichText::new("Pro DJ Link players and USB sticks appear here when the host runs the library service.").size(11.0).color(DIM));
            });
        egui::CentralPanel::default().frame(egui::Frame::none().fill(BG)).show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for (i, t) in tracks.iter().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::click());
                    if resp.clicked() { self.sel = i; }
                    let on = self.sel == i;
                    let p = ui.painter();
                    p.rect_filled(r, Rounding::ZERO, if on { Color32::from_rgb(22, 50, 74) } else if i % 2 == 1 { Color32::from_rgb(12, 15, 19) } else { BG });
                    p.text(Pos2::new(r.left() + 14.0, r.center().y), egui::Align2::LEFT_CENTER, &t.title, FontId::proportional(14.0), Color32::WHITE);
                }
                if tracks.is_empty() {
                    ui.label(RichText::new("No tracks beside the decks' tracks.").color(DIM));
                }
            });
        });
    }
}

impl eframe::App for Screen {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if !self.state.lock().unwrap().connected { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }
        self.header(ctx);
        match self.tab {
            Tab::Perform => {
                egui::CentralPanel::default().frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin { left: 12.0, right: 12.0, top: 8.0, bottom: 8.0 }))
                    .show(ctx, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        self.perform(ui);
                    });
            }
            Tab::Browse => self.browse(ctx),
            Tab::Decks => {
                egui::CentralPanel::default().frame(egui::Frame::none().fill(BG).inner_margin(egui::Margin { left: 12.0, right: 12.0, top: 8.0, bottom: 8.0 }))
                    .show(ctx, |ui| {
                        if let Some(d) = &mut self.decks { d.show(ui, &self.state, &mut self.out); }
                    });
            }
        }
        crate::shot(ctx);
        ctx.request_repaint();   // the waveforms scroll: every frame (vsync paces it)
    }
}
