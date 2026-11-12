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
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-t", SECONDS, "-i"])
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
/// decode of the same stream, channel for channel.
fn assert_matches_ffmpeg(name: &str, stream: &str, layout: &str, channels: usize, extra: &[&str]) {
    let Some(path) = sample(name) else { return };
    let reference = floats(&ffmpeg(&path, stream, &["-f", "f32le", "-c:a", "pcm_f32le"]));
    let spdif = ffmpeg(&path, stream, &["-c", "copy", "-f", "spdif"]);
    let (samples, log) = atmos67(&[&["--layout", layout], extra].concat(), spdif);
    assert!(samples.len() >= reference.len() * 9 / 10, "{name}: {} of {} samples\n{log}", samples.len(), reference.len());
    for (channel, (ours, theirs, diff)) in compare(&samples, &reference, channels).iter().enumerate() {
        eprintln!("{name} ch{channel}: ours {:.1} dB, ffmpeg {:.1} dB, difference {:.1} dB", db(*ours), db(*theirs), db(*diff));
        assert!((db(*ours) - db(*theirs)).abs() < 0.5 || *theirs < 1e-6, "{name} channel {channel} level\n{log}");
        assert!(db(*diff) < -60.0, "{name} channel {channel} differs\n{log}");
    }
}

#[test]
#[ignore]
fn ac3_5_1_matches_ffmpeg() {
    assert_matches_ffmpeg("Dolby Digital AC-3 5.1.mp4", "0:a:0", "5.1", 6, &["--input-rate", "48000"]);
}

#[test]
#[ignore]
fn eac3_5_1_matches_ffmpeg() {
    assert_matches_ffmpeg("Dolby Digital Plus E-AC-3 5.1.mp4", "0:a:0", "5.1", 6, &[]);
}

#[test]
#[ignore]
fn eac3_7_1_matches_ffmpeg() {
    assert_matches_ffmpeg("Dolby Digital Plus E-AC-3 7.1.mp4", "0:a:0", "7.1", 8, &[]);
}
