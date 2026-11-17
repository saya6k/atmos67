//! Designs written from settings rather than measured: bass management,
//! level trims and delays, in the same folder format as a SECS 2R export so
//! the sender runs both the same way (tasks/dsp.md D5).
//!
//! Without a base design, each main speaker group may cross over to the
//! subwoofers: the mains are high-passed and their low-passed sends summed,
//! with the LFE (+10 dB, low-passed at 120 Hz), into a bus that every
//! subwoofer plays at 1/N, so N subwoofers together sound like one. The
//! whole design runs 10 dB down so the sums do not clip, as SECS designs do.
//! With a base design, only the trims and delays are added to its outputs.

use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::Path;

use crate::layout::canonical;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const HEADROOM_DB: f64 = -10.0;
const LFE_GAIN_DB: f64 = 10.0;
const LFE_LOWPASS_HZ: f64 = 120.0;
pub const GROUPS: [&str; 5] = ["front", "center", "surround", "back", "top"];

#[derive(Default)]
pub struct Settings {
    pub subs: usize,
    /// Crossover per group in Hz; a group left out plays full range.
    pub crossover: HashMap<String, f64>,
    /// Linkwitz–Riley order of the crossovers, 2 or 4.
    pub order: usize,
    /// Output level trims in dB, by SECS role.
    pub trims: HashMap<String, f64>,
    /// Output delays in ms, by SECS role.
    pub delays: HashMap<String, f64>,
}

/// The SECS 2R role of a rendered speaker.
fn role(label: &str) -> Option<&'static str> {
    Some(match canonical(label) {
        "FL" => "L",
        "FR" => "R",
        "FC" => "C",
        "LFE" => "LFE",
        "Ls" => "Ls",
        "Rs" => "Rs",
        "Lb" => "Lb",
        "Rb" => "Rb",
        "Lw" => "Lw",
        "Rw" => "Rw",
        "TFL" => "Tfl",
        "TFR" => "Tfr",
        "TBL" => "Tbl",
        "TBR" => "Tbr",
        "Ltm" => "Tml",
        "Rtm" => "Tmr",
        _ => return None,
    })
}

fn group(role: &str) -> &'static str {
    match role {
        "C" => "center",
        "Ls" | "Rs" => "surround",
        "Lb" | "Rb" => "back",
        role if role.starts_with('T') => "top",
        _ => "front",
    }
}

fn json_list(names: &[String]) -> String {
    let quoted: Vec<String> = names.iter().map(|n| serde_json::Value::from(n.as_str()).to_string()).collect();
    format!("[{}]", quoted.join(", "))
}

fn gain(db: f64) -> String {
    format!(r#"{{"type": "Gain", "parameters": {{"gain": {db}, "inverted": false}}}}"#)
}

fn delay(ms: f64) -> String {
    format!(r#"{{"type": "Delay", "parameters": {{"delay": {ms}, "unit": "ms", "subsample": false}}}}"#)
}

fn pass(kind: &str, hz: f64, order: usize) -> String {
    format!(r#"{{"type": "BiquadCombo", "parameters": {{"type": "LinkwitzRiley{kind}", "freq": {hz}, "order": {order}}}}}"#)
}

fn filter_step(channel: usize, names: &[&str]) -> String {
    let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    format!(r#"{{"type": "Filter", "channels": [{channel}], "names": {}}}"#, json_list(&names))
}

fn mixer(inputs: usize, sources: &[Vec<(usize, f64)>]) -> String {
    let mapping: Vec<String> = sources
        .iter()
        .enumerate()
        .map(|(dest, sources)| {
            let sources: Vec<String> = sources
                .iter()
                .map(|(channel, gain)| format!(r#"{{"channel": {channel}, "gain": {gain}, "inverted": false}}"#))
                .collect();
            format!(r#"{{"dest": {dest}, "sources": [{}]}}"#, sources.join(", "))
        })
        .collect();
    format!(r#"{{"channels": {{"in": {inputs}, "out": {}}}, "mapping": [{}]}}"#, sources.len(), mapping.join(", "))
}

/// Writes a design for rendered speakers `labels` into `folder`, either from
/// scratch or as `base` with the trims and delays added.
pub fn write(folder: &Path, labels: &[String], base: Option<&Path>, settings: &Settings) -> Result<()> {
    if let Some(group) = settings.crossover.keys().find(|g| !GROUPS.contains(&g.as_str())) {
        return Err(format!("unknown crossover group {group}; groups are {}", GROUPS.join(", ")).into());
    }
    if !matches!(settings.order, 2 | 4) {
        return Err("crossover slope is 12 or 24 dB/octave".into());
    }
    let (manifest, mut lines) = match base {
        Some(base) => copy_base(base, folder)?,
        None => scratch(labels, settings)?,
    };
    let outputs: Vec<String> = manifest["outputs"]
        .as_array()
        .ok_or("secs-design.json has no outputs")?
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();
    for role in settings.trims.keys().chain(settings.delays.keys()) {
        if !outputs.contains(role) {
            return Err(format!("{role} is not an output of the design ({})", outputs.join(" ")).into());
        }
    }

    // Trims and delays go last, on the outputs.
    let mut filters = Vec::new();
    let mut steps = Vec::new();
    for (channel, role) in outputs.iter().enumerate() {
        let mut names = Vec::new();
        if let Some(db) = settings.trims.get(role).filter(|db| **db != 0.0) {
            filters.push(format!("  trim_{role}: {}", gain(*db)));
            names.push(format!("trim_{role}"));
        }
        if let Some(ms) = settings.delays.get(role).filter(|ms| **ms != 0.0) {
            if *ms < 0.0 {
                return Err(format!("negative delay for {role}").into());
            }
            filters.push(format!("  delay_{role}: {}", delay(*ms)));
            names.push(format!("delay_{role}"));
        }
        if !names.is_empty() {
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            steps.push(format!("  - {}", filter_step(channel, &names)));
        }
    }
    let at = lines.iter().position(|l| l.starts_with("mixers:")).ok_or("configuration without mixers")?;
    lines.splice(at..at, filters);
    lines.extend(steps);

    fs::create_dir_all(folder.join("configs"))?;
    fs::write(folder.join("configs/design.yml"), lines.join("\n") + "\n")?;
    fs::write(folder.join("secs-design.json"), serde_json::to_string_pretty(&manifest)?)?;
    Ok(())
}

/// Copies a SECS export's manifest and coefficient files into `folder` and
/// returns them with its configuration's lines.
fn copy_base(base: &Path, folder: &Path) -> Result<(serde_json::Value, Vec<String>)> {
    let mut manifest: serde_json::Value = serde_json::from_slice(&fs::read(base.join("secs-design.json"))?)?;
    let text = fs::read_to_string(super::design::config_path(base, &manifest)?)?;
    // Only the correcting configuration is copied.
    if let Some(manifest) = manifest.as_object_mut() {
        manifest.remove("bypass_config");
    }
    if base.join("coeffs").is_dir() {
        fs::create_dir_all(folder.join("coeffs"))?;
        for entry in fs::read_dir(base.join("coeffs"))? {
            let path = entry?.path();
            fs::copy(&path, folder.join("coeffs").join(path.file_name().ok_or("file name")?))?;
        }
    }
    Ok((manifest, text.lines().map(String::from).collect()))
}

fn scratch(labels: &[String], settings: &Settings) -> Result<(serde_json::Value, Vec<String>)> {
    let inputs: Vec<String> = labels
        .iter()
        .map(|label| role(label).map(String::from).ok_or_else(|| format!("speaker {label} has no SECS 2R role")))
        .collect::<std::result::Result<_, String>>()?;
    let mains: Vec<usize> = (0..inputs.len()).filter(|&i| inputs[i] != "LFE").collect();
    let lfe = inputs.iter().position(|r| r == "LFE");
    let subs = settings.subs;
    // Mains that send their low end to the subwoofers, with their crossover.
    let sends: Vec<(usize, f64)> = if subs == 0 {
        Vec::new()
    } else {
        mains.iter().filter_map(|&i| settings.crossover.get(group(&inputs[i])).map(|hz| (i, *hz))).collect()
    };

    let mut filters = vec![format!("  headroom: {}", gain(HEADROOM_DB))];
    let mut steps = vec![
        format!(
            r#"  - {{"type": "Filter", "channels": [{}], "names": ["headroom"]}}"#,
            (0..inputs.len()).map(|i| i.to_string()).collect::<Vec<_>>().join(", ")
        ),
        r#"  - {"type": "Mixer", "name": "fork"}"#.to_string(),
    ];
    // After the fork: the mains, then one send per crossed-over main, then the LFE.
    let mut fork: Vec<Vec<(usize, f64)>> = mains.iter().map(|&i| vec![(i, 0.0)]).collect();
    for (index, &(main, hz)) in sends.iter().enumerate() {
        let name = format!("xo{hz}");
        if !filters.iter().any(|f| f.starts_with(&format!("  hp_{name}:"))) {
            filters.push(format!("  hp_{name}: {}", pass("Highpass", hz, settings.order)));
            filters.push(format!("  lp_{name}: {}", pass("Lowpass", hz, settings.order)));
        }
        let position = mains.iter().position(|&m| m == main).expect("a main");
        steps.push(format!("  - {}", filter_step(position, &[&format!("hp_{name}")])));
        steps.push(format!("  - {}", filter_step(mains.len() + index, &[&format!("lp_{name}")])));
        fork.push(vec![(main, 0.0)]);
    }
    let lfe_channel = fork.len();
    if let (Some(lfe), true) = (lfe, subs > 0) {
        filters.push(format!("  lfe_gain: {}", gain(LFE_GAIN_DB)));
        filters.push(format!("  lfe_lp: {}", pass("Lowpass", LFE_LOWPASS_HZ, 4)));
        steps.push(format!("  - {}", filter_step(lfe_channel, &["lfe_gain", "lfe_lp"])));
        fork.push(vec![(lfe, 0.0)]);
    }
    // Each subwoofer plays the bus at 1/N.
    let share = -20.0 * (subs.max(1) as f64).log10();
    let bus: Vec<(usize, f64)> = (mains.len()..fork.len()).map(|c| (c, share)).collect();
    let mut sum: Vec<Vec<(usize, f64)>> = (0..mains.len()).map(|c| vec![(c, 0.0)]).collect();
    sum.extend((0..subs).map(|_| bus.clone()));
    steps.push(r#"  - {"type": "Mixer", "name": "sum"}"#.to_string());

    let mut outputs: Vec<String> = mains.iter().map(|&i| inputs[i].clone()).collect();
    outputs.extend((1..=subs).map(|n| format!("SW{n}")));
    let mut lines = vec![
        "# atmos67 manual design: bass management, trims and delays".to_string(),
        "devices:".into(),
        "  samplerate: 48000".into(),
        format!(r#"  capture: {{"channels": {}}}"#, inputs.len()),
        "filters:".into(),
    ];
    lines.extend(filters);
    lines.push("mixers:".into());
    lines.push(format!("  fork: {}", mixer(inputs.len(), &fork)));
    lines.push(format!("  sum: {}", mixer(fork.len(), &sum)));
    lines.push("pipeline:".into());
    lines.extend(steps);
    let manifest = serde_json::json!({ "generator": "atmos67", "inputs": inputs, "outputs": outputs });
    Ok((manifest, lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::camilla::{Config, Pipeline};
    use realfft::RealFftPlanner;

    const LABELS: [&str; 12] = ["FL", "FR", "FC", "LFE", "Lb", "Rb", "Ls", "Rs", "TFL", "TFR", "TBL", "TBR"];

    fn labels() -> Vec<String> {
        LABELS.iter().map(|s| s.to_string()).collect()
    }

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("atmos67-manual-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// Impulse responses of every output to an impulse on input `input`.
    fn responses(folder: &Path, input: usize, length: usize) -> Vec<Vec<f64>> {
        let config = Config::load(&folder.join("configs/design.yml")).unwrap();
        let mut pipeline = Pipeline::new(&config, length).unwrap();
        let mut channels = vec![vec![0.0; length]; LABELS.len()];
        channels[input][0] = 1.0;
        pipeline.process(&mut channels);
        channels
    }

    fn magnitude_db(response: &[f64]) -> Vec<f64> {
        let mut time = response.to_vec();
        let fft = RealFftPlanner::<f64>::new().plan_fft_forward(time.len());
        let mut spectrum = fft.make_output_vec();
        fft.process(&mut time, &mut spectrum).unwrap();
        spectrum.iter().map(|c| 20.0 * c.norm().log10()).collect()
    }

    fn settings() -> Settings {
        Settings {
            subs: 2,
            crossover: GROUPS.iter().map(|g| (g.to_string(), 80.0)).collect(),
            order: 4,
            ..Settings::default()
        }
    }

    #[test]
    fn mains_and_subwoofers_sum_flat_across_the_crossover() {
        let dir = folder("flat");
        write(&dir, &labels(), None, &settings()).unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("secs-design.json")).unwrap()).unwrap();
        assert_eq!(manifest["outputs"].as_array().unwrap().len(), 13);
        assert_eq!(manifest["outputs"][11], "SW1");

        let length = 1 << 16;
        let out = responses(&dir, 0, length);
        // FL, then both subwoofers, add up to a flat response 10 dB down.
        let sum: Vec<f64> = (0..length).map(|n| out[0][n] + out[11][n] + out[12][n]).collect();
        let bins = length / 2;
        let hz = |bin: usize| bin as f64 * 48_000.0 / length as f64;
        for (bin, db) in magnitude_db(&sum).iter().enumerate().take(bins) {
            if (20.0..20_000.0).contains(&hz(bin)) {
                assert!((db - HEADROOM_DB).abs() < 0.5, "{db} dB at {} Hz", hz(bin));
            }
        }
        // FL keeps no bass and the subwoofers no treble.
        let fl = magnitude_db(&out[0]);
        let sub = magnitude_db(&out[11]);
        let bin = |f: f64| (f * length as f64 / 48_000.0) as usize;
        assert!(fl[bin(20.0)] < HEADROOM_DB - 40.0);
        assert!(sub[bin(1000.0)] < HEADROOM_DB - 60.0);
        // No other speaker plays FL.
        assert!(out[1..11].iter().flatten().all(|s| s.abs() < 1e-12));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn lfe_goes_to_the_subwoofers_10_db_up() {
        let dir = folder("lfe");
        write(&dir, &labels(), None, &settings()).unwrap();
        let length = 1 << 14;
        let out = responses(&dir, 3, length);
        let bin = |f: f64| (f * length as f64 / 48_000.0) as usize;
        let sub = magnitude_db(&out[11]);
        // +10 dB, 10 dB of headroom, and half the bus for each of two subwoofers.
        assert!((sub[bin(30.0)] - (-6.0206)).abs() < 0.2, "{}", sub[bin(30.0)]);
        assert!(out[..11].iter().flatten().all(|s| s.abs() < 1e-12));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn trims_and_delays_apply_to_their_output() {
        let dir = folder("trim");
        let mut s = Settings { order: 4, ..Settings::default() };
        s.trims.insert("C".into(), -6.0206);
        s.delays.insert("C".into(), 1.0);
        write(&dir, &labels(), None, &s).unwrap();
        let out = responses(&dir, 2, 256);
        // C is output 2; 1 ms is 48 samples; -10 dB headroom and -6 dB trim.
        let peak = out[2].iter().enumerate().fold((0, 0.0), |m, (i, v)| if v.abs() > m.1 { (i, v.abs()) } else { m });
        assert_eq!(peak.0, 48);
        assert!((peak.1 - 0.5 * 10f64.powf(-0.5)).abs() < 1e-4, "{}", peak.1);

        // Added onto a base design, which keeps its own processing.
        let overlay = folder("overlay");
        let mut t = Settings { order: 4, ..Settings::default() };
        t.trims.insert("SW1".into(), 3.0);
        let base = folder_with_subs();
        write(&overlay, &labels(), Some(&base), &t).unwrap();
        let text = fs::read_to_string(overlay.join("configs/design.yml")).unwrap();
        assert!(text.contains("trim_SW1"));
        fs::remove_dir_all(base).unwrap();
        assert!(write(&overlay, &labels(), Some(&dir), &Settings { order: 4, trims: [("SW9".into(), 1.0)].into(), ..Settings::default() }).is_err());
        fs::remove_dir_all(dir).unwrap();
        fs::remove_dir_all(overlay).unwrap();
    }

    fn folder_with_subs() -> std::path::PathBuf {
        let dir = folder("base");
        write(&dir, &labels(), None, &settings()).unwrap();
        dir
    }

    #[test]
    fn refuses_unknown_groups_and_speakers() {
        let dir = folder("refuse");
        let mut s = settings();
        s.crossover.insert("wides".into(), 80.0);
        assert!(write(&dir, &labels(), None, &s).unwrap_err().to_string().contains("wides"));
        let odd = vec!["FL".to_string(), "TC".to_string()];
        assert!(write(&dir, &odd, None, &settings()).unwrap_err().to_string().contains("TC"));
    }
}
