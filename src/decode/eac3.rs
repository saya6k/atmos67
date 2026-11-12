//! E-AC-3 bursts: Atmos through OpenJOC when the stream carries usable JOC
//! metadata, otherwise the channel bed through libavcodec.

use super::ffmpeg::{Ffmpeg, Id, eac3_access_units};
use super::joc::Joc;
use crate::layout::Layout;
use crate::render::Panner;

/// While playing the bed, try OpenJOC again this often (about once a second).
const RETRY_BURSTS: u32 = 32;

pub struct Eac3 {
    joc: Joc,
    core: Option<Ffmpeg>,
    use_core: bool,
    bursts_since_retry: u32,
    last_reason: Option<String>,
    scratch: Vec<f32>,
}

impl Eac3 {
    pub fn new(joc: Joc) -> Self {
        Self {
            joc,
            core: None,
            use_core: false,
            bursts_since_retry: 0,
            last_reason: None,
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
        self.bursts_since_retry += 1;
        if !self.use_core || self.bursts_since_retry >= RETRY_BURSTS {
            self.bursts_since_retry = 0;
            self.scratch.clear();
            match self.joc.decode(burst, &mut self.scratch) {
                Ok(()) => {
                    self.use_core = false;
                    out.extend_from_slice(&self.scratch);
                    return Ok(());
                }
                Err(reason) => {
                    if self.last_reason.as_ref() != Some(&reason) {
                        eprintln!(
                            "atmos67: no Atmos decoding for this E-AC-3 stream ({reason}); playing its channel bed"
                        );
                        self.last_reason = Some(reason);
                    }
                    if !self.use_core {
                        if let Some(core) = &mut self.core {
                            core.reset();
                        }
                        self.use_core = true;
                    }
                }
            }
        }
        if self.core.is_none() {
            self.core = Some(Ffmpeg::new(Id::EAC3)?);
        }
        let core = self.core.as_mut().expect("created above");
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
        self.use_core = false;
        self.bursts_since_retry = 0;
    }
}
