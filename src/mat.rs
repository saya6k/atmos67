//! MAT (Metadata-enhanced Audio Transmission) frames, the HDMI carrier of
//! Dolby TrueHD.
//!
//! Each IEC 61937 burst holds one fixed-size MAT frame. Marker codes sit at
//! its start, middle and end; between them runs the TrueHD stream, its access
//! units spaced out with zero padding to keep their timing. An access unit
//! may continue across a marker or into the next frame.

const FRAME_BYTES: usize = 61_424;
const START_CODE: [u8; 20] = [
    0x07, 0x9E, 0x00, 0x03, 0x84, 0x01, 0x01, 0x01, 0x80, 0x00, 0x56, 0xA5, 0x3B, 0xF4, 0x81, 0x83,
    0x49, 0x80, 0x77, 0xE0,
];
const MIDDLE_CODE: std::ops::Range<usize> = 30_708..30_720;
const END_CODE: std::ops::Range<usize> = 61_408..61_424;

const MAJOR_SYNC: [u8; 4] = [0xF8, 0x72, 0x6F, 0xBA];

#[derive(Default)]
pub struct Mat {
    /// Whether the position in the stream is known to be between or inside
    /// access units. Until then the stream is searched for a major sync.
    synced: bool,
    /// Bytes still to copy of the access unit being passed through.
    remaining: usize,
    /// Stream bytes not yet interpreted: the search window while unsynced,
    /// or the first byte of a two-byte header split across markers.
    held: Vec<u8>,
}

impl Mat {
    /// Appends the TrueHD access units in one MAT frame to `out`, without the
    /// padding between them.
    pub fn push(&mut self, frame: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
        if frame.len() != FRAME_BYTES || !frame.starts_with(&START_CODE) {
            return Err("MAT frame does not carry TrueHD (MAT PCM is not supported)".to_owned());
        }
        self.stream(&frame[START_CODE.len()..MIDDLE_CODE.start], out);
        self.stream(&frame[MIDDLE_CODE.end..END_CODE.start], out);
        Ok(())
    }

    fn stream(&mut self, mut bytes: &[u8], out: &mut Vec<u8>) {
        if !self.synced {
            // A major sync starts four bytes into its access unit.
            self.held.extend_from_slice(bytes);
            let Some(sync) = self.held.windows(4).skip(4).position(|w| w == MAJOR_SYNC) else {
                let keep = self.held.len().min(MAJOR_SYNC.len() + 3);
                self.held.drain(..self.held.len() - keep);
                return;
            };
            let rest = self.held.split_off(sync);
            self.held.clear();
            self.synced = true;
            return self.stream(&rest, out);
        }
        loop {
            let take = self.remaining.min(bytes.len());
            out.extend_from_slice(&bytes[..take]);
            self.remaining -= take;
            bytes = &bytes[take..];
            // Between access units: padding comes in zero pairs, and a unit
            // opens with its length in 16-bit words in the low 12 bits.
            let Some(header) = self.take_pair(&mut bytes) else {
                return;
            };
            if header != [0, 0] {
                out.extend_from_slice(&header);
                let words = usize::from(u16::from_be_bytes(header) & 0x0FFF);
                self.remaining = (words * 2).saturating_sub(2);
            }
        }
    }

    /// The next two stream bytes, which may arrive one at a time.
    fn take_pair(&mut self, bytes: &mut &[u8]) -> Option<[u8; 2]> {
        while self.held.len() < 2 {
            let (first, rest) = bytes.split_first()?;
            self.held.push(*first);
            *bytes = rest;
        }
        let pair = [self.held[0], self.held[1]];
        self.held.clear();
        Some(pair)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iec61937::{DATA_TYPE_MAT, Depacketizer};

    /// The TrueHD stream FFmpeg packed into the MAT fixture:
    /// `ffmpeg -i tones-truehd-5.1.thd -c copy -f spdif tones-truehd-5.1.spdif`
    const STREAM: &[u8] = include_bytes!("../tests/fixtures/tones-truehd-5.1.thd");
    const CAPTURE: &[u8] = include_bytes!("../tests/fixtures/tones-truehd-5.1.spdif");

    fn frames() -> Vec<Vec<u8>> {
        let mut depacketizer = Depacketizer::default();
        depacketizer.feed(CAPTURE);
        let mut frames = Vec::new();
        while let Some((data_type, payload)) = depacketizer.next_burst() {
            assert_eq!(data_type, DATA_TYPE_MAT);
            frames.push(payload.to_vec());
        }
        frames
    }

    #[test]
    fn unpacks_the_stream_ffmpeg_packed() {
        let mut mat = Mat::default();
        let mut out = Vec::new();
        for frame in frames() {
            mat.push(&frame, &mut out).unwrap();
        }
        // The muxer holds back the tail of the stream at the end of input.
        assert!(out.len() > STREAM.len() * 3 / 4, "{} of {}", out.len(), STREAM.len());
        assert!(STREAM.starts_with(&out));
    }

    #[test]
    fn joining_mid_stream_starts_at_the_next_major_sync() {
        let mut mat = Mat::default();
        let mut out = Vec::new();
        for frame in &frames()[2..] {
            mat.push(frame, &mut out).unwrap();
        }
        assert!(out.len() > 1000);
        assert_eq!(out[4..8], MAJOR_SYNC);
        let at = STREAM.windows(out.len()).position(|w| w == out).expect("part of the stream");
        assert!(at > 0);
    }

    #[test]
    fn other_mat_payloads_are_refused() {
        let mut frame = frames().remove(0);
        frame[3] ^= 0xFF;
        assert!(Mat::default().push(&frame, &mut Vec::new()).is_err());
        assert!(Mat::default().push(&[0u8; 100], &mut Vec::new()).is_err());
    }
}
