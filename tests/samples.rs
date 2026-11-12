//! Checks against the real test files in `samples/` (not in git). Run with
//! `cargo test -- --ignored`; needs the `ffmpeg` command.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const SECONDS: &str = "20";

fn sample(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("samples/Audio Test Files").join(name);
    if !path.exists() {
        eprintln!("skipping: {} is not present", path.display());
        return None;
    }
    Some(path)
}

fn ffmpeg(input: &PathBuf, stream: &str, output: &[&str]) -> Vec<u8> {
    ffmpeg_at(input, "0", SECONDS, stream, output)
}

/// Runs FFmpeg on `seconds` of one stream of `input`, starting at `start`.
fn ffmpeg_at(input: &PathBuf, start: &str, seconds: &str, stream: &str, output: &[&str]) -> Vec<u8> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", start, "-t", seconds, "-i"])
        .arg(input)
        .args(["-map", stream])
        .args(output)
        .arg("pipe:1")
        .output()
        .expect("ffmpeg runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

fn atmos67(args: &[&str], input: Vec<u8>) -> (Vec<f32>, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_atmos67"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(&input).unwrap());
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert!(output.status.success());
    (floats(&output.stdout), String::from_utf8_lossy(&output.stderr).into_owned())
}

fn floats(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect()
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-12).log10()
}

/// Per-channel RMS of `samples`, and of its difference from `reference`.
fn compare(samples: &[f32], reference: &[f32], channels: usize) -> Vec<(f64, f64, f64)> {
    let frames = samples.len().min(reference.len()) / channels;
    (0..channels)
        .map(|c| {
            let (mut ours, mut theirs, mut diff) = (0.0, 0.0, 0.0);
            for i in 0..frames {
                let (a, b) = (f64::from(samples[i * channels + c]), f64::from(reference[i * channels + c]));
                ours += a * a;
                theirs += b * b;
                diff += (a - b) * (a - b);
            }
            let rms = |sum: f64| (sum / frames as f64).sqrt();
            (rms(ours), rms(theirs), rms(diff))
        })
        .collect()
}

/// A channel-based stream rendered on its own layout must equal FFmpeg's
/// decode of the same stream, channel for channel. Returns atmos67's log.
fn assert_matches_ffmpeg(name: &str, layout: &str, channels: usize, mux: &[&str], extra: &[&str]) -> String {
    let Some(path) = sample(name) else { return String::new() };
    let reference = floats(&ffmpeg(&path, "0:a:0", &["-f", "f32le", "-c:a", "pcm_f32le"]));
    let spdif = ffmpeg(&path, "0:a:0", &[&["-c", "copy"], mux, &["-f", "spdif"]].concat());
    // Output must last as long as the capture did (SPEC success criterion 6).
    let capture_channels = if extra.contains(&"--input-channels") { 8 } else { 2 };
    let capture_rate = if extra.contains(&"--input-rate") { 48_000 } else { 192_000 };
    let captured = spdif.len() / (capture_channels * 2) * 48_000 / capture_rate;
    let (samples, log) = atmos67(&[&["--layout", layout], extra].concat(), spdif);
    let frames = samples.len() / channels;
    eprintln!("{name}: {frames} frames out for {captured} frames of capture");
    assert!(frames.abs_diff(captured) <= 2 * 1536 + 1024, "{name}: output length\n{log}");
    assert!(samples.len() >= reference.len() * 9 / 10, "{name}: {} of {} samples\n{log}", samples.len(), reference.len());
    for (channel, (ours, theirs, diff)) in compare(&samples, &reference, channels).iter().enumerate() {
        eprintln!("{name} ch{channel}: ours {:.1} dB, ffmpeg {:.1} dB, difference {:.1} dB", db(*ours), db(*theirs), db(*diff));
        assert!((db(*ours) - db(*theirs)).abs() < 0.5 || *theirs < 1e-6, "{name} channel {channel} level\n{log}");
        assert!(db(*diff) < -60.0, "{name} channel {channel} differs\n{log}");
    }
    log
}

/// DTS-HD needs the high-bitrate HDMI mode: eight channels at 192 kHz.
const DTS_HD: [&str; 2] = ["-dtshd_rate", "768000"];
const EIGHT_CHANNELS: [&str; 2] = ["--input-channels", "8"];

/// TrueHD travels in MAT frames, also in the high-bitrate mode.
#[test]
#[ignore]
fn truehd_7_1_matches_ffmpeg() {
    assert_matches_ffmpeg("Dolby TrueHD 7.1.mkv", "7.1", 8, &[], &EIGHT_CHANNELS);
}

#[test]
#[ignore]
fn dts_hd_ma_5_1_matches_ffmpeg() {
    assert_matches_ffmpeg("DTS-HD MA 5.1.mkv", "5.1", 6, &DTS_HD, &EIGHT_CHANNELS);
}

#[test]
#[ignore]
fn dts_hd_ma_7_1_matches_ffmpeg() {
    assert_matches_ffmpeg("DTS-HD MA 7.1.mkv", "7.1", 8, &DTS_HD, &EIGHT_CHANNELS);
}

#[test]
#[ignore]
fn dts_hd_high_resolution_7_1_matches_ffmpeg() {
    assert_matches_ffmpeg("DTS-HD High Resolution 7.1.dtshd", "7.1", 8, &DTS_HD, &EIGHT_CHANNELS);
}

#[test]
#[ignore]
fn dts_x_plays_its_bed_and_says_objects_are_not_rendered() {
    let log = assert_matches_ffmpeg("DTS-X 7.1.4.mkv", "7.1", 8, &DTS_HD, &EIGHT_CHANNELS);
    if sample("DTS-X 7.1.4.mkv").is_some() {
        assert_eq!(log.matches("DTS:X objects are not rendered").count(), 1, "{log}");
    }
}

#[test]
#[ignore]
fn ac3_5_1_matches_ffmpeg() {
    assert_matches_ffmpeg("Dolby Digital AC-3 5.1.mp4", "5.1", 6, &[], &["--input-rate", "48000"]);
}

#[test]
#[ignore]
fn eac3_5_1_matches_ffmpeg() {
    assert_matches_ffmpeg("Dolby Digital Plus E-AC-3 5.1.mp4", "5.1", 6, &[], &[]);
}

#[test]
#[ignore]
fn eac3_7_1_matches_ffmpeg() {
    assert_matches_ffmpeg("Dolby Digital Plus E-AC-3 7.1.mp4", "7.1", 8, &[], &[]);
}

fn rms_per_channel(samples: &[f32], channels: usize) -> Vec<f64> {
    let frames = samples.len() / channels;
    (0..channels)
        .map(|c| {
            let sum: f64 = (0..frames).map(|i| f64::from(samples[i * channels + c]).powi(2)).sum();
            (sum / frames as f64).sqrt()
        })
        .collect()
}

#[test]
#[ignore]
fn stream_openjoc_rejects_plays_its_channel_bed() {
    let Some(path) = sample("Atmos test tones 1000Hz V4.mp4") else { return };
    let reference = floats(&ffmpeg(&path, "0:a:0", &["-f", "f32le", "-c:a", "pcm_f32le"]));
    let spdif = ffmpeg(&path, "0:a:0", &["-c", "copy", "-f", "spdif"]);
    let (samples, log) = atmos67(&["--layout", "5.1"], spdif);
    assert_eq!(log.matches("playing its channel bed").count(), 1, "{log}");
    for (channel, (ours, theirs, diff)) in compare(&samples, &reference, 6).iter().enumerate() {
        eprintln!("ch{channel}: ours {:.1} dB, ffmpeg {:.1} dB, difference {:.1} dB", db(*ours), db(*theirs), db(*diff));
        assert!(db(*diff) < -60.0, "channel {channel}\n{log}");
    }
    assert!(rms_per_channel(&samples, 6).iter().any(|rms| *rms > 1e-3), "silent\n{log}");
}

#[test]
#[ignore]
fn atmos_streams_still_use_openjoc() {
    let Some(path) = sample("Dolby Atmos test by Buzz_Buzz_Buzz_ - 7.1.4 master with 5.1 E-AC-3 bed.mp4") else { return };
    let spdif = ffmpeg(&path, "0:a:0", &["-c", "copy", "-f", "spdif"]);
    let (samples, log) = atmos67(&["--layout", "7.1.4"], spdif);
    assert!(!log.contains("channel bed"), "{log}");
    // The 5.1 core has no height channels; only object decoding fills them.
    let rms = rms_per_channel(&samples, 12);
    assert!(rms[8..].iter().any(|r| *r > 1e-4), "{rms:?}");
}

#[test]
#[ignore]
fn one_rejected_access_unit_does_not_switch_the_stream_to_its_bed() {
    let Some(path) = sample("Dolby Atmos E-AC-3 5.1.2.mp4") else { return };
    let spdif = ffmpeg(&path, "0:a:0", &["-c", "copy", "-f", "spdif"]);
    let (samples, log) = atmos67(&["--layout", "5.1.2"], spdif);
    // Its first access unit carries metadata OpenJOC rejects; the rest decode.
    assert!(log.contains("playing its channel bed"), "{log}");
    let rms = rms_per_channel(&samples, 8);
    assert!(rms[6..].iter().all(|r| *r > 1e-4), "{rms:?}");
}

/// This file names one speaker of a 7.1.4 room per minute, in this order.
const WALK: [&str; 12] = ["FL", "FR", "FC", "LFE", "Ls", "Rs", "Lb", "Rb", "TFL", "TFR", "TBL", "TBR"];
/// Output order of the `7.1.4` preset.
const PRESET_7_1_4: [&str; 12] = ["FL", "FR", "FC", "LFE", "Lb", "Rb", "Ls", "Rs", "TFL", "TFR", "TBL", "TBR"];

/// Levels in dB of the 7.1.4 render of five seconds from the middle of
/// `minute`, taken from stream `stream` of the 7.1.4 walk file.
fn walk_levels(path: &PathBuf, stream: &str, minute: usize, extra: &[&str]) -> Vec<f64> {
    let start = (minute * 60 + 25).to_string();
    let spdif = ffmpeg_at(path, &start, "5", stream, &["-c", "copy", "-f", "spdif"]);
    let (samples, log) = atmos67(&[&["--layout", "7.1.4"], extra].concat(), spdif);
    assert!(samples.len() > 3 * 48_000 * 12, "{} samples\n{log}", samples.len());
    // Skip the first second: decoding starts at the next sync point.
    rms_per_channel(&samples[48_000 * 12..], 12).iter().map(|rms| db(*rms)).collect()
}

#[test]
#[ignore]
fn truehd_atmos_objects_reach_the_speaker_they_name() {
    let Some(path) = sample("Dolby Atmos TrueHD, E-AC-3 7.1.4.mkv") else { return };
    let mut agree = 0;
    for (minute, expected) in WALK.iter().enumerate() {
        let truehd = walk_levels(&path, "0:a:0", minute, &EIGHT_CHANNELS);
        let joc = walk_levels(&path, "0:a:2", minute, &[]);
        let loudest = |levels: &[f64]| {
            let index = (0..12).max_by(|a, b| levels[*a].total_cmp(&levels[*b])).unwrap();
            PRESET_7_1_4[index]
        };
        let own = PRESET_7_1_4.iter().position(|name| name == expected).unwrap();
        let mut others = truehd.clone();
        others.remove(own);
        let leak = others.iter().cloned().fold(f64::MIN, f64::max);
        eprintln!(
            "{expected}: TrueHD loudest {} at {:.1} dB (next {leak:.1} dB), E-AC-3 JOC loudest {}",
            loudest(&truehd), truehd[own], loudest(&joc)
        );
        assert_eq!(loudest(&truehd), *expected);
        assert!(truehd[own] > -40.0, "{expected} is too quiet");
        assert!(truehd[own] - leak > 20.0, "{expected} leaks into another speaker");
        agree += usize::from(loudest(&truehd) == loudest(&joc));
    }
    // SPEC success criterion 3: at least 80 % agreement with the JOC track.
    assert!(agree * 10 >= WALK.len() * 8, "{agree} of {} agree", WALK.len());
}
