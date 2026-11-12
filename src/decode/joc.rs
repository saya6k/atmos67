//! E-AC-3 JOC (Dolby Digital Plus Atmos) through OpenJOC, which decodes and
//! renders to the speaker layout in one step.

use openjoc_api::{
    OpenJocConfig, OpenJocError, OpenJocOutputInfo, OpenJocPacket, OpenJocSession, OpenJocStatus,
};

pub struct Joc {
    session: OpenJocSession,
    /// Whether the session has taken audio since it was last flushed.
    active: bool,
}

impl Joc {
    pub fn new(config: OpenJocConfig) -> Result<Self, OpenJocError> {
        Ok(Self {
            session: OpenJocSession::new(config)?,
            active: false,
        })
    }

    pub fn output_info(&self) -> OpenJocOutputInfo {
        self.session.output_info()
    }

    /// Decodes one IEC 61937 burst payload, which holds exactly one access
    /// unit. A damaged or unsupported unit is dropped and reported as `Err`;
    /// the decoder is ready for the next one either way.
    pub fn decode(&mut self, access_unit: &[u8], out: &mut Vec<f32>) -> Result<(), String> {
        let packet = OpenJocPacket {
            data: access_unit,
            pts_samples: None,
            discontinuity: false,
            preroll: false,
        };
        loop {
            self.receive(out);
            match self.session.push_packet(packet) {
                Ok(OpenJocStatus::OutputPending) => {}
                Ok(_) => {
                    self.active = true;
                    self.receive(out);
                    return Ok(());
                }
                Err(error) => {
                    self.session.reset();
                    return Err(error.to_string());
                }
            }
        }
    }

    /// Plays out the access unit the decoder holds back. Called when bursts
    /// stop, so that audio is not heard late when the stream resumes.
    pub fn flush(&mut self, out: &mut Vec<f32>) {
        if self.active && self.session.drain().is_ok() {
            self.receive(out);
        }
        self.session.reset();
        self.active = false;
    }

    fn receive(&mut self, out: &mut Vec<f32>) {
        while let Some(frame) = self.session.receive_frame() {
            out.extend_from_slice(&frame.interleaved_f32);
        }
    }
}
