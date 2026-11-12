//! Channel-based decoding through libavcodec: AC-3, E-AC-3 without Atmos
//! metadata, and the DTS family. The decoded channels are routed onto the
//! speaker layout as a bed.

use ffmpeg_next::format::Sample;
use ffmpeg_next::format::sample::Type;
use ffmpeg_next::{ChannelLayout, Packet, codec, frame};

use crate::layout::Layout;
use crate::render::{BedMix, Panner};

pub use ffmpeg_next::codec::Id;

/// libavcodec channel names by bit position in a channel mask, which is also
/// the order of the channels in a decoded frame.
const CHANNEL_NAMES: [(u32, &str); 22] = [
    (0, "FL"),
    (1, "FR"),
    (2, "FC"),
    (3, "LFE"),
    (4, "BL"),
    (5, "BR"),
    (6, "FLC"),
    (7, "FRC"),
    (8, "BC"),
    (9, "SL"),
    (10, "SR"),
    (11, "TC"),
    (12, "TFL"),
    (13, "TFC"),
    (14, "TFR"),
    (15, "TBL"),
    (16, "TBC"),
    (17, "TBR"),
    (31, "WL"),
    (32, "WR"),
    (36, "TSL"),
    (37, "TSR"),
];

pub struct Ffmpeg {
    decoder: codec::decoder::Audio,
    frame: frame::Audio,
    interleaved: Vec<f32>,
    /// Routing for the channel mask of the stream being decoded.
    bed: Option<(u64, BedMix)>,
}

impl Ffmpeg {
    pub fn new(id: Id) -> Result<Self, String> {
        ffmpeg_next::init().map_err(|error| error.to_string())?;
        // Damaged frames are reported by the caller, once, not by libavcodec.
        ffmpeg_next::log::set_level(ffmpeg_next::log::Level::Fatal);
        let codec = codec::decoder::find(id).ok_or(format!("FFmpeg has no {id:?} decoder"))?;
        let decoder = codec::Context::new_with_codec(codec)
            .decoder()
            .audio()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            decoder,
            frame: frame::Audio::empty(),
            interleaved: Vec::new(),
            bed: None,
        })
    }

    /// Decodes one access unit and appends the rendered frames to `out`.
    pub fn decode(
        &mut self,
        access_unit: &[u8],
        layout: &Layout,
        panner: &Panner,
        out: &mut Vec<f32>,
    ) -> Result<(), String> {
        self.decoder
            .send_packet(&Packet::copy(access_unit))
            .map_err(|error| error.to_string())?;
        while self.decoder.receive_frame(&mut self.frame).is_ok() {
            if self.frame.rate() != 48_000 {
                return Err(format!("{} Hz audio is not supported", self.frame.rate()));
            }
            let channels = usize::from(self.frame.channels());
            let mut mask = self.frame.channel_layout().bits();
            if mask.count_ones() as usize != channels {
                mask = ChannelLayout::default(channels as i32).bits();
            }
            if self.bed.as_ref().map(|(known, _)| *known) != Some(mask) {
                let names = channel_names(mask);
                let bed = BedMix::new(layout, panner, &names);
                if !bed.dropped.is_empty() {
                    eprintln!("atmos67: no speaker for {}", bed.dropped.join(", "));
                }
                self.bed = Some((mask, bed));
            }
            self.interleave(channels)?;
            if let Some((_, bed)) = &self.bed {
                bed.mix(&self.interleaved, out);
            }
        }
        Ok(())
    }

    /// Forgets the stream so far, for when it stops or changes.
    pub fn reset(&mut self) {
        self.decoder.flush();
    }

    fn interleave(&mut self, channels: usize) -> Result<(), String> {
        let samples = self.frame.samples();
        self.interleaved.clear();
        self.interleaved.resize(samples * channels, 0.0);
        let frame = &self.frame;
        let mut fill = |value: &dyn Fn(usize, usize) -> f32| {
            for (index, sample) in self.interleaved.iter_mut().enumerate() {
                *sample = value(index / channels, index % channels);
            }
        };
        match frame.format() {
            Sample::F32(Type::Planar) => fill(&|i, c| frame.plane::<f32>(c)[i]),
            Sample::F32(Type::Packed) => fill(&|i, c| frame.plane::<f32>(0)[i * channels + c]),
            Sample::I32(Type::Planar) => fill(&|i, c| frame.plane::<i32>(c)[i] as f32 / I32_SCALE),
            Sample::I32(Type::Packed) => {
                fill(&|i, c| frame.plane::<i32>(0)[i * channels + c] as f32 / I32_SCALE);
            }
            Sample::I16(Type::Planar) => {
                fill(&|i, c| f32::from(frame.plane::<i16>(c)[i]) / I16_SCALE);
            }
            Sample::I16(Type::Packed) => {
                fill(&|i, c| f32::from(frame.plane::<i16>(0)[i * channels + c]) / I16_SCALE);
            }
            other => return Err(format!("unsupported sample format {other:?}")),
        }
        Ok(())
    }
}

const I32_SCALE: f32 = 2_147_483_648.0;
const I16_SCALE: f32 = 32_768.0;

fn channel_names(mask: u64) -> Vec<&'static str> {
    let mut names: Vec<&str> = (0..64)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| {
            CHANNEL_NAMES
                .iter()
                .find(|(known, _)| *known == bit)
                .map_or("unknown", |(_, name)| *name)
        })
        .collect();
    // A stream with one surround pair may label it "back" (5.1) or "side"
    // (5.1(side)); either way it is the surround pair.
    if !names.contains(&"SL") && !names.contains(&"SR") {
        for name in &mut names {
            *name = match *name {
                "BL" => "SL",
                "BR" => "SR",
                other => other,
            };
        }
    }
    names
}

/// Splits an E-AC-3 burst into the packets libavcodec expects: each
/// independent frame of the main programme with the dependent frames that
/// extend it. Frames of other programmes are left out.
pub fn eac3_access_units(burst: &[u8]) -> Vec<&[u8]> {
    let mut units: Vec<&[u8]> = Vec::new();
    let mut start = None;
    let mut position = 0;
    while position + 6 <= burst.len() && burst[position..position + 2] == [0x0B, 0x77] {
        let header = &burst[position..];
        if header[5] >> 3 <= 10 {
            // Plain AC-3 has no length field here; it is a frame on its own.
            return vec![burst];
        }
        let stream_type = header[2] >> 6;
        let substream = (header[2] >> 3) & 7;
        let length = ((usize::from(header[2] & 7) << 8 | usize::from(header[3])) + 1) * 2;
        let dependent = stream_type == 1;
        if !dependent {
            if let Some(start) = start.take() {
                units.push(&burst[start..position]);
            }
            if substream == 0 {
                start = Some(position);
            }
        }
        position += length;
    }
    if let Some(start) = start {
        units.push(&burst[start..position.min(burst.len())]);
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An E-AC-3 frame header followed by padding, `words` 16-bit words long.
    fn frame(stream_type: u8, substream: u8, words: usize) -> Vec<u8> {
        let size = words - 1;
        let mut out = vec![0u8; words * 2];
        out[0] = 0x0B;
        out[1] = 0x77;
        out[2] = stream_type << 6 | substream << 3 | (size >> 8) as u8;
        out[3] = size as u8;
        out[5] = 16 << 3; // bsid 16: E-AC-3
        out
    }

    #[test]
    fn dependent_frames_stay_with_their_independent_frame() {
        let burst = [
            frame(0, 0, 8),
            frame(1, 0, 4),
            frame(0, 1, 6), // another programme: skipped with its dependents
            frame(1, 1, 3),
            frame(0, 0, 5),
        ]
        .concat();
        let units = eac3_access_units(&burst);
        assert_eq!(units.iter().map(|u| u.len()).collect::<Vec<_>>(), [24, 10]);
        assert_eq!(units[0], &burst[..24]);
        assert_eq!(units[1], &burst[42..]);
    }

    #[test]
    fn surround_pair_is_named_side_however_the_stream_labels_it() {
        assert_eq!(channel_names(0x3F), ["FL", "FR", "FC", "LFE", "SL", "SR"]);
        assert_eq!(channel_names(0x60F), ["FL", "FR", "FC", "LFE", "SL", "SR"]);
        assert_eq!(
            channel_names(0x63F),
            ["FL", "FR", "FC", "LFE", "BL", "BR", "SL", "SR"]
        );
    }
}
