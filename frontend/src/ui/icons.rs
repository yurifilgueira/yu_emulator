//! The menu's icons, drawn with the painter (there is no icon font).

use eframe::egui::{pos2, vec2, Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind};

/// A compact disc: a ring with its centre hole.
pub fn disc(painter: &Painter, centre: Pos2, radius: f32, colour: Color32) {
    painter.circle_stroke(centre, radius, Stroke::new(radius * 0.3, colour));
    painter.circle_filled(centre, radius * 0.3, colour);
}

/// The outline of a folder with its tab, filling `rect`.
pub fn folder(painter: &Painter, rect: Rect, colour: Color32) {
    let (left, right, top, bottom) = (rect.left(), rect.right(), rect.top(), rect.bottom());
    let tab = rect.height() * 0.18;
    let points = vec![
        pos2(left, top),
        pos2(left + rect.width() * 0.38, top),
        pos2(left + rect.width() * 0.5, top + tab),
        pos2(right, top + tab),
        pos2(right, bottom),
        pos2(left, bottom),
    ];
    painter.add(Shape::closed_line(points, Stroke::new(1.6, colour)));
}

/// A "play" triangle pointing right, `size` wide.
pub fn play(painter: &Painter, centre: Pos2, size: f32, colour: Color32) {
    let half = size / 2.0;
    let points = vec![centre + vec2(-half, -half * 1.15), centre + vec2(half, 0.0), centre + vec2(-half, half * 1.15)];
    painter.add(Shape::convex_polygon(points, colour, Stroke::NONE));
}

/// One of a controller's four face buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Face {
    /// Cross on a PlayStation pad.
    Bottom,
    /// Circle on a PlayStation pad.
    Right,
}

/// The four face buttons as a diamond of dots, with one of them filled.
pub fn face_buttons(painter: &Painter, centre: Pos2, filled: Face, colour: Color32, dim: Color32) {
    const SPREAD: f32 = 6.0;
    const DOT: f32 = 2.6;
    let buttons = [
        (vec2(0.0, SPREAD), Some(Face::Bottom)),
        (vec2(SPREAD, 0.0), Some(Face::Right)),
        (vec2(0.0, -SPREAD), None),
        (vec2(-SPREAD, 0.0), None),
    ];
    for (offset, face) in buttons {
        if face == Some(filled) {
            painter.circle_filled(centre + offset, DOT + 0.6, colour);
        } else {
            painter.circle_stroke(centre + offset, DOT, Stroke::new(1.2, dim));
        }
    }
}

/// A directional pad: a plus sign.
pub fn d_pad(painter: &Painter, centre: Pos2, colour: Color32) {
    const ARM: f32 = 8.0;
    const THICKNESS: f32 = 3.0;
    painter.rect_filled(Rect::from_center_size(centre, vec2(2.0 * ARM, THICKNESS)), 1.0, colour);
    painter.rect_filled(Rect::from_center_size(centre, vec2(THICKNESS, 2.0 * ARM)), 1.0, colour);
}

/// A status light: filled when on, a ring when off.
pub fn status(painter: &Painter, centre: Pos2, on: bool, on_colour: Color32, off_colour: Color32) {
    if on {
        painter.circle_filled(centre, 4.0, on_colour);
    } else {
        painter.circle_stroke(centre, 3.4, Stroke::new(1.3, off_colour));
    }
}

/// An arrow up next to an arrow down, for the "up/down keys" key cap.
pub fn up_down(painter: &Painter, centre: Pos2, colour: Color32) {
    let stroke = Stroke::new(1.3, colour);
    const HALF: f32 = 5.0;
    const HEAD: f32 = 3.0;
    for (x, direction) in [(-3.5, -1.0), (3.5, 1.0)] {
        let tip = centre + vec2(x, direction * HALF);
        painter.line_segment([centre + vec2(x, -direction * HALF), tip], stroke);
        painter.line_segment([tip, tip + vec2(-HEAD, -direction * HEAD)], stroke);
        painter.line_segment([tip, tip + vec2(HEAD, -direction * HEAD)], stroke);
    }
}

/// A rectangle's outline, drawn inside its edge.
pub fn outline(painter: &Painter, rect: Rect, radius: f32, width: f32, colour: Color32) {
    painter.rect_stroke(rect, radius, Stroke::new(width, colour), StrokeKind::Inside);
}
