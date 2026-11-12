//! IEC 61937 burst extraction from 16-bit little-endian PCM capture.
//!
//! HDMI carries compressed audio as data bursts inside what looks like PCM:
//! a four-word preamble (Pa, Pb, Pc, Pd) followed by the payload, each 16-bit
//! word holding two bitstream bytes most-significant first.

const SYNC: [u8; 4] = [0x72, 0xF8, 0x1F, 0x4E]; // Pa = 0xF872, Pb = 0x4E1F as LE words
const HEADER_BYTES: usize = 8;

pub const DATA_TYPE_EAC3: u8 = 0x15;
pub const DATA_TYPE_MAT: u8 = 0x16;

#[derive(Default)]
pub struct Depacketizer {
    buf: Vec<u8>,
    pos: usize,
    payload: Vec<u8>,
}

impl Depacketizer {
    /// Appends captured PCM bytes. Must start on a sample boundary.
    pub fn feed(&mut self, pcm: &[u8]) {
        self.buf.drain(..self.pos);
        self.pos = 0;
        self.buf.extend_from_slice(pcm);
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
            return Some((data_type, &self.payload));
        }
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
}
