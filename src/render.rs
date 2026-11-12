//! Amplitude panning onto an arbitrary speaker layout (VBAP).
//!
//! A source direction is reproduced by the three speakers whose directions
//! enclose it, with gains that add up to the source vector. Two virtual
//! speakers, straight up and straight down, close the sphere for layouts
//! without speakers there; whatever lands on them is shared among their
//! neighbours.

use crate::layout::{Direction, Layout, canonical, dot, nominal_direction, normalize};

const UP: Direction = [0.0, 0.0, 1.0];
const DOWN: Direction = [0.0, 0.0, -1.0];

/// Numerical slack when testing whether a direction lies in a triplet.
const EDGE_TOLERANCE: f64 = 1e-9;

struct Triplet {
    /// Indices into `Panner::directions`.
    nodes: [usize; 3],
    /// Rows turn a source direction into the three gains.
    inverse: [Direction; 3],
}

pub struct Panner {
    /// Full-range speakers followed by the virtual ones.
    directions: Vec<Direction>,
    /// Output channel of each full-range speaker.
    outputs: Vec<usize>,
    output_count: usize,
    triplets: Vec<Triplet>,
}

impl Panner {
    pub fn new(layout: &Layout) -> Self {
        let mut directions = Vec::new();
        let mut outputs = Vec::new();
        for (index, speaker) in layout.speakers.iter().enumerate() {
            if !speaker.lfe {
                directions.push(speaker.direction);
                outputs.push(index);
            }
        }
        for pole in [UP, DOWN] {
            if directions.iter().all(|d| dot(*d, pole) < 0.9998) {
                directions.push(pole);
            }
        }

        // The faces of the convex hull of the speaker directions tile the
        // sphere without overlap: a triplet is a face when every other
        // speaker is on the same side of its plane. Faces whose plane passes
        // through the listener cannot pan and are left out.
        let mut triplets = Vec::new();
        let n = directions.len();
        for a in 0..n {
            for b in a + 1..n {
                for c in b + 1..n {
                    let (pa, pb, pc) = (directions[a], directions[b], directions[c]);
                    let normal = cross(sub(pb, pa), sub(pc, pa));
                    let sides: Vec<f64> = (0..n)
                        .filter(|i| ![a, b, c].contains(i))
                        .map(|i| dot(normal, sub(directions[i], pa)))
                        .collect();
                    let is_face = sides.iter().all(|s| *s < EDGE_TOLERANCE)
                        || sides.iter().all(|s| *s > -EDGE_TOLERANCE);
                    if let (true, Some(inverse)) = (is_face, invert([pa, pb, pc])) {
                        triplets.push(Triplet {
                            nodes: [a, b, c],
                            inverse,
                        });
                    }
                }
            }
        }
        Self {
            directions,
            outputs,
            output_count: layout.speakers.len(),
            triplets,
        }
    }

    /// Per-output gains for a source in `direction`; their squares sum to one.
    pub fn gains(&self, direction: Direction) -> Vec<f32> {
        let direction = normalize(direction);
        // The enclosing triplet has no negative gain. Where triplets overlap,
        // or none encloses the source, take the one the source is deepest in.
        let best = self
            .triplets
            .iter()
            .map(|triplet| (triplet, gains_in(&triplet.inverse, direction)))
            .max_by(|(_, a), (_, b)| min(a).total_cmp(&min(b)));
        let mut node_gains = vec![0.0f64; self.directions.len()];
        if let Some((triplet, gains)) = best {
            for (node, gain) in triplet.nodes.iter().zip(gains) {
                node_gains[*node] = gain.max(0.0);
            }
        }

        // Share each virtual speaker's power among the real speakers it
        // forms triplets with.
        let real = self.outputs.len();
        let mut power: Vec<f64> = node_gains[..real].iter().map(|g| g * g).collect();
        for (virtual_node, gain) in node_gains.iter().enumerate().skip(real) {
            if *gain == 0.0 {
                continue;
            }
            let mut neighbours: Vec<usize> = self
                .triplets
                .iter()
                .filter(|t| t.nodes.contains(&virtual_node))
                .flat_map(|t| t.nodes)
                .filter(|node| *node < real)
                .collect();
            neighbours.sort_unstable();
            neighbours.dedup();
            for node in &neighbours {
                power[*node] += gain * gain / neighbours.len() as f64;
            }
        }
        let mut total: f64 = power.iter().sum();
        if total == 0.0 {
            // Outside every triplet, as behind a front-only layout.
            if let Some(nearest) = (0..real).max_by(|a, b| {
                dot(self.directions[*a], direction).total_cmp(&dot(self.directions[*b], direction))
            }) {
                power[nearest] = 1.0;
                total = 1.0;
            }
        }
        let mut out = vec![0.0f32; self.output_count];
        for (node, p) in power.iter().enumerate() {
            out[self.outputs[node]] = (p / total.max(f64::MIN_POSITIVE)).sqrt() as f32;
        }
        out
    }
}

/// A named channel within this angle of a speaker goes to that speaker alone.
const SNAP_DEGREES: f64 = 5.0;

/// Fixed routing of named input channels (a channel bed) onto a layout.
pub struct BedMix {
    /// For each input channel, the (output, gain) pairs it feeds.
    routes: Vec<Vec<(usize, f32)>>,
    output_count: usize,
    /// Input channels with nowhere to go: LFE without a subwoofer, or a
    /// name with no known position.
    pub dropped: Vec<String>,
}

impl BedMix {
    pub fn new(layout: &Layout, panner: &Panner, channels: &[&str]) -> Self {
        let mut dropped = Vec::new();
        let routes = channels
            .iter()
            .map(|label| {
                let name = canonical(label);
                let route = if name == "LFE" {
                    let subs = layout.speakers.iter().enumerate().filter(|(_, s)| s.lfe);
                    subs.map(|(index, _)| (index, 1.0)).collect()
                } else if let Some(index) = layout
                    .speakers
                    .iter()
                    .position(|s| !s.lfe && canonical(&s.name) == name)
                {
                    vec![(index, 1.0)]
                } else if let Some(direction) = nominal_direction(name) {
                    let near = layout.speakers.iter().position(|s| {
                        !s.lfe && dot(s.direction, direction) > SNAP_DEGREES.to_radians().cos()
                    });
                    match near {
                        Some(index) => vec![(index, 1.0)],
                        None => {
                            let gains = panner.gains(direction);
                            (0..gains.len())
                                .filter(|i| gains[*i] > 0.0)
                                .map(|i| (i, gains[i]))
                                .collect()
                        }
                    }
                } else {
                    Vec::new()
                };
                if route.is_empty() {
                    dropped.push((*label).to_owned());
                }
                route
            })
            .collect();
        Self {
            routes,
            output_count: layout.speakers.len(),
            dropped,
        }
    }

    /// Appends the mixed frames of interleaved `input` to `output`.
    pub fn mix(&self, input: &[f32], output: &mut Vec<f32>) {
        for frame in input.chunks_exact(self.routes.len()) {
            let start = output.len();
            output.resize(start + self.output_count, 0.0);
            for (sample, route) in frame.iter().zip(&self.routes) {
                for (index, gain) in route {
                    output[start + index] += sample * gain;
                }
            }
        }
    }
}

fn min(gains: &[f64; 3]) -> f64 {
    gains[0].min(gains[1]).min(gains[2])
}

fn gains_in(inverse: &[Direction; 3], direction: Direction) -> [f64; 3] {
    [
        dot(inverse[0], direction),
        dot(inverse[1], direction),
        dot(inverse[2], direction),
    ]
}

/// Inverse of the matrix whose columns are the three directions, or `None`
/// when they lie in one plane through the listener.
fn invert(columns: [Direction; 3]) -> Option<[Direction; 3]> {
    let [a, b, c] = columns;
    let determinant = dot(a, cross(b, c));
    if determinant.abs() < 1e-6 {
        return None;
    }
    Some([cross(b, c), cross(c, a), cross(a, b)].map(|row| row.map(|x| x / determinant)))
}

fn cross(u: Direction, v: Direction) -> Direction {
    [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ]
}

fn sub(u: Direction, v: Direction) -> Direction {
    [u[0] - v[0], u[1] - v[1], u[2] - v[2]]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{Speaker, direction};

    fn layout(speakers: &[(&str, f64, f64)]) -> Layout {
        Layout {
            speakers: speakers
                .iter()
                .map(|(name, azimuth, elevation)| Speaker {
                    name: (*name).to_owned(),
                    direction: direction(*azimuth, *elevation),
                    lfe: *name == "LFE",
                })
                .collect(),
        }
    }

    fn seven_one_four() -> Layout {
        layout(&[
            ("FL", -30.0, 0.0),
            ("FR", 30.0, 0.0),
            ("FC", 0.0, 0.0),
            ("LFE", 0.0, 0.0),
            ("Lb", -135.0, 0.0),
            ("Rb", 135.0, 0.0),
            ("Ls", -90.0, 0.0),
            ("Rs", 90.0, 0.0),
            ("TFL", -45.0, 45.0),
            ("TFR", 45.0, 45.0),
            ("TBL", -135.0, 45.0),
            ("TBR", 135.0, 45.0),
        ])
    }

    fn active(gains: &[f32]) -> Vec<usize> {
        (0..gains.len()).filter(|i| gains[*i] > 1e-4).collect()
    }

    fn power(gains: &[f32]) -> f32 {
        gains.iter().map(|g| g * g).sum()
    }

    #[test]
    fn a_source_at_a_speaker_uses_only_that_speaker() {
        let layout = seven_one_four();
        let panner = Panner::new(&layout);
        for (index, speaker) in layout.speakers.iter().enumerate() {
            if speaker.lfe {
                continue;
            }
            let gains = panner.gains(speaker.direction);
            assert_eq!(active(&gains), [index], "{}", speaker.name);
            assert!((gains[index] - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn a_source_between_two_speakers_uses_those_two() {
        let panner = Panner::new(&seven_one_four());
        let gains = panner.gains(direction(15.0, 0.0));
        assert_eq!(active(&gains), [1, 2]); // FR and FC
        assert!((gains[1] - gains[2]).abs() < 1e-6);
        let gains = panner.gains(direction(-112.5, 0.0));
        assert_eq!(active(&gains), [4, 6]); // Lb and Ls
    }

    #[test]
    fn power_is_preserved_everywhere_and_lfe_stays_silent() {
        let panner = Panner::new(&seven_one_four());
        for azimuth in (-180..180).step_by(15) {
            for elevation in [-30.0, 0.0, 20.0, 45.0, 70.0, 90.0] {
                let gains = panner.gains(direction(f64::from(azimuth), elevation));
                assert!((power(&gains) - 1.0).abs() < 1e-5, "{azimuth} {elevation}");
                assert_eq!(gains[3], 0.0);
            }
        }
    }

    #[test]
    fn overhead_is_shared_by_the_ceiling_speakers() {
        let panner = Panner::new(&seven_one_four());
        assert_eq!(active(&panner.gains(UP)), [8, 9, 10, 11]);
    }

    #[test]
    fn a_flat_layout_pans_height_by_azimuth() {
        let flat = layout(&[
            ("FL", -30.0, 0.0),
            ("FR", 30.0, 0.0),
            ("FC", 0.0, 0.0),
            ("LFE", 0.0, 0.0),
            ("Ls", -110.0, 0.0),
            ("Rs", 110.0, 0.0),
        ]);
        let panner = Panner::new(&flat);
        let gains = panner.gains(direction(-30.0, 40.0));
        assert!(gains[0] > 0.8, "{gains:?}");
        assert!((power(&gains) - 1.0).abs() < 1e-5);
        // Behind the listener, between the surrounds.
        assert_eq!(active(&panner.gains(direction(180.0, 0.0))), [4, 5]);
    }

    #[test]
    fn stereo_still_gets_every_source() {
        let panner = Panner::new(&layout(&[("FL", -30.0, 0.0), ("FR", 30.0, 0.0)]));
        for azimuth in [-170.0, -90.0, 0.0, 90.0, 170.0] {
            assert!((power(&panner.gains(direction(azimuth, 0.0))) - 1.0).abs() < 1e-5);
        }
        assert_eq!(active(&panner.gains(direction(-90.0, 0.0))), [0]);
    }

    const SEVEN_ONE: [&str; 8] = ["FL", "FR", "FC", "LFE", "BL", "BR", "SL", "SR"];

    fn mixed(layout: &Layout, channels: &[&str], input: &[f32]) -> (Vec<f32>, Vec<String>) {
        let bed = BedMix::new(layout, &Panner::new(layout), channels);
        let mut output = Vec::new();
        bed.mix(input, &mut output);
        (output, bed.dropped)
    }

    #[test]
    fn a_bed_on_its_own_layout_is_passed_through_by_name() {
        let input = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let (output, dropped) = mixed(&seven_one_four(), &SEVEN_ONE, &input);
        assert_eq!(output, [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 0.0, 0.0, 0.0, 0.0]);
        assert!(dropped.is_empty());
    }

    #[test]
    fn missing_rear_speakers_fold_into_the_surrounds() {
        let five_one = layout(&[
            ("FL", -30.0, 0.0),
            ("FR", 30.0, 0.0),
            ("FC", 0.0, 0.0),
            ("LFE", 0.0, 0.0),
            ("Ls", -110.0, 0.0),
            ("Rs", 110.0, 0.0),
        ]);
        let only_back_left = [0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        let (output, _) = mixed(&five_one, &SEVEN_ONE, &only_back_left);
        assert!(output[4] > 0.9, "{output:?}");
        assert!(output[..4].iter().all(|g| *g == 0.0));
        // The side channel matches the surround speaker by name, not angle.
        let only_side_left = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let (output, _) = mixed(&five_one, &SEVEN_ONE, &only_side_left);
        assert_eq!(output, [0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn a_channel_near_a_differently_named_speaker_snaps_to_it() {
        let custom = layout(&[("Left", -27.0, 0.0), ("Right", 27.0, 0.0), ("Mid", 0.0, 0.0)]);
        let (output, _) = mixed(&custom, &["FL"], &[1.0]);
        assert_eq!(output, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn lfe_without_a_subwoofer_and_unknown_names_are_dropped() {
        let stereo = layout(&[("FL", -30.0, 0.0), ("FR", 30.0, 0.0)]);
        let (output, dropped) = mixed(&stereo, &["FL", "LFE", "Mystery"], &[1.0, 1.0, 1.0]);
        assert_eq!(output, [1.0, 0.0]);
        assert_eq!(dropped, ["LFE", "Mystery"]);
    }
}
