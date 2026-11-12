//! IEC 61937 burst extraction from 16-bit little-endian PCM capture.
//!
//! HDMI carries compressed audio as data bursts inside what looks like PCM:
//! a four-word preamble (Pa, Pb, Pc, Pd) followed by the payload, each 16-bit
//! word holding two bitstream bytes most-significant first.

const SYNC: [u8; 4] = [0x72, 0xF8, 0x1F, 0x4E]; // Pa = 0xF872, Pb = 0x4E1F as LE words
const HEADER_BYTES: usize = 8;

pub const DATA_TYPE_AC3: u8 = 0x01;
pub const DATA_TYPE_EAC3: u8 = 0x15;
pub const DATA_TYPE_DTS_HD: u8 = 0x11;
pub const DATA_TYPE_MAT: u8 = 0x16;

#[derive(Default)]
pub struct Depacketizer {
    buf: Vec<u8>,
    pos: usize,
    payload: Vec<u8>,
    last_type: u8,
}

impl Depacketizer {
    /// Appends captured PCM bytes. Must start on a sample boundary.
    pub fn feed(&mut self, pcm: &[u8]) {
        self.buf.drain(..self.pos);
        self.pos = 0;
        self.buf.extend_from_slice(pcm);
    }

    /// The burst most recently returned by `next_burst`.
    fn last_burst(&self) -> Option<(u8, &[u8])> {
        Some((self.last_type, &self.payload))
    }

    /// Returns the next complete burst as (data type, payload in bitstream
    /// byte order), or `None` when more input is needed.
    pub fn next_burst(&mut self) -> Option<(u8, &[u8])> {
        loop {
            let rest = &self.buf[self.pos..];
            if rest.len() < HEADER_BYTES {
                return None;
            }
            if rest[..4] != SYNC {
                self.pos += 2;
                continue;
            }
            let data_type = rest[4] & 0x7F;
            let pd = usize::from(u16::from_le_bytes([rest[6], rest[7]]));
            // Pd counts bytes for E-AC-3 and MAT, bits for the older types.
            let len = if matches!(data_type, DATA_TYPE_EAC3 | DATA_TYPE_MAT) {
                pd
            } else {
                pd.div_ceil(8)
            };
            let padded = len + (len & 1);
            if rest.len() < HEADER_BYTES + padded {
                return None;
            }
            self.payload.clear();
            for word in rest[HEADER_BYTES..HEADER_BYTES + padded].chunks_exact(2) {
                self.payload.extend_from_slice(&[word[1], word[0]]);
            }
            self.payload.truncate(len);
            self.pos += HEADER_BYTES + padded;
            self.last_type = data_type;
            return Some((data_type, &self.payload));
        }
    }
}

/// Finds bursts in a stereo or 8-channel capture.
///
/// HDMI carries most formats in the first two channels. The high-bitrate
/// ones (TrueHD in MAT, DTS-HD) use all eight as one continuous stream, so an
/// 8-channel capture is searched both ways.
pub struct Capture {
    channels: usize,
    pair: Depacketizer,
    all: Depacketizer,
    /// Bytes of an incomplete frame, kept until the rest arrives.
    carry: Vec<u8>,
    scratch: Vec<u8>,
    /// Input bytes since the last high-bitrate burst.
    since_high_bitrate: u64,
    /// While a high-bitrate stream is running, anything found in the first
    /// two channels alone is a coincidence. This many bytes must pass first.
    holdoff: u64,
}

impl Capture {
    pub fn new(channels: usize, rate: u32) -> Self {
        let bytes_per_second = u64::from(rate) * channels as u64 * 2;
        Self {
            channels,
            pair: Depacketizer::default(),
            all: Depacketizer::default(),
            carry: Vec::new(),
            scratch: Vec::new(),
            since_high_bitrate: u64::MAX,
            holdoff: bytes_per_second / 4,
        }
    }

    pub fn feed(&mut self, pcm: &[u8]) {
        if self.channels == 2 {
            self.pair.feed(pcm);
            return;
        }
        self.since_high_bitrate = self.since_high_bitrate.saturating_add(pcm.len() as u64);
        self.all.feed(pcm);
        self.carry.extend_from_slice(pcm);
        let frame = self.channels * 2;
        let whole = self.carry.len() / frame * frame;
        self.scratch.clear();
        for frame in self.carry[..whole].chunks_exact(frame) {
            self.scratch.extend_from_slice(&frame[..4]);
        }
        self.carry.drain(..whole);
        self.pair.feed(&self.scratch);
    }

    pub fn next_burst(&mut self) -> Option<(u8, &[u8])> {
        if self.channels > 2 {
            while let Some((data_type, _)) = self.all.next_burst() {
                if matches!(data_type, DATA_TYPE_MAT | DATA_TYPE_DTS_HD) {
                    self.since_high_bitrate = 0;
                    return self.all.last_burst();
                }
            }
        }
        while self.pair.next_burst().is_some() {
            if self.since_high_bitrate >= self.holdoff {
                return self.pair.last_burst();
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a burst the way a transmitter would: LE words, payload byte-swapped.
    fn burst(data_type: u8, pd: u16, payload: &[u8]) -> Vec<u8> {
        let mut out = SYNC.to_vec();
        out.extend_from_slice(&[data_type, 0]);
        out.extend_from_slice(&pd.to_le_bytes());
        for pair in payload.chunks(2) {
            out.extend_from_slice(&[*pair.get(1).unwrap_or(&0), pair[0]]);
        }
        out
    }

    fn collect(d: &mut Depacketizer) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        while let Some((ty, payload)) = d.next_burst() {
            out.push((ty, payload.to_vec()));
        }
        out
    }

    #[test]
    fn extracts_odd_length_burst_after_silence() {
        let mut pcm = vec![0u8; 6];
        pcm.extend(burst(DATA_TYPE_EAC3, 5, &[1, 2, 3, 4, 5]));
        pcm.extend([0u8; 10]);
        let mut d = Depacketizer::default();
        d.feed(&pcm);
        assert_eq!(collect(&mut d), [(DATA_TYPE_EAC3, vec![1, 2, 3, 4, 5])]);
    }

    #[test]
    fn length_is_in_bits_for_ac3() {
        let mut d = Depacketizer::default();
        d.feed(&burst(0x01, 32, &[9, 8, 7, 6]));
        assert_eq!(collect(&mut d), [(0x01, vec![9, 8, 7, 6])]);
    }

    #[test]
    fn burst_split_across_feeds_at_every_offset() {
        let pcm = [
            burst(DATA_TYPE_EAC3, 4, &[1, 2, 3, 4]),
            vec![0; 4],
            burst(DATA_TYPE_EAC3, 2, &[5, 6]),
        ]
        .concat();
        for split in 0..pcm.len() {
            let mut d = Depacketizer::default();
            d.feed(&pcm[..split]);
            let mut got = collect(&mut d);
            d.feed(&pcm[split..]);
            got.extend(collect(&mut d));
            let want = [
                (DATA_TYPE_EAC3, vec![1, 2, 3, 4]),
                (DATA_TYPE_EAC3, vec![5, 6]),
            ];
            assert_eq!(got, want, "split at {split}");
        }
    }

    #[test]
    fn plain_pcm_yields_nothing() {
        let mut d = Depacketizer::default();
        d.feed(&[0x11; 4096]);
        assert!(collect(&mut d).is_empty());
    }
    fn widen(stereo: &[u8]) -> Vec<u8> {
        stereo
            .chunks(4)
            .flat_map(|frame| [frame, &[0u8; 12]].concat())
            .collect()
    }

    fn collect_capture(c: &mut Capture) -> Vec<(u8, Vec<u8>)> {
        let mut out = Vec::new();
        while let Some((ty, payload)) = c.next_burst() {
            out.push((ty, payload.to_vec()));
        }
        out
    }

    #[test]
    fn eight_channel_capture_finds_stereo_bursts_in_the_first_pair() {
        let stereo = [vec![0; 8], burst(DATA_TYPE_EAC3, 6, &[1, 2, 3, 4, 5, 6]), vec![0; 4]].concat();
        let wide = widen(&stereo);
        // Feed in pieces that do not line up with frames.
        let mut capture = Capture::new(8, 192_000);
        let mut got = Vec::new();
        for piece in wide.chunks(7) {
            capture.feed(piece);
            got.extend(collect_capture(&mut capture));
        }
        assert_eq!(got, [(DATA_TYPE_EAC3, vec![1, 2, 3, 4, 5, 6])]);
    }

    #[test]
    fn eight_channel_capture_finds_high_bitrate_bursts_across_all_channels() {
        let payload: Vec<u8> = (0..40).collect();
        let mut capture = Capture::new(8, 192_000);
        capture.feed(&[vec![0; 16], burst(DATA_TYPE_MAT, 40, &payload), vec![0; 16]].concat());
        assert_eq!(collect_capture(&mut capture), [(DATA_TYPE_MAT, payload)]);
    }

    #[test]
    fn stereo_bursts_are_ignored_while_a_high_bitrate_stream_runs() {
        // The first frame of this MAT payload reads as a burst preamble when
        // only channels 1-2 of each frame are looked at.
        let mut payload = vec![0u8; 64];
        payload[8..12].copy_from_slice(&[0xF8, 0x72, 0x4E, 0x1F]);
        let mut capture = Capture::new(8, 192_000);
        capture.feed(&[burst(DATA_TYPE_MAT, 64, &payload), vec![0; 64]].concat());
        let got = collect_capture(&mut capture);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, DATA_TYPE_MAT);
    }
}
