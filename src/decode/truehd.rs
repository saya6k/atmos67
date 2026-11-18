//! Dolby TrueHD from MAT frames, decoded with the `truehd` crate.
//!
//! A plain stream is routed onto the speaker layout as a channel bed. An
//! Atmos stream decodes to up to 16 object channels with metadata (OAMD)
//! saying where each one belongs: fixed bed channels first, then objects
//! with positions that move.

use truehd::process::decode::Decoder;
use truehd::process::extract::Extractor;
use truehd::process::parse::Parser;
use truehd::structs::channel::ChannelLabel;
use truehd::structs::oamd::{GAIN_MINUS_INFINITY, ObjectAudioMetadataPayload};
use truehd::utils::errors::ExtractError;

use crate::layout::Layout;
use crate::mat::Mat;
use crate::render::{BedMix, ObjectMix, Panner, channel_gains};

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
    /// Set once the stream has sent object metadata.
    objects: Option<ObjectMix>,
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
            // The objects and their scheduled movement, and the bed's routing,
            // stay: a damaged unit does not change what the stream carries.
            let (bed, objects) = (self.bed.take(), self.objects.take());
            self.reset();
            (self.bed, self.objects) = (bed, objects);
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
            if decoded.substream_info_changed {
                self.objects = None;
            }
            for payload in &decoded.oamd {
                let objects = self
                    .objects
                    .get_or_insert_with(|| ObjectMix::new(decoded.channel_count, layout.speakers.len()));
                schedule(objects, payload, layout, panner);
            }
            if self.objects.is_none()
                && self.bed.as_ref().map(|(labels, _)| labels) != Some(&decoded.channel_labels)
            {
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
            if let Some(objects) = &mut self.objects {
                objects.mix(&self.interleaved, decoded.channel_count, out);
            } else if let Some((_, bed)) = &self.bed {
                bed.mix(&self.interleaved, decoded.channel_count, out);
            }
        }
        Ok(())
    }
}

/// Bed channel names in the order OAMD numbers them.
const BED_CHANNELS: [&str; 17] = [
    "FL", "FR", "FC", "LFE", "Ls", "Rs", "Lb", "Rb", "TFL", "TFR", "Ltm", "Rtm", "TBL", "TBR", "Lw",
    "Rw", "LFE",
];

/// OAMD counts an update block's start in units of this many samples.
const BLOCK_OFFSET_SAMPLES: u64 = 32;

/// Schedules the gain changes one metadata payload asks for. Each object has
/// one entry per update block; blocks start at their own offsets and ramp.
fn schedule(mix: &mut ObjectMix, payload: &ObjectAudioMetadataPayload, layout: &Layout, panner: &Panner) {
    let Some(element) = &payload.object_element else {
        return;
    };
    let program = &payload.program_assignment;
    let beds: Vec<&str> = program
        .bed_assignment
        .iter()
        .flat_map(|bed| bed.to_index_vec())
        .map(|index| BED_CHANNELS[index])
        .collect();
    let positions = payload.get_damf_pos();
    let update = &element.md_update_info;
    for (object, blocks) in element.object_data.iter().enumerate() {
        for (index, (block, timing)) in blocks.iter().zip(&update.block_update_info).enumerate() {
            let gain = &block.object_basic_info.object_gain;
            let gain = if block.b_object_not_active || *gain == GAIN_MINUS_INFINITY {
                0.0
            } else {
                10f32.powf(f32::from(*gain) / 20.0)
            };
            let mut target = if let Some(bed) = beds.get(object) {
                channel_gains(layout, panner, bed)
            } else if object < beds.len() + program.num_isf_objects {
                // Intermediate spatial format channels are not rendered.
                vec![0.0; layout.speakers.len()]
            } else {
                panner.room_gains(positions[object][index])
            };
            for value in &mut target {
                *value *= gain;
            }
            let delay = payload.evo_sample_offset
                + update.sample_offset as u64
                + u64::from(timing.block_offset_factor_bits) * BLOCK_OFFSET_SAMPLES;
            mix.schedule(object, delay, u32::from(timing.ramp_duration), target);
        }
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
