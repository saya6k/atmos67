//! A SECS 2R CamillaDSP export folder as the sender's correction.
//!
//! The folder holds `secs-design.json`, whose `inputs` and `outputs` name
//! the pipeline's channels by SECS speaker role, and one configuration
//! under `configs/`. Inputs are matched to the rendered speakers by name;
//! a speaker the design does not cover, or a design input with no speaker,
//! is refused rather than played silently. Outputs follow the design: its
//! main speakers, then its subwoofers.

use std::error::Error;
use std::fs;
use std::path::Path;

use super::camilla::{Config, Pipeline};
use crate::layout::canonical;

/// Frames processed at a time, which is also the latency the correction adds.
pub const BLOCK: usize = 1024;

pub struct Design {
    pipeline: Pipeline,
    /// The rendered channel feeding each pipeline input.
    routing: Vec<usize>,
    channels_in: usize,
    pub outputs: Vec<String>,
    /// Frames waiting for a whole block, one buffer per pipeline input.
    pending: Vec<Vec<f64>>,
}

/// The rendered speaker name of a SECS 2R input role.
fn speaker(role: &str) -> &str {
    match role {
        "L" => "FL",
        "R" => "FR",
        "C" => "FC",
        "Tfl" => "TFL",
        "Tfr" => "TFR",
        "Tbl" => "TBL",
        "Tbr" => "TBR",
        "Tml" => "Ltm",
        "Tmr" => "Rtm",
        other => other,
    }
}

impl Design {
    /// Loads the design in `folder` for rendered channels named `labels`.
    pub fn load(folder: &Path, labels: &[String]) -> Result<Design, Box<dyn Error>> {
        let manifest: serde_json::Value = serde_json::from_slice(&fs::read(folder.join("secs-design.json"))?)?;
        let names = |key: &str| -> Result<Vec<String>, Box<dyn Error>> {
            manifest[key]
                .as_array()
                .ok_or_else(|| format!("secs-design.json has no `{key}`"))?
                .iter()
                .map(|v| Ok(v.as_str().ok_or("role name")?.to_string()))
                .collect()
        };
        let (inputs, outputs) = (names("inputs")?, names("outputs")?);

        let configs: Vec<_> = fs::read_dir(folder.join("configs"))?
            .map(|entry| entry.map(|e| e.path()))
            .filter(|path| path.as_ref().map_or(true, |p| p.extension().is_some_and(|e| e == "yml")))
            .collect::<Result<_, _>>()?;
        let [config] = configs.as_slice() else {
            return Err(format!("{} must hold exactly one .yml", folder.join("configs").display()).into());
        };
        let config = Config::load(config)?;
        let pipeline = Pipeline::new(&config, BLOCK)?;
        if pipeline.inputs() != inputs.len() || pipeline.outputs() != outputs.len() {
            return Err("the configuration and secs-design.json disagree on the channel count".into());
        }

        let routing = inputs
            .iter()
            .map(|role| {
                labels
                    .iter()
                    .position(|label| canonical(label) == speaker(role))
                    .ok_or_else(|| format!("design input {role} has no speaker in the layout"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some((_, label)) = labels.iter().enumerate().find(|(index, _)| !routing.contains(index)) {
            return Err(format!("speaker {label} is not in the design").into());
        }
        Ok(Design { pipeline, channels_in: labels.len(), outputs, pending: vec![Vec::with_capacity(BLOCK); inputs.len()], routing })
    }

    /// Takes interleaved rendered frames and appends the corrected frames of
    /// every block they complete to `out`, interleaved in output order.
    pub fn push(&mut self, frames: &[f32], out: &mut Vec<f32>) {
        for frame in frames.chunks_exact(self.channels_in) {
            for (buffer, &channel) in self.pending.iter_mut().zip(&self.routing) {
                buffer.push(f64::from(frame[channel]));
            }
            if self.pending[0].len() == BLOCK {
                let mut channels: Vec<Vec<f64>> =
                    self.pending.iter_mut().map(|b| std::mem::replace(b, Vec::with_capacity(BLOCK))).collect();
                self.pipeline.process(&mut channels);
                for n in 0..BLOCK {
                    out.extend(channels.iter().map(|channel| channel[n] as f32));
                }
            }
        }
    }
}
