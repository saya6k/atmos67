//! Output speaker layout and the nominal directions of named channels.
//!
//! Directions are unit vectors with +X right, +Y front, +Z up. Angles here are
//! in degrees with azimuth positive to the right. Layout files use the
//! OpenJOC convention instead, where positive azimuth is to the left.

use openjoc_scene::SpeakerLayout;

pub type Direction = [f64; 3];

pub struct Speaker {
    pub name: String,
    pub direction: Direction,
    pub lfe: bool,
}

pub struct Layout {
    pub speakers: Vec<Speaker>,
}

pub fn direction(azimuth: f64, elevation: f64) -> Direction {
    let (az, el) = (azimuth.to_radians(), elevation.to_radians());
    [az.sin() * el.cos(), az.cos() * el.cos(), el.sin()]
}

/// Maps the channel names used by FFmpeg, TrueHD and layout files onto one
/// spelling, so a decoded channel can be matched to a speaker by name.
pub fn canonical(label: &str) -> &str {
    match label {
        "L" => "FL",
        "R" => "FR",
        "C" => "FC",
        "LFE1" | "LFE2" => "LFE",
        "SL" | "Lss" => "Ls",
        "SR" | "Rss" => "Rs",
        "BL" | "Lrs" | "Lsr" => "Lb",
        "BR" | "Rrs" | "Rsr" => "Rb",
        "BC" => "Cs",
        "Ltf" => "TFL",
        "Rtf" => "TFR",
        "Ltr" => "TBL",
        "Rtr" => "TBR",
        "TSL" | "Lts" => "Ltm",
        "TSR" | "Rts" => "Rtm",
        "WL" => "Lw",
        "WR" => "Rw",
        other => other,
    }
}

/// Where a channel sits in a reference room. `None` for LFE and unknown names.
pub fn nominal_direction(label: &str) -> Option<Direction> {
    let (azimuth, elevation) = match canonical(label) {
        "FL" => (-30.0, 0.0),
        "FR" => (30.0, 0.0),
        "FC" => (0.0, 0.0),
        "Lw" => (-60.0, 0.0),
        "Rw" => (60.0, 0.0),
        "Ls" => (-90.0, 0.0),
        "Rs" => (90.0, 0.0),
        "Lb" => (-135.0, 0.0),
        "Rb" => (135.0, 0.0),
        "Cs" => (180.0, 0.0),
        "TFL" => (-45.0, 45.0),
        "TFR" => (45.0, 45.0),
        "Ltm" => (-90.0, 45.0),
        "Rtm" => (90.0, 45.0),
        "TBL" => (-135.0, 45.0),
        "TBR" => (135.0, 45.0),
        _ => return None,
    };
    Some(direction(azimuth, elevation))
}

impl Layout {
    /// Presets carry only channel names, so their speakers take the nominal
    /// directions; custom layouts carry their own coordinates.
    pub fn from_openjoc(layout: &SpeakerLayout) -> Result<Self, String> {
        let coordinates = layout.channel_coordinates();
        let speakers = layout
            .channel_labels()
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let lfe = layout.lfe_indices().contains(&index);
                let direction = match coordinates.get(index) {
                    Some(position) => from_room_position(position.map(f64::from)),
                    None if lfe => [0.0, 1.0, 0.0],
                    None => nominal_direction(name)
                        .ok_or_else(|| format!("no known direction for speaker {name}"))?,
                };
                Ok(Speaker {
                    name: name.clone(),
                    direction,
                    lfe,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(Self { speakers })
    }
}

/// OpenJOC gives custom speakers as room positions: x from 0 (left) to 1
/// (right), y from 0 (front) to 1 (back), and a scaled height whose sign
/// alone is used here, since the speakers sit on the unit sphere.
fn from_room_position([x, y, z]: [f64; 3]) -> Direction {
    let (right, front) = (2.0 * x - 1.0, 1.0 - 2.0 * y);
    let up = (1.0 - right * right - front * front).max(0.0).sqrt();
    normalize([right, front, up.copysign(z)])
}

pub fn normalize(v: Direction) -> Direction {
    let length = dot(v, v).sqrt();
    v.map(|c| c / length)
}

pub fn dot(a: Direction, b: Direction) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Direction, b: Direction) -> bool {
        dot(a, b) > 0.9999
    }

    #[test]
    fn preset_speakers_take_nominal_directions() {
        let layout = Layout::from_openjoc(&SpeakerLayout::preset("7.1.4").unwrap()).unwrap();
        assert_eq!(layout.speakers.len(), 12);
        let by_name = |name: &str| layout.speakers.iter().find(|s| s.name == name).unwrap();
        assert!(close(by_name("FL").direction, direction(-30.0, 0.0)));
        assert!(close(by_name("TBR").direction, direction(135.0, 45.0)));
        assert!(by_name("LFE").lfe);
        assert_eq!(layout.speakers.iter().filter(|s| s.lfe).count(), 1);
    }

    #[test]
    fn custom_layout_keeps_its_own_angles() {
        let json = include_str!("../tests/fixtures/layout-5.1.2.json");
        let layout = Layout::from_openjoc(&SpeakerLayout::from_json_str(json).unwrap()).unwrap();
        let names: Vec<&str> = layout.speakers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["FL", "FR", "FC", "LFE", "SL", "SR", "TL", "TR"]);
        // The file says FL is 28 degrees to the left and TR 80 to the right.
        assert!(close(layout.speakers[0].direction, direction(-28.0, 0.0)));
        assert!(close(layout.speakers[5].direction, direction(112.0, 0.0)));
        assert!(close(layout.speakers[6].direction, direction(-80.0, 45.0)));
        assert!(layout.speakers[3].lfe);
    }

    #[test]
    fn ffmpeg_and_truehd_names_share_one_spelling() {
        assert_eq!(canonical("SL"), "Ls");
        assert_eq!(canonical("BR"), "Rb");
        assert_eq!(canonical("Ltf"), "TFL");
        assert_eq!(canonical("FL"), "FL");
        assert!(nominal_direction("LFE").is_none());
        assert!(close(nominal_direction("BL").unwrap(), direction(-135.0, 0.0)));
    }
}
