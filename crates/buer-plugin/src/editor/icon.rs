//! The wheel of Buer: five goat legs radiating at seventy-two degrees from a lion's head.
//!
//! Drawn rather than imported. The figure is line art in the same stroke weight as everything else
//! here, so a path is its native form; an SVG would need a rasteriser — `resvg` and its text shaper,
//! forty-odd crates — that draws at one size, which is the one thing this interface cannot promise:
//! `ui scale` runs from 100 % to 250 % and the host's DPI multiplies on top of that.
//!
//! And a path can turn. The wheel's rotation is the playhead, so the mark is also the transport
//! light, which is a thing the header needed anyway.
//!
//! CLAP has no icon extension in nih-plug's wrapper, so there is nowhere in the bundle for a file to
//! go even if one existed. This is the only picture of buer there is.

use nih_plug_egui::egui;

/// How many legs. It is five; the constant exists so the arithmetic below says why a number is 72.
const LEGS: usize = 5;
/// Rays around the head.
const MANE: usize = 14;

const HEAD: f32 = 0.30;
const MANE_TIP: f32 = 0.39;
const HIP: f32 = 0.34;
const KNEE: f32 = 0.66;
/// How far the knee is thrown sideways, as a share of the radius. This is what makes a ring of legs
/// read as turning rather than as a star.
const KNEE_THROW: f32 = 0.17;
/// Where the cannon ends. Short of the rim, because the toes reach past it — the figure has to
/// fit the circle it was given, and the test says so.
const HOOF: f32 = 0.84;
const HOOF_TOE: f32 = 0.09;

/// The whole figure. `rotation` is in turns, so a playhead's phase can be handed straight to it.
pub fn wheel(
    centre: egui::Pos2,
    radius: f32,
    rotation: f32,
    stroke: egui::Stroke,
) -> Vec<egui::Shape> {
    let mut shapes = Vec::with_capacity(LEGS * 3 + MANE + 4);
    for index in 0..LEGS {
        let turns = rotation + index as f32 / LEGS as f32;
        leg(&mut shapes, centre, radius, turns, stroke);
    }
    lion(&mut shapes, centre, radius, rotation, stroke);
    shapes
}

/// One leg: a thigh out of the hub, a knee thrown against the direction of travel, a cannon, and a
/// cloven hoof.
fn leg(
    shapes: &mut Vec<egui::Shape>,
    centre: egui::Pos2,
    radius: f32,
    turns: f32,
    stroke: egui::Stroke,
) {
    let (out, side) = axes(turns);
    let at = |along: f32, across: f32| {
        centre + (out * along + side * across) * radius
    };

    let hip = at(HIP, 0.0);
    let knee = at(KNEE, KNEE_THROW);
    let hoof = at(HOOF, -0.02);

    shapes.push(egui::Shape::line(vec![hip, knee, hoof], stroke));

    // The hoof, cloven: two short toes splayed either side of the leg's own direction.
    for &spread in &[-0.055, 0.055] {
        let toe = hoof + (out * HOOF_TOE + side * spread * 2.0) * radius;
        shapes.push(egui::Shape::line_segment([hoof, toe], stroke));
    }
}

/// The head: a ring, a mane, two eyes and a muzzle. Small enough that it is a mark rather than a
/// portrait — at sixteen points across, the mane is what carries it.
fn lion(
    shapes: &mut Vec<egui::Shape>,
    centre: egui::Pos2,
    radius: f32,
    turns: f32,
    stroke: egui::Stroke,
) {
    shapes.push(egui::Shape::circle_stroke(centre, radius * HEAD, stroke));

    for index in 0..MANE {
        let (out, _) = axes(turns + index as f32 / MANE as f32);
        shapes.push(egui::Shape::line_segment(
            [
                centre + out * radius * HEAD,
                centre + out * radius * MANE_TIP,
            ],
            stroke,
        ));
    }

    // The face does not turn with the wheel: a lion looking at you while its legs go round is the
    // figure; a lion revolving is a catherine wheel.
    let eye = radius * 0.035;
    for &side in &[-1.0f32, 1.0] {
        shapes.push(egui::Shape::circle_filled(
            centre + egui::vec2(side * radius * 0.11, -radius * 0.06),
            eye.max(0.6),
            stroke.color,
        ));
    }
    shapes.push(egui::Shape::line(
        vec![
            centre + egui::vec2(-radius * 0.09, radius * 0.11),
            centre + egui::vec2(0.0, radius * 0.16),
            centre + egui::vec2(radius * 0.09, radius * 0.11),
        ],
        stroke,
    ));
}

/// The outward and sideways unit vectors at a given angle, in turns clockwise from straight up.
///
/// Straight up rather than the usual straight right, because the figure has a top: at rest the
/// first leg points at twelve o'clock and the wheel reads as upright rather than as tilted.
fn axes(turns: f32) -> (egui::Vec2, egui::Vec2) {
    let angle = turns * std::f32::consts::TAU;
    let out = egui::vec2(angle.sin(), -angle.cos());
    (out, egui::vec2(-out.y, out.x))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn points(shapes: &[egui::Shape]) -> Vec<egui::Pos2> {
        let mut points = Vec::new();
        for shape in shapes {
            match shape {
                egui::Shape::Path(path) => points.extend(path.points.iter().copied()),
                egui::Shape::LineSegment { points: pair, .. } => points.extend(pair.iter().copied()),
                egui::Shape::Circle(circle) => {
                    let r = circle.radius;
                    points.push(circle.center + egui::vec2(r, 0.0));
                    points.push(circle.center + egui::vec2(-r, 0.0));
                    points.push(circle.center + egui::vec2(0.0, r));
                    points.push(circle.center + egui::vec2(0.0, -r));
                }
                _ => {}
            }
        }
        points
    }

    #[test]
    fn the_whole_figure_fits_inside_the_circle_it_was_given() {
        let centre = egui::pos2(50.0, 50.0);
        let radius = 40.0;
        for step in 0..16 {
            let shapes = wheel(
                centre,
                radius,
                step as f32 / 16.0,
                egui::Stroke::new(1.0, egui::Color32::WHITE),
            );
            for point in points(&shapes) {
                let distance = (point - centre).length();
                assert!(
                    distance <= radius + 0.001,
                    "a point {distance} out of a radius of {radius}"
                );
            }
        }
    }

    #[test]
    fn it_has_five_legs() {
        let shapes = wheel(
            egui::pos2(0.0, 0.0),
            10.0,
            0.0,
            egui::Stroke::new(1.0, egui::Color32::WHITE),
        );
        // A leg is one three-point path; the muzzle is the only other one.
        let paths = shapes
            .iter()
            .filter(|shape| matches!(shape, egui::Shape::Path(_)))
            .count();
        assert_eq!(paths, LEGS + 1);
    }

    #[test]
    fn a_leg_reaches_the_rim_and_the_hub_does_not() {
        let centre = egui::pos2(0.0, 0.0);
        let radius = 100.0;
        let shapes = wheel(
            centre,
            radius,
            0.0,
            egui::Stroke::new(1.0, egui::Color32::WHITE),
        );
        let furthest = points(&shapes)
            .iter()
            .map(|point| (*point - centre).length())
            .fold(0.0f32, f32::max);
        assert!(furthest > radius * 0.9, "the legs stop short at {furthest}");
    }

    #[test]
    fn the_figure_is_the_same_at_any_size_but_for_the_scale() {
        // Circles are left out: the eyes have a floor on their radius so that they are still a dot
        // at sixteen points across, and a floor is by definition not proportional.
        let small = wheel(
            egui::pos2(0.0, 0.0),
            10.0,
            0.25,
            egui::Stroke::new(1.0, egui::Color32::WHITE),
        );
        let large = wheel(
            egui::pos2(0.0, 0.0),
            100.0,
            0.25,
            egui::Stroke::new(1.0, egui::Color32::WHITE),
        );
        let strokes = |shapes: &[egui::Shape]| -> Vec<egui::Pos2> {
            points(
                &shapes
                    .iter()
                    .filter(|shape| !matches!(shape, egui::Shape::Circle(_)))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };
        for (a, b) in strokes(&small).iter().zip(strokes(&large).iter()) {
            assert!((a.x * 10.0 - b.x).abs() < 0.01);
            assert!((a.y * 10.0 - b.y).abs() < 0.01);
        }
    }
}
