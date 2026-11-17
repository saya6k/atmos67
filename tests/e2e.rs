//! `fixtures/joc.spdif` is OpenJOC's public synthetic JOC fixture
//! (`crates/openjoc-api/tests/fixtures/timestamps.ec3`, 6 access units of
//! 1,536 samples at 48 kHz) wrapped by FFmpeg's IEC 61937 muxer:
//! `ffmpeg -i timestamps.ec3 -c copy -f spdif joc.spdif`.

use std::io::Write;
use std::process::{Command, Stdio};

fn run(args: &[&str], input: &[u8]) -> Vec<f32> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_atmos67"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let input = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input).unwrap());
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert!(output.status.success());
    output
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

const SPDIF: &[u8] = include_bytes!("fixtures/joc.spdif");
/// OpenJOC flushes its internal delay as extra frames when the stream ends.
const DRAIN_TAIL_FRAMES: usize = 32;
/// Mirrors `MAX_LAG_FRAMES` in `src/main.rs`.
const MAX_LAG_FRAMES: usize = 2 * 1536;
const LAYOUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/layout-5.1.2.json");

#[test]
fn renders_joc_capture_to_custom_layout() {
    let samples = run(&["--layout-file", LAYOUT], SPDIF);
    assert_eq!(samples.len(), (6 * 1536 + DRAIN_TAIL_FRAMES) * 8);
    assert!(samples.iter().all(|s| s.is_finite()));
    assert!(samples.iter().any(|s| s.abs() > 1e-4), "output is silent");
}

#[test]
fn survives_a_corrupted_burst() {
    let mut damaged = SPDIF.to_vec();
    let burst = damaged.len() / 6;
    // Wreck the payload of the third burst but leave its preamble intact.
    damaged[2 * burst + 8..2 * burst + 2048].fill(0xA5);
    let samples = run(&["--layout", "5.1"], &damaged);
    assert_eq!(samples.len() % 6, 0);
    let frames = samples.len() / 6;
    // Lost audio is replaced by silence, so output stays on the input clock.
    assert!(frames >= 6 * 1536 - MAX_LAG_FRAMES, "only {frames} frames");
    assert!(frames <= 6 * 1536 + DRAIN_TAIL_FRAMES, "{frames} frames");
    assert!(samples[samples.len() - 1536 * 6..].iter().any(|s| *s != 0.0));
}

#[test]
fn pcm_input_becomes_silence_on_the_capture_clock() {
    // 10,000 output frames' worth of plain PCM (16 input bytes per output frame).
    let samples = run(&["--layout", "5.1"], &vec![0u8; 16 * 10_000]);
    assert_eq!(samples.len(), (10_000 - MAX_LAG_FRAMES) * 6);
    assert!(samples.iter().all(|s| *s == 0.0));
}

#[test]
fn silence_fills_the_gap_after_bursts_stop() {
    let mut input = SPDIF.to_vec();
    input.extend(vec![0u8; 16 * 8_000]);
    let samples = run(&["--layout", "5.1"], &input);
    // Output follows the input clock (6 access units + 8,000 frames) minus the
    // allowed lag; the decoder is flushed before the silence, not at the end.
    let frames = 6 * 1536 + 8_000 - MAX_LAG_FRAMES;
    assert_eq!(samples.len(), frames * 6);
    let decoded = (6 * 1536 + DRAIN_TAIL_FRAMES) * 6;
    assert!(samples[decoded - 1536 * 6..decoded].iter().any(|s| *s != 0.0));
    assert!(samples[decoded..].iter().all(|s| *s == 0.0));
}

#[test]
fn prints_daemon_sources_for_the_layout() {
    let output = Command::new(env!("CARGO_BIN_EXE_atmos67"))
        .args(["--layout", "7.1.4", "--aes67-sources"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json = String::from_utf8(output.stdout).unwrap();
    assert!(json.contains("\"map\": [0, 1, 2, 3, 4, 5, 6, 7]"));
    assert!(json.contains("\"map\": [8, 9, 10, 11]"));
}

#[test]
fn eight_channel_capture_decodes_like_stereo() {
    let wide: Vec<u8> = SPDIF
        .chunks(4)
        .flat_map(|frame| [frame, &[0u8; 12]].concat())
        .collect();
    let from_wide = run(&["--layout", "5.1", "--input-channels", "8"], &wide);
    assert_eq!(from_wide, run(&["--layout", "5.1"], SPDIF));
}

#[test]
fn input_rate_sets_the_silence_clock() {
    // At 48 kHz stereo input, one input frame (4 bytes) is one output frame.
    let samples = run(&["--layout", "5.1", "--input-rate", "48000"], &vec![0u8; 4 * 10_000]);
    assert_eq!(samples.len(), (10_000 - MAX_LAG_FRAMES) * 6);
}

/// Amplitude of the `hz` component of one channel between `start` and
/// `start + len` frames, which must span whole cycles.
fn tone_in(samples: &[f32], channels: usize, channel: usize, hz: f64, start: usize, len: usize) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for i in 0..len {
        let x = f64::from(samples[(start + i) * channels + channel]);
        let phase = 2.0 * std::f64::consts::PI * hz * i as f64 / 48_000.0;
        re += x * phase.cos();
        im += x * phase.sin();
    }
    2.0 * (re * re + im * im).sqrt() / len as f64
}

fn tone(samples: &[f32], channels: usize, channel: usize, hz: f64) -> f64 {
    // 0.1 s to 0.3 s: clear of codec ramp-up, and whole cycles of every test tone.
    tone_in(samples, channels, channel, hz, 4_800, 9_600)
}

/// The tone fixtures carry a different frequency at amplitude 0.25 in each of
/// FL, FR, FC, LFE, SL, SR. FFmpeg made them, for example:
/// `ffmpeg -f lavfi -i "aevalsrc=exprs='0.25*sin(2*PI*300*t)|...':channel_layout=5.1(side):duration=0.4" -c:a ac3 -b:a 448k -f spdif tones-ac3-5.1.spdif`
const TONES: [f64; 6] = [300.0, 450.0, 600.0, 60.0, 750.0, 900.0];

/// `lfe_floor` is the least amplitude accepted for the LFE tone: FFmpeg's
/// experimental DTS encoder stores that channel well below its input level.
fn assert_each_tone_in_its_own_channel(samples: &[f32], lfe_floor: f64) {
    assert!(samples.len() >= 19_200 * 6, "{} samples", samples.len());
    for (channel, own) in TONES.iter().enumerate() {
        for hz in TONES {
            let amplitude = tone(samples, 6, channel, hz);
            if hz == *own && channel == 3 {
                assert!((lfe_floor..0.27).contains(&amplitude), "LFE at {amplitude}");
            } else if hz == *own {
                assert!((amplitude - 0.25).abs() < 0.02, "channel {channel}: {hz} Hz at {amplitude}");
            } else {
                assert!(amplitude < 0.0025, "channel {channel}: {hz} Hz leaks at {amplitude}");
            }
        }
    }
}

#[test]
fn ac3_channels_reach_their_speakers() {
    // AC-3 travels at the audio rate, not four times it.
    let input = include_bytes!("fixtures/tones-ac3-5.1.spdif");
    assert_each_tone_in_its_own_channel(&run(&["--layout", "5.1", "--input-rate", "48000"], input), 0.23);
}

#[test]
fn eac3_without_atmos_plays_its_channel_bed() {
    let input = include_bytes!("fixtures/tones-eac3-5.1.spdif");
    assert_each_tone_in_its_own_channel(&run(&["--layout", "5.1"], input), 0.23);
}

#[test]
fn dts_channels_reach_their_speakers() {
    let input = include_bytes!("fixtures/tones-dts-5.1.spdif");
    assert_each_tone_in_its_own_channel(&run(&["--layout", "5.1", "--input-rate", "48000"], input), 0.05);
}

#[test]
fn truehd_channels_reach_their_speakers() {
    // Lossless and only 0.2 s long; carried in MAT as an 8-channel capture.
    let input = include_bytes!("fixtures/tones-truehd-5.1.spdif");
    let samples = run(&["--layout", "5.1", "--input-channels", "8"], input);
    assert!(samples.len() >= 2_400 * 6, "{} samples", samples.len());
    for (channel, own) in TONES.iter().enumerate() {
        for hz in TONES {
            let amplitude = tone_in(&samples, 6, channel, hz, 800, 1_600);
            if hz == *own {
                assert!((amplitude - 0.25).abs() < 0.001, "channel {channel}: {hz} Hz at {amplitude}");
            } else {
                assert!(amplitude < 0.0001, "channel {channel}: {hz} Hz leaks at {amplitude}");
            }
        }
    }
}
