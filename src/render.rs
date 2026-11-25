//! Amplitude panning onto an arbitrary speaker layout (VBAP).
//!
//! A source direction is reproduced by the three speakers whose directions
//! enclose it, with gains that add up to the source vector. Two virtual
//! speakers, straight up and straight down, close the sphere for layouts
//! without speakers there; whatever lands on them is shared among their
//! neighbours.

use std::collections::VecDeque;

use crate::layout::{Direction, Layout, canonical, direction, dot, nominal_direction, normalize};

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

impl Panner {
    /// Gains for a position in the Dolby Atmos room: a cube with x from -1
    /// (left wall) to 1, y from 1 (front wall) to -1, z from 0 (ear level)
    /// to 1 (ceiling). Sources on the walls or ceiling are panned by
    /// direction; towards the middle of the room they spread over all
    /// speakers. Object size, snap and zone constraints are not modelled.
    pub fn room_gains(&self, [x, y, z]: [f64; 3]) -> Vec<f32> {
        let radius = x.abs().max(y.abs());
        let elevation = z.atan2(radius).to_degrees();
        // The cube's front corners are the front left and right speakers,
        // which sit at 30 degrees; the ceiling corners sit at 45.
        let front = 30.0 + 15.0 * (elevation.abs() / 45.0).min(1.0);
        let cube_azimuth = if radius > 0.0 { x.atan2(y).to_degrees() } else { 0.0 };
        let azimuth = match cube_azimuth.abs() {
            a if a <= 45.0 => a * front / 45.0,
            a if a <= 90.0 => front + (a - 45.0) * (90.0 - front) / 45.0,
            a => a,
        }
        .copysign(cube_azimuth);
        let mut gains = self.gains(direction(azimuth, elevation));
        let on_surface = radius.max(z.abs()).min(1.0) as f32;
        let spread = (1.0 - on_surface) / self.outputs.len().max(1) as f32;
        for output in &self.outputs {
            let gain = &mut gains[*output];
            *gain = (on_surface * *gain * *gain + spread).sqrt();
        }
        gains
    }
}

/// Mixes audio objects whose gains change over time.
pub struct ObjectMix {
    output_count: usize,
    objects: Vec<Object>,
    /// Frames mixed so far.
    clock: u64,
}

#[derive(Default)]
struct Object {
    gains: Vec<f32>,
    step: Vec<f32>,
    ramp_left: u32,
    updates: VecDeque<Update>,
}

struct Update {
    at: u64,
    ramp: u32,
    target: Vec<f32>,
}

impl ObjectMix {
    pub fn new(objects: usize, output_count: usize) -> Self {
        let object = || Object {
            gains: vec![0.0; output_count],
            step: vec![0.0; output_count],
            ..Object::default()
        };
        Self {
            output_count,
            objects: (0..objects).map(|_| object()).collect(),
            clock: 0,
        }
    }

    /// Moves an object to `target` gains, starting `delay` frames from now
    /// and arriving `ramp` frames after that. Updates queued for that time
    /// or later came from older metadata, which this one supersedes.
    pub fn schedule(&mut self, object: usize, delay: u64, ramp: u32, target: Vec<f32>) {
        if let Some(object) = self.objects.get_mut(object) {
            let at = self.clock + delay;
            object.updates.retain(|update| update.at < at);
            object.updates.push_back(Update {
                at,
                ramp,
                target,
            });
        }
    }

    /// Appends the mixed frames of interleaved `input`, one channel per
    /// object, to `output`.
    pub fn mix(&mut self, input: &[f32], channels: usize, output: &mut Vec<f32>) {
        for frame in input.chunks_exact(channels) {
            let start = output.len();
            output.resize(start + self.output_count, 0.0);
            let mixed = &mut output[start..];
            for (sample, object) in frame.iter().zip(&mut self.objects) {
                while object.updates.front().is_some_and(|u| u.at <= self.clock) {
                    let update = object.updates.pop_front().expect("checked above");
                    if update.ramp == 0 {
                        object.gains = update.target;
                        object.ramp_left = 0;
                    } else {
                        for ((step, gain), target) in
                            object.step.iter_mut().zip(&object.gains).zip(&update.target)
                        {
                            *step = (target - gain) / update.ramp as f32;
                        }
                        object.ramp_left = update.ramp;
                    }
                }
                if object.ramp_left > 0 {
                    for (gain, step) in object.gains.iter_mut().zip(&object.step) {
                        *gain += step;
                    }
                    object.ramp_left -= 1;
                }
                for (out, gain) in mixed.iter_mut().zip(&object.gains) {
                    *out += sample * gain;
                }
            }
            self.clock += 1;
        }
    }
}

/// A named channel within this angle of a speaker goes to that speaker alone.
const SNAP_DEGREES: f64 = 5.0;

/// The ITU-R BS.775 downmix coefficient, -3 dB.
const FOLD: f32 = std::f32::consts::FRAC_1_SQRT_2;

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
                let route = route(layout, panner, label, channels);
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

    /// Appends the mixed frames of interleaved `input`, `channels` to a
    /// frame, to `output`. Channels beyond the named ones are dropped.
    pub fn mix(&self, input: &[f32], channels: usize, output: &mut Vec<f32>) {
        if channels == 0 {
            return;
        }
        for frame in input.chunks_exact(channels) {
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

/// The (output, gain) pairs a named channel of `bed` feeds; empty when it
/// has nowhere to go.
fn route(layout: &Layout, panner: &Panner, label: &str, bed: &[&str]) -> Vec<(usize, f32)> {
    let name = canonical(label);
    if name == "LFE" {
        // Each of N subwoofers plays 1/N, so together they sound like one.
        let subs: Vec<usize> = layout.speakers.iter().enumerate().filter(|(_, s)| s.lfe).map(|(i, _)| i).collect();
        let share = 1.0 / subs.len().max(1) as f32;
        return subs.into_iter().map(|index| (index, share)).collect();
    }
    if let Some(index) = speaker(layout, name) {
        return vec![(index, 1.0)];
    }
    if let Some(route) = fold(layout, name, bed) {
        return route;
    }
    let Some(direction) = nominal_direction(name) else {
        return Vec::new();
    };
    let gains = panner.gains(direction);
    (0..gains.len()).filter(|i| gains[*i] > 0.0).map(|i| (i, gains[i])).collect()
}

/// The speaker that plays channel `name` as it is: one of that name, or one
/// within `SNAP_DEGREES` of where the channel belongs.
fn speaker(layout: &Layout, name: &str) -> Option<usize> {
    let named = |s: &crate::layout::Speaker| !s.lfe && canonical(&s.name) == name;
    layout.speakers.iter().position(named).or_else(|| {
        let direction = nominal_direction(name)?;
        let near = |s: &crate::layout::Speaker| {
            !s.lfe && dot(s.direction, direction) > SNAP_DEGREES.to_radians().cos()
        };
        layout.speakers.iter().position(near)
    })
}

/// The standard downmix (ITU-R BS.775, Dolby Lo/Ro and 7.1 to 5.1) of a
/// channel with no speaker of its own: the centre splits between the fronts,
/// a side or back surround goes to the other surround on its side, or else
/// to the front on its side. A surround joins the speaker at -3 dB when the
/// bed has a channel for that speaker too, and keeps its level when it only
/// moves, as the back pair of a 5.1(back) bed on side speakers. `None` where
/// there is no standard rule (heights, wides) or no speaker to fold into.
fn fold(layout: &Layout, name: &str, bed: &[&str]) -> Option<Vec<(usize, f32)>> {
    let into = |target: &str| {
        let shared = bed.iter().any(|b| canonical(b) == target);
        speaker(layout, target).map(|index| vec![(index, if shared { FOLD } else { 1.0 })])
    };
    match name {
        "FC" => Some(vec![(speaker(layout, "FL")?, FOLD), (speaker(layout, "FR")?, FOLD)]),
        "Ls" => into("Lb").or_else(|| into("FL")),
        "Rs" => into("Rb").or_else(|| into("FR")),
        "Lb" => into("Ls").or_else(|| into("FL")),
        "Rb" => into("Rs").or_else(|| into("FR")),
        _ => None,
    }
}

/// Per-output gains for a named channel of `bed`, as `route` gives them.
pub fn channel_gains(layout: &Layout, panner: &Panner, label: &str, bed: &[&str]) -> Vec<f32> {
    let mut gains = vec![0.0; layout.speakers.len()];
    for (index, gain) in route(layout, panner, label, bed) {
        gains[index] = gain;
    }
    gains
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
    use crate::layout::Speaker;

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
        bed.mix(input, channels.len(), &mut output);
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
        // At -3 dB, since the side channel plays there too, and none of it
        // on the right.
        let only_back_left = [0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        let (output, _) = mixed(&five_one, &SEVEN_ONE, &only_back_left);
        assert_eq!(output, [0.0, 0.0, 0.0, 0.0, FOLD, 0.0]);
        // The side channel matches the surround speaker by name, not angle.
        let only_side_left = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let (output, _) = mixed(&five_one, &SEVEN_ONE, &only_side_left);
        assert_eq!(output, [0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    }

    #[test]
    fn a_five_one_bed_on_stereo_is_the_itu_downmix() {
        let stereo = layout(&[("FL", -30.0, 0.0), ("FR", 30.0, 0.0)]);
        let bed = ["FL", "FR", "FC", "LFE", "SL", "SR"];
        let gains = |channel: usize| {
            let mut input = [0.0; 6];
            input[channel] = 1.0;
            mixed(&stereo, &bed, &input).0
        };
        assert_eq!(gains(0), [1.0, 0.0]);
        assert_eq!(gains(2), [FOLD, FOLD]);
        assert_eq!(gains(3), [0.0, 0.0]);
        assert_eq!(gains(4), [FOLD, 0.0]);
        assert_eq!(gains(5), [0.0, FOLD]);
        // A 7.1 bed folds its back pair into the fronts the same way.
        let (output, _) = mixed(&stereo, &SEVEN_ONE, &[0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        assert_eq!(output, [0.0, FOLD]);
    }

    #[test]
    fn a_back_pair_alone_moves_to_the_side_speakers_at_full_level() {
        let five_one = layout(&[
            ("FL", -30.0, 0.0),
            ("FR", 30.0, 0.0),
            ("FC", 0.0, 0.0),
            ("LFE", 0.0, 0.0),
            ("Ls", -110.0, 0.0),
            ("Rs", 110.0, 0.0),
        ]);
        let back = ["FL", "FR", "FC", "LFE", "BL", "BR"];
        let (output, _) = mixed(&five_one, &back, &[0.0, 0.0, 0.0, 0.0, 1.0, 2.0]);
        assert_eq!(output, [0.0, 0.0, 0.0, 0.0, 1.0, 2.0]);
    }

    #[test]
    fn heights_without_speakers_are_still_panned() {
        let stereo = layout(&[("FL", -30.0, 0.0), ("FR", 30.0, 0.0), ("FC", 0.0, 0.0)]);
        let (output, dropped) = mixed(&stereo, &["TFL"], &[1.0]);
        assert!(dropped.is_empty());
        assert!(output[0] > output[1] && (power(&output) - 1.0).abs() < 1e-5, "{output:?}");
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
    #[test]
    fn atmos_room_corners_are_the_speakers() {
        let panner = Panner::new(&seven_one_four());
        // FL FR FC LFE Lb Rb Ls Rs TFL TFR TBL TBR
        let corners = [
            ([-1.0, 1.0, 0.0], 0),
            ([1.0, 1.0, 0.0], 1),
            ([0.0, 1.0, 0.0], 2),
            ([-1.0, -1.0, 0.0], 4),
            ([1.0, -1.0, 0.0], 5),
            ([-1.0, 0.0, 0.0], 6),
            ([1.0, 0.0, 0.0], 7),
            ([-1.0, 1.0, 1.0], 8),
            ([1.0, 1.0, 1.0], 9),
            ([-1.0, -1.0, 1.0], 10),
            ([1.0, -1.0, 1.0], 11),
        ];
        for (position, speaker) in corners {
            assert_eq!(active(&panner.room_gains(position)), [speaker], "{position:?}");
        }
    }

    #[test]
    fn the_middle_of_the_room_is_every_speaker_equally() {
        let panner = Panner::new(&seven_one_four());
        let gains = panner.room_gains([0.0, 0.0, 0.0]);
        assert_eq!(active(&gains).len(), 11);
        assert!((power(&gains) - 1.0).abs() < 1e-5);
        assert!((gains[0] - gains[11]).abs() < 1e-6);
        // Halfway to the left wall leans left without leaving the others silent.
        let gains = panner.room_gains([-0.5, 0.0, 0.0]);
        assert!(gains[6] > 2.0 * gains[7] && gains[7] > 0.0, "{gains:?}");
        assert!((power(&gains) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_bed_short_of_names_drops_the_rest_and_keeps_its_frames() {
        let stereo = layout(&[("FL", -30.0, 0.0), ("FR", 30.0, 0.0)]);
        let bed = BedMix::new(&stereo, &Panner::new(&stereo), &["FL"]);
        let mut output = Vec::new();
        bed.mix(&[1.0, 9.0, 2.0, 9.0], 2, &mut output);
        assert_eq!(output, [1.0, 0.0, 2.0, 0.0]);
        let none = BedMix::new(&stereo, &Panner::new(&stereo), &[]);
        // No names: the frame still comes out, silent, instead of a panic.
        none.mix(&[1.0, 2.0], 2, &mut output);
        assert_eq!(output, [1.0, 0.0, 2.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn two_subwoofers_share_the_lfe() {
        let two = layout(&[("FL", -30.0, 0.0), ("LFE", 0.0, 0.0), ("LFE", 0.0, 0.0)]);
        let (output, _) = mixed(&two, &["LFE"], &[1.0]);
        assert_eq!(output, [0.0, 0.5, 0.5]);
    }

    #[test]
    fn newer_metadata_supersedes_later_updates() {
        let mut mix = ObjectMix::new(1, 1);
        mix.schedule(0, 6, 0, vec![0.25]);
        mix.schedule(0, 2, 0, vec![1.0]);
        let mut out = Vec::new();
        mix.mix(&[1.0; 8], 1, &mut out);
        assert_eq!(out, [0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn objects_jump_ramp_and_wait_as_scheduled() {
        let mut mix = ObjectMix::new(2, 2);
        mix.schedule(0, 0, 0, vec![1.0, 0.0]);
        mix.schedule(1, 2, 0, vec![0.0, 0.5]);
        mix.schedule(0, 4, 4, vec![0.0, 1.0]);
        let mut out = Vec::new();
        mix.mix(&[1.0, 1.0].repeat(9), 2, &mut out);
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        let right: Vec<f32> = out.iter().skip(1).step_by(2).copied().collect();
        assert_eq!(left, [1.0, 1.0, 1.0, 1.0, 0.75, 0.5, 0.25, 0.0, 0.0]);
        assert_eq!(right, [0.0, 0.0, 0.5, 0.5, 0.75, 1.0, 1.25, 1.5, 1.5]);
    }
}
