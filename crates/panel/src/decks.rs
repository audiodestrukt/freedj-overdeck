//! DECKS: each deck's own screen, live, drawn into the unit's (the display
//! contract, `overdeck-display`), with the mixer under them.
//!
//! The decks draw their screens off-screen into shared memory the host
//! made; this shows the newest frame of each in its tile and sends the
//! touches that land on a tile back to that deck, in the tile's own 0..1
//! coordinates, so the deck's screen works as it would on the deck.

use crate::State;
use display::{Screen, Touch};
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Stroke, TextureHandle, Vec2};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

const LETTER: [&str; 4] = ["A", "B", "C", "D"];
const ACCENT: Color32 = Color32::from_rgb(0, 190, 220);
const KILL: Color32 = Color32::from_rgb(230, 60, 50);
const DIM: Color32 = Color32::from_rgb(159, 179, 200);

struct Live {
    screen: Option<Screen>,
    path: std::path::PathBuf,
    tex: Option<TextureHandle>,
    seen: u32,
    buf: Vec<u8>,
    /// The finger on this tile, where it was last sent.
    down: Option<(f32, f32)>,
}

pub struct Decks {
    live: Vec<Live>,
    held: Option<u32>,
}

/// Where each deck's tile goes in `area`, and what's left under them.
pub fn tiles(n: usize, area: Rect) -> (Vec<Rect>, Option<Rect>) {
    let gap = 8.0;
    let aspect = 600.0 / 1024.0;
    if n <= 2 {
        let w = (area.width() - gap * (n as f32 - 1.0).max(0.0)) / n.max(1) as f32;
        let h = w * aspect;
        let rects = (0..n).map(|i| Rect::from_min_size(area.min + Vec2::new(i as f32 * (w + gap), 0.0), Vec2::new(w, h))).collect();
        let rest = Rect::from_min_max(Pos2::new(area.left(), area.top() + h + gap), area.max);
        (rects, (rest.height() > 120.0).then_some(rest))
    } else {
        let h = (area.height() - gap) / 2.0;
        let w = h / aspect;
        let x0 = area.center().x - w - gap / 2.0;
        let rects = (0..n.min(4)).map(|i| Rect::from_min_size(Pos2::new(x0 + (i % 2) as f32 * (w + gap), area.top() + (i / 2) as f32 * (h + gap)), Vec2::new(w, h))).collect();
        (rects, None)
    }
}

impl Decks {
    pub fn new(template: &str, n: usize) -> Decks {
        let live = (1..=n as u32).map(|i| {
            let path = Screen::path_for(template, i);
            Live { screen: Screen::open(&path).ok(), path, tex: None, seen: u32::MAX, buf: Vec::new(), down: None }
        }).collect();
        Decks { live, held: None }
    }

    /// Bring each deck's newest frame in.
    fn refresh(&mut self, ctx: &egui::Context) {
        for (i, l) in self.live.iter_mut().enumerate() {
            if l.screen.is_none() { l.screen = Screen::open(&l.path).ok(); }
            let Some(s) = &l.screen else { continue };
            if !s.attached() || s.frame() == l.seen { continue; }
            let Some(f) = s.read_frame(&mut l.buf) else { continue };
            l.seen = f;
            let (w, h) = s.size();
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &l.buf);
            match &mut l.tex {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => l.tex = Some(ctx.load_texture(format!("deck{i}"), img, egui::TextureOptions::LINEAR)),
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, state: &Arc<Mutex<State>>, out: &mut UnixStream) {
        self.refresh(ui.ctx());
        let area = ui.available_rect_before_wrap();
        let (rects, rest) = tiles(self.live.len(), area);
        for (i, r) in rects.iter().enumerate() {
            let p = ui.painter();
            p.rect_filled(*r, 4.0, Color32::from_rgb(4, 5, 7));
            let l = &mut self.live[i];
            match &l.tex {
                Some(t) => { p.image(t.id(), *r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE); }
                None => { p.text(r.center(), egui::Align2::CENTER_CENTER, format!("deck {} starting…", LETTER[i]), FontId::proportional(14.0), DIM); }
            }
            p.rect_stroke(*r, 4.0, Stroke::new(1.0, Color32::from_rgb(29, 35, 43)));
            // Touches go to the deck: press, drag, lift (Sense::drag, so a press is sent on the press).
            let resp = ui.interact(*r, ui.id().with(("deck", i)), Sense::drag());
            let at = resp.interact_pointer_pos().or(resp.hover_pos()).map(|q| (((q.x - r.left()) / r.width()).clamp(0.0, 1.0), ((q.y - r.top()) / r.height()).clamp(0.0, 1.0)));
            if let (Some(s), Some((x, y))) = (&mut l.screen, at) {
                if resp.drag_started() || (resp.is_pointer_button_down_on() && l.down.is_none()) {
                    s.push_touch(Touch::Down(x, y));
                    l.down = Some((x, y));
                } else if resp.dragged() && l.down != Some((x, y)) && l.down.is_some() {
                    s.push_touch(Touch::Move(x, y));
                    l.down = Some((x, y));
                }
            }
            if l.down.is_some() && !resp.is_pointer_button_down_on() && !resp.dragged() {
                let (x, y) = at.or(l.down).unwrap();
                if let Some(s) = &mut l.screen { s.push_touch(Touch::Up(x, y)); }
                l.down = None;
            }
        }
        if let Some(rest) = rest { self.mixer(ui, &rects, rest, state, out); }
        ui.allocate_rect(area, Sense::hover());
    }

    // ── The mixer, under the decks ───────────────────────────────────────────

    fn mixer(&mut self, ui: &mut egui::Ui, rects: &[Rect], rest: Rect, state: &Arc<Mutex<State>>, out: &mut UnixStream) {
        if state.lock().unwrap().params.is_empty() {
            ui.painter().text(rest.center(), egui::Align2::CENTER_CENTER, "no mixer", FontId::proportional(13.0), DIM);
            return;
        }
        let xf_h = 44.0;
        for (i, r) in rects.iter().enumerate() {
            let ch = LETTER[i];
            let col = Rect::from_min_max(Pos2::new(r.left(), rest.top()), Pos2::new(r.right(), rest.bottom() - xf_h));
            let p = ui.painter();
            p.rect_filled(col, 6.0, Color32::from_rgb(17, 21, 26));
            // Knobs left to right, the channel fader at the inner edge (by the crossfader).
            let knobs = ["Trim", "High", "Mid", "Low"];
            let fader_w = 54.0;
            let inner_right = i % 2 == 0;
            let kx0 = if inner_right { col.left() + 10.0 } else { col.left() + fader_w + 20.0 };
            let kw = (col.width() - fader_w - 30.0) / 4.0;
            for (k, name) in knobs.iter().enumerate() {
                let c = Pos2::new(kx0 + kw * (k as f32 + 0.5), col.top() + 46.0);
                self.knob(ui, state, out, &format!("{ch} {name}"), &name.to_uppercase(), c, (kw * 0.42).min(24.0));
            }
            let fx = if inner_right { col.right() - fader_w - 6.0 } else { col.left() + 6.0 };
            let fr = Rect::from_min_max(Pos2::new(fx, col.top() + 10.0), Pos2::new(fx + fader_w, col.bottom() - 10.0));
            self.fader(ui, state, out, &format!("{ch} Fader"), fr, true);
            ui.painter().text(Pos2::new(kx0, col.bottom() - 12.0), egui::Align2::LEFT_BOTTOM, format!("CH {ch}"), FontId::proportional(13.0), ACCENT);
        }
        let xf = Rect::from_center_size(Pos2::new(rest.center().x, rest.bottom() - xf_h / 2.0), Vec2::new(280.0, 32.0));
        ui.painter().text(Pos2::new(xf.left() - 10.0, xf.center().y), egui::Align2::RIGHT_CENTER, "XF", FontId::proportional(12.0), DIM);
        self.fader(ui, state, out, "Crossfader", xf, false);
        // Master: a small knob at the end of the crossfader's row, labelled to its left.
        let m = Pos2::new(rest.right() - 24.0, rest.bottom() - xf_h / 2.0);
        ui.painter().text(Pos2::new(m.x - 22.0, m.y), egui::Align2::RIGHT_CENTER, "MASTER", FontId::proportional(11.0), DIM);
        self.knob(ui, state, out, "Master", "", m, 13.0);
    }

    fn set(&mut self, state: &Arc<Mutex<State>>, out: &mut UnixStream, id: u32, v: f64) {
        let v = v.clamp(0.0, 1.0);
        let _ = writeln!(out, "{}", serde_json::json!({ "set": { "id": id, "value": v } }));
        if let Some(p) = state.lock().unwrap().params.iter_mut().find(|p| p.id == id) { p.value = v; }
    }

    #[allow(clippy::too_many_arguments)]
    fn knob(&mut self, ui: &mut egui::Ui, state: &Arc<Mutex<State>>, out: &mut UnixStream, name: &str, label: &str, c: Pos2, r: f32) {
        let Some(p) = state.lock().unwrap().params.iter().find(|p| p.name == name).cloned() else { return };
        let rect = Rect::from_center_size(c, Vec2::splat(r * 2.0 + 6.0));
        let resp = ui.interact(rect, ui.id().with(name), Sense::click_and_drag());
        if resp.drag_started() { self.held = Some(p.id); }
        if resp.dragged() { self.set(state, out, p.id, p.value - f64::from(resp.drag_delta().y) / 150.0); }
        if resp.drag_stopped() { self.held = None; }
        if resp.double_clicked() { self.set(state, out, p.id, p.default); }
        let v = state.lock().unwrap().params.iter().find(|q| q.id == p.id).map_or(p.value, |q| q.value) as f32;
        let killed = p.text.contains("kill");
        let painter = ui.painter();
        painter.circle_filled(c, r, Color32::from_gray(40));
        let a0 = std::f32::consts::PI * 0.75;
        let sweep = std::f32::consts::PI * 1.5;
        let pts: Vec<Pos2> = (0..=24).map(|i| c + Vec2::angled(a0 + sweep * v * i as f32 / 24.0) * (r + 2.0)).collect();
        painter.add(egui::Shape::line(pts, Stroke::new(3.0, if killed { KILL } else { ACCENT })));
        painter.line_segment([c, c + Vec2::angled(a0 + sweep * v) * (r - 3.0)], Stroke::new(2.0, Color32::WHITE));
        if label.is_empty() { return; }
        painter.text(Pos2::new(c.x, c.y + r + 6.0), egui::Align2::CENTER_TOP, label, FontId::proportional(10.0), DIM);
        painter.text(Pos2::new(c.x, c.y + r + 18.0), egui::Align2::CENTER_TOP, &p.text, FontId::proportional(10.0), if killed { KILL } else { Color32::GRAY });
    }

    fn fader(&mut self, ui: &mut egui::Ui, state: &Arc<Mutex<State>>, out: &mut UnixStream, name: &str, rect: Rect, vertical: bool) {
        let Some(p) = state.lock().unwrap().params.iter().find(|p| p.name == name).cloned() else { return };
        let resp = ui.interact(rect, ui.id().with(name), Sense::click_and_drag());
        let to_v = |q: Pos2| -> f64 { if vertical { f64::from(1.0 - (q.y - rect.top()) / rect.height()) } else { f64::from((q.x - rect.left()) / rect.width()) } };
        if let (true, Some(q)) = (resp.dragged() || resp.clicked(), resp.interact_pointer_pos()) { self.set(state, out, p.id, to_v(q)); }
        if resp.double_clicked() { self.set(state, out, p.id, p.default); }
        let v = state.lock().unwrap().params.iter().find(|q| q.id == p.id).map_or(p.value, |q| q.value) as f32;
        let painter = ui.painter();
        let slot = if vertical { Rect::from_center_size(rect.center(), Vec2::new(6.0, rect.height())) } else { Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 6.0)) };
        painter.rect_filled(slot, 3.0, Color32::from_gray(25));
        let cap = if vertical {
            Rect::from_center_size(Pos2::new(rect.center().x, rect.bottom() - v * rect.height()), Vec2::new(rect.width() * 0.7, 14.0))
        } else {
            Rect::from_center_size(Pos2::new(rect.left() + v * rect.width(), rect.center().y), Vec2::new(16.0, rect.height() * 0.8))
        };
        painter.rect_filled(cap, 3.0, Color32::from_gray(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_decks_sit_side_by_side_at_their_shape_with_the_mixer_under_them() {
        let area = Rect::from_min_size(Pos2::new(12.0, 48.0), Vec2::new(1000.0, 544.0));
        let (r, rest) = tiles(2, area);
        assert_eq!(r.len(), 2);
        assert!((r[0].width() / r[0].height() - 1024.0 / 600.0).abs() < 0.01);
        assert!(r[0].right() < r[1].left());
        assert!(rest.unwrap().top() > r[0].bottom());
    }

    #[test]
    fn four_decks_fill_a_grid_inside_the_screen() {
        let area = Rect::from_min_size(Pos2::new(12.0, 48.0), Vec2::new(1000.0, 544.0));
        let (r, rest) = tiles(4, area);
        assert_eq!(r.len(), 4);
        assert!(rest.is_none());
        for t in &r { assert!(area.expand(0.5).contains_rect(*t), "{t:?}"); }
    }
}
