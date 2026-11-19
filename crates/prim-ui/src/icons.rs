//! Small vector icons painted with egui shapes (no icon font, so they look the same
//! on every machine). Each draws inside a square `rect`.

use eframe::egui::{self, epaint::PathStroke, Color32, Painter, Pos2, Rect, Shape, Stroke};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Mic,
    MicOff,
    Headphones,
    HeadphonesOff,
    Cube,
    Play,
    Invite,
    Leave,
    Pencil,
    Door,
}

fn arc(c: Pos2, r: f32, a0: f32, a1: f32, n: usize) -> Vec<Pos2> {
    (0..=n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            Pos2::new(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect()
}

pub fn paint(p: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height());
    let c = rect.center();
    let st = Stroke::new((s / 11.0).max(1.4), color);
    let pt = |x: f32, y: f32| Pos2::new(c.x + x * s, c.y + y * s);
    let line = |a: Pos2, b: Pos2| Shape::line_segment([a, b], st);
    let slash = |p: &Painter| {
        let red = Color32::from_rgb(235, 90, 80);
        p.line_segment([pt(-0.42, -0.42), pt(0.42, 0.42)], Stroke::new(st.width + 0.8, red));
    };
    use std::f32::consts::PI;
    match icon {
        Icon::Mic | Icon::MicOff => {
            let body = Rect::from_center_size(pt(0.0, -0.12), egui::vec2(0.30 * s, 0.50 * s));
            p.rect_stroke(body, 0.15 * s, st, egui::StrokeKind::Middle);
            p.add(Shape::line(arc(pt(0.0, -0.02), 0.27 * s, 0.0, PI, 14), st));
            p.add(line(pt(0.0, 0.25), pt(0.0, 0.40)));
            p.add(line(pt(-0.14, 0.40), pt(0.14, 0.40)));
            if icon == Icon::MicOff {
                slash(p);
            }
        }
        Icon::Headphones | Icon::HeadphonesOff => {
            p.add(Shape::line(arc(pt(0.0, 0.02), 0.34 * s, PI, 2.0 * PI, 18), st));
            for sx in [-1.0f32, 1.0] {
                let r = Rect::from_center_size(pt(sx * 0.34, 0.14), egui::vec2(0.16 * s, 0.30 * s));
                p.rect_filled(r, 0.05 * s, color);
            }
            if icon == Icon::HeadphonesOff {
                slash(p);
            }
        }
        Icon::Cube => {
            let hex: Vec<Pos2> = (0..6)
                .map(|i| {
                    let a = PI / 6.0 + i as f32 * PI / 3.0;
                    pt(0.40 * a.cos(), 0.40 * a.sin())
                })
                .collect();
            p.add(Shape::closed_line(hex.clone(), st));
            p.add(line(pt(0.0, 0.0), hex[1]));
            p.add(line(pt(0.0, 0.0), hex[3]));
            p.add(line(pt(0.0, 0.0), hex[5]));
        }
        Icon::Play => {
            p.add(Shape::convex_polygon(
                vec![pt(-0.20, -0.30), pt(-0.20, 0.30), pt(0.32, 0.0)],
                color,
                PathStroke::NONE,
            ));
        }
        Icon::Invite => {
            p.circle_stroke(pt(-0.12, -0.18), 0.15 * s, st);
            p.add(Shape::line(arc(pt(-0.12, 0.40), 0.30 * s, PI * 1.1, PI * 1.9, 10), st));
            p.add(line(pt(0.28, -0.10), pt(0.28, 0.20)));
            p.add(line(pt(0.13, 0.05), pt(0.43, 0.05)));
        }
        Icon::Leave => {
            let door = Rect::from_min_max(pt(-0.40, -0.40), pt(0.05, 0.40));
            p.rect_stroke(door, 0.03 * s, st, egui::StrokeKind::Middle);
            p.add(line(pt(-0.05, 0.0), pt(0.42, 0.0)));
            p.add(line(pt(0.28, -0.14), pt(0.42, 0.0)));
            p.add(line(pt(0.28, 0.14), pt(0.42, 0.0)));
        }
        Icon::Pencil => {
            p.add(line(pt(-0.30, 0.32), pt(0.28, -0.26)));
            p.add(line(pt(-0.38, 0.40), pt(-0.30, 0.32)));
            p.add(line(pt(0.28, -0.26), pt(0.36, -0.34)));
        }
        Icon::Door => {
            let door = Rect::from_min_max(pt(-0.28, -0.40), pt(0.28, 0.40));
            p.rect_stroke(door, 0.03 * s, st, egui::StrokeKind::Middle);
            p.circle_filled(pt(0.14, 0.0), 0.04 * s, color);
        }
    }
}
