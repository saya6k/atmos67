//! The CamillaDSP configurations SECS 2R exports, read and run.
//!
//! SECS writes every value on one line as JSON after its key (`  name: {…}`,
//! `  - {…}`), under the top-level sections `devices`, `filters`, `mixers`
//! and `pipeline`, so a line reader and a JSON parser cover the format
//! without a YAML parser. Only the filters SECS uses are accepted; anything
//! else is refused by name rather than run differently from CamillaDSP.

use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::Path;

use serde_json::Value;

use super::biquad::{Biquad, Coefficients, Family, Pass, combo};
use super::conv::Convolver;
use super::Delay;

const RATE: f64 = 48_000.0;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

enum Filter {
    Biquads(Vec<Coefficients>),
    Conv(Vec<f64>),
    Delay(usize),
    Gain(f64),
}

struct Mixer {
    outputs: usize,
    /// For each output channel, its sources as (input channel, linear gain).
    mapping: Vec<Vec<(usize, f64)>>,
}

enum Step {
    Mixer(String),
    Filter { channels: Vec<usize>, names: Vec<String> },
}

pub struct Config {
    capture_channels: usize,
    filters: HashMap<String, Filter>,
    mixers: HashMap<String, Mixer>,
    pipeline: Vec<Step>,
}

impl Config {
    /// Reads a configuration and the coefficient files it names, relative to
    /// its own folder.
    pub fn load(path: &Path) -> Result<Config> {
        let text = fs::read_to_string(path)?;
        let folder = path.parent().unwrap_or(Path::new("."));
        parse(&text, folder).map_err(|e| format!("{}: {e}", path.display()).into())
    }

    /// Channels the pipeline takes in.
    pub fn inputs(&self) -> usize {
        self.capture_channels
    }
}

fn parse(text: &str, folder: &Path) -> Result<Config> {
    let mut config = Config {
        capture_channels: 0,
        filters: HashMap::new(),
        mixers: HashMap::new(),
        pipeline: Vec::new(),
    };
    let mut section = "";
    for (number, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let at = |e: Box<dyn Error>| -> Box<dyn Error> { format!("line {}: {e}", number + 1).into() };
        if !line.starts_with(' ') {
            section = line.split_once(':').ok_or_else(|| at("expected `key:`".into()))?.0;
            continue;
        }
        if section == "pipeline" {
            let item = trimmed.strip_prefix("- ").ok_or_else(|| at("expected `- {…}`".into()))?;
            config.pipeline.push(step(&json(item).map_err(at)?).map_err(at)?);
            continue;
        }
        let (key, item) = trimmed.split_once(": ").ok_or_else(|| at("expected `name: value`".into()))?;
        match section {
            "devices" => match key {
                "samplerate" if json(item).map_err(at)?.as_f64() != Some(RATE) => {
                    return Err(at(format!("sample rate {item}; only 48000 is supported").into()));
                }
                "capture" => config.capture_channels = count(&json(item).map_err(at)?["channels"]).map_err(at)?,
                _ => {}
            },
            "filters" => {
                config.filters.insert(key.into(), filter(&json(item).map_err(at)?, folder).map_err(at)?);
            }
            "mixers" => {
                config.mixers.insert(key.into(), mixer(&json(item).map_err(at)?).map_err(at)?);
            }
            _ => {}
        }
    }
    if config.capture_channels == 0 {
        return Err("no capture channel count".into());
    }
    Ok(config)
}

fn json(text: &str) -> Result<Value> {
    Ok(serde_json::from_str(text).map_err(|e| format!("not a SECS 2R export ({e})"))?)
}

fn count(value: &Value) -> Result<usize> {
    Ok(value.as_u64().ok_or("expected a count")? as usize)
}

fn number(value: &Value, name: &str) -> Result<f64> {
    value[name].as_f64().ok_or_else(|| format!("missing `{name}`").into())
}

fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name].as_str().ok_or_else(|| format!("missing `{name}`").into())
}

/// A gain in dB (or linear with `scale: linear`), negative when inverted.
fn gain(value: &Value) -> Result<f64> {
    if value["mute"].as_bool() == Some(true) {
        return Ok(0.0);
    }
    let gain = number(value, "gain")?;
    let linear = match value["scale"].as_str() {
        None | Some("dB") => 10f64.powf(gain / 20.0),
        Some("linear") => gain,
        Some(other) => return Err(format!("gain scale {other}").into()),
    };
    Ok(if value["inverted"].as_bool() == Some(true) { -linear } else { linear })
}

fn filter(spec: &Value, folder: &Path) -> Result<Filter> {
    let kind = text(spec, "type")?;
    let p = &spec["parameters"];
    let subtype = || text(p, "type");
    Ok(match kind {
        "Biquad" => {
            let freq = number(p, "freq")?;
            let q = || number(p, "q");
            Filter::Biquads(vec![match subtype()? {
                "Peaking" => Coefficients::peaking(RATE, freq, q()?, number(p, "gain")?),
                "Allpass" => Coefficients::allpass(RATE, freq, q()?),
                "Highpass" => Coefficients::pass(Pass::High, RATE, freq, q()?),
                "Lowpass" => Coefficients::pass(Pass::Low, RATE, freq, q()?),
                "HighpassFO" => Coefficients::pass_first_order(Pass::High, RATE, freq),
                "LowpassFO" => Coefficients::pass_first_order(Pass::Low, RATE, freq),
                other => return Err(format!("unsupported Biquad type {other}").into()),
            }])
        }
        "BiquadCombo" => {
            let (family, pass) = match subtype()? {
                "ButterworthHighpass" => (Family::Butterworth, Pass::High),
                "ButterworthLowpass" => (Family::Butterworth, Pass::Low),
                "LinkwitzRileyHighpass" => (Family::LinkwitzRiley, Pass::High),
                "LinkwitzRileyLowpass" => (Family::LinkwitzRiley, Pass::Low),
                other => return Err(format!("unsupported BiquadCombo type {other}").into()),
            };
            let order = count(&p["order"])?;
            Filter::Biquads(
                combo(family, pass, RATE, number(p, "freq")?, order).ok_or_else(|| format!("BiquadCombo order {order}"))?,
            )
        }
        "Conv" => {
            if subtype()? != "Wav" || p["channel"].as_u64().unwrap_or(0) != 0 {
                return Err("only mono Wav convolution is supported".into());
            }
            Filter::Conv(wav_samples(&fs::read(folder.join(text(p, "filename")?))?)?)
        }
        "Delay" => {
            if p["subsample"].as_bool() == Some(true) {
                return Err("subsample delay is not supported".into());
            }
            let delay = number(p, "delay")?;
            let samples = match p["unit"].as_str() {
                Some("samples") => delay,
                None | Some("ms") => delay / 1000.0 * RATE,
                Some(other) => return Err(format!("delay unit {other}").into()),
            };
            Filter::Delay(samples.round().max(0.0) as usize)
        }
        "Gain" => Filter::Gain(gain(p)?),
        other => return Err(format!("unsupported filter type {other}").into()),
    })
}

fn mixer(spec: &Value) -> Result<Mixer> {
    let outputs = count(&spec["channels"]["out"])?;
    let mut mapping = vec![Vec::new(); outputs];
    for entry in spec["mapping"].as_array().ok_or("missing `mapping`")? {
        if entry["mute"].as_bool() == Some(true) {
            continue;
        }
        let dest = count(&entry["dest"])?;
        let slot = mapping.get_mut(dest).ok_or_else(|| format!("mixer dest {dest} out of range"))?;
        for source in entry["sources"].as_array().ok_or("missing `sources`")? {
            slot.push((count(&source["channel"])?, gain(source)?));
        }
    }
    Ok(Mixer { outputs, mapping })
}

fn step(spec: &Value) -> Result<Step> {
    match text(spec, "type")? {
        "Mixer" => Ok(Step::Mixer(text(spec, "name")?.into())),
        "Filter" => Ok(Step::Filter {
            channels: if spec["bypassed"].as_bool() == Some(true) {
                Vec::new()
            } else {
                spec["channels"].as_array().ok_or("missing `channels`")?.iter().map(count).collect::<Result<_>>()?
            },
            names: spec["names"]
                .as_array()
                .ok_or("missing `names`")?
                .iter()
                .map(|name| Ok(name.as_str().ok_or("filter name")?.to_string()))
                .collect::<Result<_>>()?,
        }),
        other => Err(format!("unsupported pipeline step {other}").into()),
    }
}

/// Samples of a mono IEEE-float WAV file.
pub fn wav_samples(bytes: &[u8]) -> Result<Vec<f64>> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let mut rest = &bytes[12..];
    let mut float_mono = false;
    while rest.len() >= 8 {
        let size = u32::from_le_bytes(rest[4..8].try_into()?) as usize;
        let body = rest.get(8..8 + size).ok_or("truncated WAV chunk")?;
        match &rest[..4] {
            b"fmt " if body.len() >= 16 => {
                let format = u16::from_le_bytes([body[0], body[1]]);
                let channels = u16::from_le_bytes([body[2], body[3]]);
                let bits = u16::from_le_bytes([body[14], body[15]]);
                float_mono = format == 3 && channels == 1 && bits == 32;
            }
            b"data" => {
                if !float_mono {
                    return Err("only mono 32-bit float WAV is supported".into());
                }
                return Ok(body.as_chunks::<4>().0.iter().map(|b| f64::from(f32::from_le_bytes(*b))).collect());
            }
            _ => {}
        }
        rest = &rest[(8 + size + size % 2).min(rest.len())..];
    }
    Err("WAV file without data".into())
}

enum Processor {
    Biquad(Biquad),
    Conv(Box<Convolver>),
    Delay(Delay),
    Gain(f64),
}

impl Processor {
    fn process(&mut self, block: &mut [f64]) {
        match self {
            Processor::Biquad(biquad) => biquad.process(block),
            Processor::Conv(convolver) => convolver.process(block),
            Processor::Delay(delay) => delay.process(block),
            Processor::Gain(gain) => block.iter_mut().for_each(|s| *s *= *gain),
        }
    }
}

enum Stage {
    Mixer { outputs: usize, mapping: Vec<Vec<(usize, f64)>> },
    Filter { channel: usize, chain: Vec<Processor> },
}

/// A configuration ready to run on blocks of a fixed length. Every pipeline
/// step gets its own filter state, as in CamillaDSP.
pub struct Pipeline {
    block: usize,
    inputs: usize,
    outputs: usize,
    stages: Vec<Stage>,
    scratch: Vec<Vec<f64>>,
}

impl Pipeline {
    pub fn new(config: &Config, block: usize) -> Result<Pipeline> {
        let mut channels = config.capture_channels;
        let mut stages = Vec::new();
        for step in &config.pipeline {
            match step {
                Step::Mixer(name) => {
                    let mixer = config.mixers.get(name).ok_or_else(|| format!("unknown mixer {name}"))?;
                    if let Some(source) = mixer.mapping.iter().flatten().find(|(input, _)| *input >= channels) {
                        return Err(format!("mixer {name} reads channel {} of {channels}", source.0).into());
                    }
                    channels = mixer.outputs;
                    stages.push(Stage::Mixer { outputs: mixer.outputs, mapping: mixer.mapping.clone() });
                }
                Step::Filter { channels: targets, names } => {
                    for &channel in targets {
                        if channel >= channels {
                            return Err(format!("filter step on channel {channel} of {channels}").into());
                        }
                        let mut chain = Vec::new();
                        for name in names {
                            match config.filters.get(name).ok_or_else(|| format!("unknown filter {name}"))? {
                                Filter::Biquads(sections) => {
                                    chain.extend(sections.iter().map(|c| Processor::Biquad(Biquad::new(*c))));
                                }
                                Filter::Conv(taps) => chain.push(Processor::Conv(Box::new(Convolver::new(taps, block)))),
                                Filter::Delay(samples) => chain.push(Processor::Delay(Delay::new(*samples))),
                                Filter::Gain(gain) => chain.push(Processor::Gain(*gain)),
                            }
                        }
                        stages.push(Stage::Filter { channel, chain });
                    }
                }
            }
        }
        Ok(Pipeline { block, inputs: config.capture_channels, outputs: channels, stages, scratch: Vec::new() })
    }

    pub fn inputs(&self) -> usize {
        self.inputs
    }

    pub fn outputs(&self) -> usize {
        self.outputs
    }

    /// Runs one block: `channels` holds `inputs()` buffers of the block
    /// length on entry and `outputs()` buffers on return.
    pub fn process(&mut self, channels: &mut Vec<Vec<f64>>) {
        assert_eq!(channels.len(), self.inputs, "pipeline input channels");
        for stage in &mut self.stages {
            match stage {
                Stage::Mixer { outputs, mapping } => {
                    self.scratch.resize_with(*outputs, Vec::new);
                    for (out, sources) in self.scratch.iter_mut().zip(mapping.iter()) {
                        out.clear();
                        out.resize(self.block, 0.0);
                        for &(input, gain) in sources {
                            for (o, i) in out.iter_mut().zip(&channels[input]) {
                                *o += gain * i;
                            }
                        }
                    }
                    std::mem::swap(channels, &mut self.scratch);
                }
                Stage::Filter { channel, chain } => {
                    for processor in chain {
                        processor.process(&mut channels[*channel]);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav_bytes(taps: &[f32]) -> Vec<u8> {
        let data = (taps.len() * 4) as u32;
        let mut out = b"RIFF".to_vec();
        out.extend((36 + data).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(3u16.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(48_000u32.to_le_bytes());
        out.extend((48_000u32 * 4).to_le_bytes());
        out.extend(4u16.to_le_bytes());
        out.extend(32u16.to_le_bytes());
        out.extend(b"data");
        out.extend(data.to_le_bytes());
        taps.iter().for_each(|t| out.extend(t.to_le_bytes()));
        out
    }

    /// Two inputs: a delayed, convolved copy of input 0 on output 0, and the
    /// inverted sum of both, 6 dB down, on output 1.
    const CONFIG: &str = r#"# made by hand in the shape SECS 2R writes
title: "test"
devices:
  samplerate: 48000
  chunksize: 1024
  capture: {"type": "RawFile", "channels": 2, "filename": "in.raw", "format": "FLOAT64LE"}
  playback: {"type": "File", "channels": 2, "filename": "out.raw", "format": "FLOAT64LE"}
filters:
  fir: {"type": "Conv", "parameters": {"type": "Wav", "filename": "coeffs/fir.wav"}}
  late: {"type": "Delay", "parameters": {"delay": 3, "unit": "samples", "subsample": false}}
  down: {"type": "Gain", "parameters": {"gain": -6.0206, "inverted": true}}
mixers:
  route: {"channels": {"in": 2, "out": 2}, "mapping": [{"dest": 0, "sources": [{"channel": 0, "gain": 0, "inverted": false}]}, {"dest": 1, "sources": [{"channel": 0, "gain": 0, "inverted": false}, {"channel": 1, "gain": 0, "inverted": false}]}]}
pipeline:
  - {"type": "Mixer", "name": "route"}
  - {"type": "Filter", "channels": [0], "names": ["late", "fir"]}
  - {"type": "Filter", "channels": [1], "names": ["down"], "description": "x", "bypassed": false}
"#;

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("atmos67-{name}-{}", std::process::id()));
        fs::create_dir_all(dir.join("coeffs")).unwrap();
        dir
    }

    #[test]
    fn runs_mixers_filters_and_coefficient_files() {
        let dir = folder("camilla");
        fs::write(dir.join("coeffs/fir.wav"), wav_bytes(&[1.0, 0.5])).unwrap();
        fs::write(dir.join("config.yml"), CONFIG).unwrap();
        let config = Config::load(&dir.join("config.yml")).unwrap();
        let mut pipeline = Pipeline::new(&config, 8).unwrap();
        assert_eq!((pipeline.inputs(), pipeline.outputs()), (2, 2));

        let mut channels = vec![vec![0.0; 8], vec![0.0; 8]];
        channels[0][0] = 1.0;
        channels[1][1] = 1.0;
        pipeline.process(&mut channels);
        let expect0 = [0.0, 0.0, 0.0, 1.0, 0.5, 0.0, 0.0, 0.0];
        for (got, want) in channels[0].iter().zip(expect0) {
            assert!((got - want).abs() < 1e-9, "{:?}", channels[0]);
        }
        for (got, want) in channels[1].iter().zip([-0.5, -0.5, 0.0]) {
            assert!((got - want).abs() < 1e-4, "{:?}", channels[1]);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_what_secs_does_not_write() {
        let dir = folder("camilla-refuse");
        for (line, message) in [
            (r#"  x: {"type": "Loudness", "parameters": {}}"#, "unsupported filter type Loudness"),
            (r#"  x: {"type": "Biquad", "parameters": {"type": "Notch", "freq": 50, "q": 1}}"#, "Notch"),
            (r#"  x: {"type": "Delay", "parameters": {"delay": 1, "unit": "ms", "subsample": true}}"#, "subsample"),
        ] {
            let text = format!("devices:\n  samplerate: 48000\n  capture: {{\"channels\": 2}}\nfilters:\n{line}\n");
            let error = parse(&text, &dir).err().expect(message).to_string();
            assert!(error.contains(message), "{error}");
        }
        let error = parse("devices:\n  samplerate: 44100\n", &dir).err().unwrap().to_string();
        assert!(error.contains("48000"), "{error}");
        let error = parse("filters:\n  x: Gain\n", &dir).err().unwrap().to_string();
        assert!(error.contains("SECS"), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }
}
