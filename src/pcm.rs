//! Plain PCM in the capture: what Kodi plays itself, such as stereo music and
//! its menu sounds, rather than passes through as a bitstream. It is played
//! as a stereo bed, so the correction applies to it as to everything else.
//!
//! Kodi passes bitstreams through as 16-bit samples only, so capture in
//! another sample format is PCM throughout. In 16-bit capture the two can
//! follow each other without the device being reopened, and are told apart
//! by the IEC 61937 sync word that starts every burst. Bursts, and the pause
//! bursts between them, come at least every 47 ms (an AC-3 or DTS frame at
//! 44.1 kHz), so capture that has gone `QUIET_MS` without one is PCM. While
//! it is, everything before the next sync word is PCM; the bitstream begins
//! with that word. Bitstream data is never played as PCM: what follows the
//! last sync word, the end of the last burst, is dropped when PCM resumes.

use ffmpeg_next::format::Sample;
use ffmpeg_next::format::sample::Type;
use ffmpeg_next::software::resampling;
use ffmpeg_next::{ChannelLayout, frame};

use crate::iec61937::SYNC;

/// Rate of everything downstream.
pub const RATE: u32 = 48_000;

/// Capture without a sync word for this long is PCM.
const QUIET_MS: u64 = 50;

/// Capture sample formats, by their ALSA names.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    S16,
    S32,
    F32,
}

impl Format {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "S16_LE" => Self::S16,
            "S32_LE" => Self::S32,
            "FLOAT_LE" => Self::F32,
            _ => return None,
        })
    }

    pub fn bytes(self) -> usize {
        match self {
            Self::S16 => 2,
            Self::S32 | Self::F32 => 4,
        }
    }
}

/// Splits 16-bit capture into PCM, returned, and bitstream, left to the
/// IEC 61937 depacketizer which reads the same capture.
pub struct Gate {
    frame: usize,
    quiet: usize,
    /// Capture not yet given out as PCM or dropped.
    pending: Vec<u8>,
    /// Bytes fed so far, to keep what is given out on frame boundaries.
    position: u64,
    /// Bytes since the last sync word, if one has been seen.
    since_sync: Option<usize>,
    pcm: bool,
}

impl Gate {
    pub fn new(channels: usize, rate: u32) -> Self {
        let frame = channels * 2;
        let quiet = (u64::from(rate) * QUIET_MS / 1000) as usize * frame;
        Self { frame, quiet, pending: Vec::new(), position: 0, since_sync: None, pcm: false }
    }

    /// Takes the next capture bytes and appends what is PCM to `out`, in
    /// whole frames.
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<u8>) {
        self.pending.extend_from_slice(bytes);
        self.position += bytes.len() as u64;
        let syncs = || (0..self.pending.len().saturating_sub(3)).step_by(2).filter(|&i| self.pending[i..i + 4] == SYNC);
        if let Some(last) = syncs().next_back() {
            if self.pcm {
                let first = syncs().next().expect("there is a last");
                self.give(first, out);
            }
            self.pcm = false;
            self.since_sync = Some(self.pending.len() - last);
            self.pending.clear();
            return;
        }
        if let Some(since) = &mut self.since_sync {
            *since += bytes.len();
        }
        if !self.pcm {
            if self.pending.len() < self.quiet {
                return;
            }
            self.pcm = true;
            if self.since_sync.is_some() {
                // The end of the last burst, its stuffing and the start of
                // the PCM: none of it can be told apart, so none is played.
                self.pending.clear();
                return;
            }
        }
        // A sync word may be split across feeds: its first bytes wait.
        self.give(self.pending.len().saturating_sub(SYNC.len() - 1), out);
    }

    /// Gives out the first `len` pending bytes as whole frames and drops the
    /// rest of them.
    fn give(&mut self, len: usize, out: &mut Vec<u8>) {
        let start = self.position - self.pending.len() as u64;
        let skip = (self.frame - (start % self.frame as u64) as usize) % self.frame;
        if len > skip {
            let whole = (len - skip) / self.frame * self.frame;
            out.extend_from_slice(&self.pending[skip..skip + whole]);
            self.pending.drain(..skip + whole);
        } else {
            self.pending.drain(..len.min(self.pending.len()));
        }
    }
}

/// Stereo PCM in any capture format and rate, as interleaved FLOAT32 at 48 kHz.
pub struct Stereo {
    format: Format,
    rate: u32,
    resampler: Option<resampling::Context>,
    /// Bytes short of a whole frame.
    carry: Vec<u8>,
    samples: Vec<f32>,
}

impl Stereo {
    pub fn new(format: Format, rate: u32) -> Result<Self, String> {
        let resampler = if rate == RATE {
            None
        } else {
            let float = Sample::F32(Type::Packed);
            let layout = ChannelLayout::STEREO;
            Some(resampling::Context::get(float, layout, rate, float, layout, RATE).map_err(|e| e.to_string())?)
        };
        Ok(Self { format, rate, resampler, carry: Vec::new(), samples: Vec::new() })
    }

    /// Appends the frames of `bytes` to `out`.
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<f32>) -> Result<(), String> {
        self.carry.extend_from_slice(bytes);
        let frame = 2 * self.format.bytes();
        let whole = self.carry.len() / frame * frame;
        self.samples.clear();
        let data = &self.carry[..whole];
        match self.format {
            Format::S16 => self.samples.extend(
                data.as_chunks::<2>().0.iter().map(|b| f32::from(i16::from_le_bytes(*b)) / 32_768.0),
            ),
            Format::S32 => self.samples.extend(
                data.as_chunks::<4>().0.iter().map(|b| i32::from_le_bytes(*b) as f32 / 2_147_483_648.0),
            ),
            Format::F32 => self.samples.extend(data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b))),
        }
        self.carry.drain(..whole);
        let frames = self.samples.len() / 2;
        let Some(resampler) = &mut self.resampler else {
            out.extend_from_slice(&self.samples);
            return Ok(());
        };
        if frames == 0 {
            return Ok(());
        }
        let float = Sample::F32(Type::Packed);
        let mut input = frame::Audio::new(float, frames, ChannelLayout::STEREO);
        input.set_rate(self.rate);
        input.plane_mut::<f32>(0)[..frames * 2].copy_from_slice(&self.samples);
        // Room for every frame the input makes, and what the filter held back.
        let room = frames * RATE as usize / self.rate as usize + 64;
        let mut output = frame::Audio::new(float, room, ChannelLayout::STEREO);
        resampler.run(&input, &mut output).map_err(|e| e.to_string())?;
        out.extend_from_slice(&output.plane::<f32>(0)[..output.samples() * 2]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A burst header and `payload` bytes, as a transmitter would send them.
    fn burst(payload: usize) -> Vec<u8> {
        let mut out = SYNC.to_vec();
        out.extend_from_slice(&[0x15, 0, payload as u8, (payload >> 8) as u8]);
        out.extend((0..payload).map(|i| (i % 251) as u8 | 1));
        out
    }

    /// Stereo 16-bit PCM of `frames` frames that say which they are (up to
    /// 32768) and never make a sync word.
    fn pcm(frames: usize) -> Vec<u8> {
        (0..frames).flat_map(|k| [k as u8, (k >> 8) as u8 & 0x7F, 1, 2]).collect()
    }

    /// Whether `part` is a run of whole frames of `whole`.
    fn whole_frames_of(part: &[u8], whole: &[u8]) -> bool {
        let Some(at) = whole.windows(4).position(|w| w == &part[..4]) else { return false };
        at.is_multiple_of(4) && part.len().is_multiple_of(4) && whole.get(at..at + part.len()) == Some(part)
    }

    fn run(gate: &mut Gate, input: &[u8], piece: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in input.chunks(piece) {
            gate.feed(chunk, &mut out);
        }
        out
    }

    const QUIET_FRAMES: usize = 48_000 * QUIET_MS as usize / 1000;

    #[test]
    fn pcm_from_the_start_plays_once_quiet_long_enough() {
        let input = pcm(10 * QUIET_FRAMES);
        let out = run(&mut Gate::new(2, 48_000), &input, 4096);
        // All but a frame or so held back for a split sync word.
        assert!(out.len() >= input.len() - 8 && input.starts_with(&out), "{} of {}", out.len(), input.len());
        // Nothing before the capture has been quiet long enough.
        assert!(run(&mut Gate::new(2, 48_000), &pcm(QUIET_FRAMES - 10), 4096).is_empty());
    }

    #[test]
    fn pcm_ends_exactly_where_the_bitstream_starts_at_any_split() {
        let tune = pcm(2 * QUIET_FRAMES);
        let input = [tune.clone(), burst(600), vec![0; 400], burst(600)].concat();
        for piece in [1, 2, 3, 7, 4096] {
            let out = run(&mut Gate::new(2, 48_000), &input, piece);
            assert_eq!(out, tune, "pieces of {piece}");
        }
    }

    #[test]
    fn the_end_of_a_bitstream_is_never_played() {
        let after = pcm(3 * QUIET_FRAMES);
        let input = [burst(6000), after.clone()].concat();
        for piece in [5, 4096] {
            let out = run(&mut Gate::new(2, 48_000), &input, piece);
            // Some PCM is lost with the end of the burst, none of the burst plays.
            assert!(!out.is_empty() && whole_frames_of(&out, &after), "pieces of {piece}");
        }
    }

    #[test]
    fn a_bitstream_with_short_gaps_never_becomes_pcm() {
        let gap = vec![0; (QUIET_FRAMES - 100) * 4];
        let input = [burst(600), gap.clone(), burst(600), gap.clone(), burst(600)].concat();
        assert!(run(&mut Gate::new(2, 48_000), &input, 1000).is_empty());
    }

    #[test]
    fn given_out_pcm_stays_on_frame_boundaries() {
        // A burst of odd length, padded to whole frames as in a real stream.
        let mut input = burst(601);
        input.resize(input.len().div_ceil(4) * 4, 0);
        let after = pcm(3 * QUIET_FRAMES);
        input.extend_from_slice(&after);
        for piece in [333, 1001, 4096] {
            let out = run(&mut Gate::new(2, 48_000), &input, piece);
            assert!(!out.is_empty() && whole_frames_of(&out, &after), "pieces of {piece}");
        }
    }

    #[test]
    fn converts_each_format_at_48_khz_as_it_is() {
        let s16: Vec<u8> = [16_384i16, -32_768].iter().flat_map(|s| s.to_le_bytes()).collect();
        let s32: Vec<u8> = [1_073_741_824i32, i32::MIN].iter().flat_map(|s| s.to_le_bytes()).collect();
        let f32: Vec<u8> = [0.5f32, -1.0].iter().flat_map(|s| s.to_le_bytes()).collect();
        for (format, bytes) in [(Format::S16, s16), (Format::S32, s32), (Format::F32, f32)] {
            let mut stereo = Stereo::new(format, 48_000).unwrap();
            let mut out = Vec::new();
            // Split mid-frame: the rest of the frame waits for the next push.
            stereo.push(&bytes[..3], &mut out).unwrap();
            stereo.push(&bytes[3..], &mut out).unwrap();
            assert_eq!(out, [0.5, -1.0], "{format:?}");
        }
    }

    #[test]
    fn resamples_44_1_khz_to_48_khz_keeping_the_pitch() {
        let mut stereo = Stereo::new(Format::F32, 44_100).unwrap();
        let tone = |i: usize| (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44_100.0).sin() as f32 * 0.5;
        let bytes: Vec<u8> = (0..44_100).flat_map(|i| [tone(i), tone(i)]).flat_map(f32::to_le_bytes).collect();
        let mut out = Vec::new();
        for chunk in bytes.chunks(16_384) {
            stereo.push(chunk, &mut out).unwrap();
        }
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        // One second in, about one second out (less what the filter holds).
        assert!((47_800..=48_000).contains(&left.len()), "{}", left.len());
        // 1 kHz at 48 kHz crosses zero upward 1000 times a second.
        let middle = &left[4_800..43_200];
        let crossings = middle.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((799..=801).contains(&crossings), "{crossings}");
        let peak = middle.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.01, "{peak}");
    }
}
