//! Dolby TrueHD from MAT frames, decoded with the `truehd` crate and routed
//! onto the speaker layout as a channel bed.

use truehd::process::decode::Decoder;
use truehd::process::extract::Extractor;
use truehd::process::parse::Parser;
use truehd::structs::channel::ChannelLabel;
use truehd::utils::errors::ExtractError;

use crate::layout::Layout;
use crate::mat::Mat;
use crate::render::{BedMix, Panner};

/// Decoded samples are 24-bit.
const SCALE: f32 = 8_388_608.0;

/// The largest presentation a stream offers: index 3 is the 16-channel one,
/// and the decoder falls back to the largest present.
const PRESENTATIONS: [bool; 4] = [false, false, false, true];

#[derive(Default)]
pub struct TrueHd {
    mat: Mat,
    extractor: Extractor,
    parser: Parser,
    decoder: Decoder,
    stream: Vec<u8>,
    interleaved: Vec<f32>,
    /// Routing for the channels of the stream being decoded.
    bed: Option<(Vec<ChannelLabel>, BedMix)>,
}

impl TrueHd {
    /// Decodes the access units in one MAT frame and appends the rendered
    /// frames to `out`. After an error the decoder starts over at the next
    /// major sync.
    pub fn decode(
        &mut self,
        mat_frame: &[u8],
        layout: &Layout,
        panner: &Panner,
        out: &mut Vec<f32>,
    ) -> Result<(), String> {
        self.stream.clear();
        self.mat.push(mat_frame, &mut self.stream)?;
        self.extractor.push_bytes(&self.stream);
        let result = self.drain(layout, panner, out);
        if result.is_err() {
            self.reset();
        }
        result
    }

    /// Forgets the stream so far, for when it stops or changes.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    fn drain(&mut self, layout: &Layout, panner: &Panner, out: &mut Vec<f32>) -> Result<(), String> {
        for frame in self.extractor.by_ref() {
            let frame = match frame {
                Ok(frame) => frame,
                // The rest of this access unit is in the next MAT frame.
                Err(ExtractError::InsufficientData) => break,
                Err(error) => return Err(error.to_string()),
            };
            let unit = self.parser.parse(&frame).map_err(|error| error.to_string())?;
            let decoded = self
                .decoder
                .decode_presentations(&unit, &PRESENTATIONS)
                .map_err(|error| error.to_string())?;
            let Some(decoded) = decoded.iter().rev().flatten().next() else {
                continue;
            };
            if decoded.is_duplicate {
                continue;
            }
            if decoded.sampling_frequency != 48_000 {
                return Err(format!("{} Hz audio is not supported", decoded.sampling_frequency));
            }
            if self.bed.as_ref().map(|(labels, _)| labels) != Some(&decoded.channel_labels) {
                let names: Vec<&str> = decoded.channel_labels.iter().map(name).collect();
                let bed = BedMix::new(layout, panner, &names);
                if !bed.dropped.is_empty() {
                    eprintln!("atmos67: no speaker for {}", bed.dropped.join(", "));
                }
                self.bed = Some((decoded.channel_labels.clone(), bed));
            }
            self.interleaved.clear();
            for frame in &decoded.pcm_data[..decoded.sample_length] {
                let channels = &frame[..decoded.channel_count];
                self.interleaved.extend(channels.iter().map(|s| *s as f32 / SCALE));
            }
            if let Some((_, bed)) = &self.bed {
                bed.mix(&self.interleaved, out);
            }
        }
        Ok(())
    }
}

/// The layout module's name for a TrueHD channel.
fn name(label: &ChannelLabel) -> &'static str {
    match label {
        ChannelLabel::L => "FL",
        ChannelLabel::R => "FR",
        ChannelLabel::C => "FC",
        ChannelLabel::LFE | ChannelLabel::LFE2 => "LFE",
        ChannelLabel::Ls | ChannelLabel::Lsd => "Ls",
        ChannelLabel::Rs | ChannelLabel::Rsd => "Rs",
        ChannelLabel::Lb => "Lb",
        ChannelLabel::Rb => "Rb",
        ChannelLabel::Cb => "Cs",
        ChannelLabel::Tfl => "TFL",
        ChannelLabel::Tfr => "TFR",
        ChannelLabel::Tsl => "Ltm",
        ChannelLabel::Tsr => "Rtm",
        ChannelLabel::Tbl => "TBL",
        ChannelLabel::Tbr => "TBR",
        ChannelLabel::Lsc => "FLC",
        ChannelLabel::Rsc => "FRC",
        ChannelLabel::Lw => "Lw",
        ChannelLabel::Rw => "Rw",
        ChannelLabel::Tc => "TC",
        ChannelLabel::Tfc => "TFC",
    }
}
