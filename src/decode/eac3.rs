//! E-AC-3 bursts: Atmos through OpenJOC when the stream carries usable JOC
//! metadata, otherwise the channel bed through libavcodec.

use super::ffmpeg::{Ffmpeg, Id, eac3_access_units};
use super::joc::Joc;
use crate::layout::Layout;
use crate::render::Panner;

/// After this many rejected bursts in a row the stream is taken to have no
/// usable Atmos metadata, and OpenJOC is tried only now and then.
const GIVE_UP_AFTER: u32 = 4;

/// How often OpenJOC is tried once given up on (about once a second).
const RETRY_BURSTS: u32 = 32;

pub struct Eac3 {
    joc: Joc,
    core: Option<Ffmpeg>,
    /// Bursts in a row that OpenJOC rejected or was not offered.
    rejected: u32,
    scratch: Vec<f32>,
}

impl Eac3 {
    pub fn new(joc: Joc) -> Self {
        Self {
            joc,
            core: None,
            rejected: 0,
            scratch: Vec::new(),
        }
    }

    pub fn decode(
        &mut self,
        burst: &[u8],
        layout: &Layout,
        panner: &Panner,
        out: &mut Vec<f32>,
    ) -> Result<(), String> {
        let try_joc = self.rejected < GIVE_UP_AFTER || self.rejected.is_multiple_of(RETRY_BURSTS);
        if try_joc {
            self.scratch.clear();
            match self.joc.decode(burst, &mut self.scratch) {
                Ok(()) => {
                    self.rejected = 0;
                    out.extend_from_slice(&self.scratch);
                    return Ok(());
                }
                Err(reason) => {
                    // What OpenJOC held back from the unit before still plays.
                    out.extend_from_slice(&self.scratch);
                    // Say so when the bed takes over, not at every retry.
                    if self.rejected == 0 {
                        eprintln!(
                            "atmos67: no Atmos decoding for this E-AC-3 stream ({reason}); playing its channel bed"
                        );
                    }
                }
            }
        }
        if self.core.is_none() {
            self.core = Some(Ffmpeg::new(Id::EAC3)?);
        }
        let core = self.core.as_mut().expect("created above");
        if self.rejected == 0 {
            // The bed decoder last saw some other part of the stream.
            core.reset();
        }
        self.rejected += 1;
        for unit in eac3_access_units(burst) {
            core.decode(unit, layout, panner, out)?;
        }
        Ok(())
    }

    /// Plays out held audio and forgets the stream; called when it stops.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        self.joc.flush(out);
        if let Some(core) = &mut self.core {
            core.reset();
        }
        self.rejected = 0;
    }
}
