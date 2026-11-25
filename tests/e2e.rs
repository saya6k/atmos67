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
fn silent_pcm_plays_as_silence_on_the_capture_clock() {
    // 10,000 output frames' worth of plain PCM at 192 kHz (16 input bytes per
    // output frame), resampled: all of it, less what the filter holds back.
    let samples = run(&["--layout", "5.1"], &vec![0u8; 16 * 10_000]);
    assert!((9_936 * 6..=10_000 * 6).contains(&samples.len()), "{} frames", samples.len() / 6);
    assert!(samples.iter().all(|s| *s == 0.0));
}

#[test]
fn silence_fills_the_gap_after_bursts_stop() {
    let mut input = SPDIF.to_vec();
    input.extend(vec![0u8; 16 * 8_000]);
    let samples = run(&["--layout", "5.1"], &input);
    // Output follows the input clock (6 access units + 8,000 frames), within
    // the allowed lag; the decoder is flushed before the silence, not at the
    // end. The silence after the bursts plays as PCM once it has lasted 50 ms.
    let frames = 6 * 1536 + 8_000;
    assert!(((frames - MAX_LAG_FRAMES) * 6..=frames * 6).contains(&samples.len()), "{} frames", samples.len() / 6);
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
fn writes_a_bass_managed_design_for_the_layout() {
    let folder = std::env::temp_dir().join(format!("atmos67-e2e-design-{}", std::process::id()));
    let output = Command::new(env!("CARGO_BIN_EXE_atmos67"))
        .args(["--layout", "7.1.4", "--write-design", folder.to_str().unwrap()])
        .args(["--subs", "2", "--crossover", "front=80", "--slope", "12", "--trim", "SW2=-1.5"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let manifest = std::fs::read_to_string(folder.join("secs-design.json")).unwrap();
    assert!(manifest.contains("\"SW2\""), "{manifest}");
    let config = std::fs::read_to_string(folder.join("configs/design.yml")).unwrap();
    assert!(config.contains("\"order\": 2") && config.contains("trim_SW2"), "{config}");
    std::fs::remove_dir_all(folder).unwrap();
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
fn input_rate_sets_the_output_clock() {
    // At 48 kHz stereo input, one input frame (4 bytes) is one output frame;
    // the last may wait for more input.
    let samples = run(&["--layout", "5.1", "--input-rate", "48000"], &vec![0u8; 4 * 10_000]);
    assert!((9_999 * 6..=10_000 * 6).contains(&samples.len()), "{} frames", samples.len() / 6);
}

/// 1 kHz on the left and 1.5 kHz on the right at 0.25, sampled at `rate`;
/// neither is among `TONES`.
fn stereo_tones(seconds: f64, rate: f64) -> Vec<f32> {
    let tone = |hz: f64, i: usize| (0.25 * (2.0 * std::f64::consts::PI * hz * i as f64 / rate).sin()) as f32;
    (0..(seconds * rate) as usize).flat_map(|i| [tone(1000.0, i), tone(1500.0, i)]).collect()
}

fn s16(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| ((s * 32_767.0).round() as i16).to_le_bytes()).collect()
}

/// Checks that the left and right PCM tones are in FL and FR alone of a 5.1
/// output, over the 0.2 s from frame `start`.
fn assert_stereo_tones_at(samples: &[f32], start: usize) {
    for channel in 0..6 {
        for (side, hz) in [(0, 1000.0), (1, 1500.0)] {
            let amplitude = tone_in(samples, 6, channel, hz, start, 9_600);
            if channel == side {
                assert!((amplitude - 0.25).abs() < 0.002, "channel {channel}: {hz} Hz at {amplitude}");
            } else {
                assert!(amplitude < 0.0025, "channel {channel}: {hz} Hz leaks at {amplitude}");
            }
        }
    }
}

#[test]
fn stereo_pcm_plays_on_the_front_speakers() {
    let input = s16(&stereo_tones(0.4, 48_000.0));
    let samples = run(&["--layout", "5.1", "--input-rate", "48000"], &input);
    assert!((19_199 * 6..=19_200 * 6).contains(&samples.len()), "{} frames", samples.len() / 6);
    assert_stereo_tones_at(&samples, 4_800);
}

#[test]
fn float_pcm_at_44_1_khz_is_resampled_to_48_khz() {
    let input: Vec<u8> = stereo_tones(0.4, 44_100.0).iter().flat_map(|s| s.to_le_bytes()).collect();
    let samples = run(&["--layout", "5.1", "--input-rate", "44100", "--input-format", "FLOAT_LE"], &input);
    assert!((19_100 * 6..=19_200 * 6).contains(&samples.len()), "{} frames", samples.len() / 6);
    assert_stereo_tones_at(&samples, 4_800);
}

#[test]
fn a_bitstream_between_pcm_is_decoded_and_never_played_as_pcm() {
    // 0.4 s of stereo PCM, the AC-3 tones (19,968 frames), 0.4 s of PCM, at 48 kHz.
    let pcm = s16(&stereo_tones(0.4, 48_000.0));
    let ac3 = include_bytes!("fixtures/tones-ac3-5.1.spdif");
    let input = [&pcm[..], ac3, &pcm[..]].concat();
    let samples = run(&["--layout", "5.1", "--input-rate", "48000"], &input);
    let frames = samples.len() / 6;
    assert!((19_200 + 19_968 + 19_200 - 6_000..=19_200 + 19_968 + 19_200).contains(&frames), "{frames} frames");
    // Encoded data played as PCM would be loud noise.
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!(peak < 0.3, "peak {peak}");
    assert_stereo_tones_at(&samples, 4_800);
    // The AC-3 tones, each in its channel, and none of the PCM.
    for (channel, hz) in TONES.iter().enumerate().filter(|(c, _)| *c != 3) {
        let amplitude = tone_in(&samples, 6, channel, *hz, 19_200 + 4_800, 9_600);
        assert!((amplitude - 0.25).abs() < 0.02, "channel {channel}: {hz} Hz at {amplitude}");
    }
    assert!(tone_in(&samples, 6, 0, 1000.0, 19_200 + 4_800, 9_600) < 0.0025);
    // The PCM again at the end.
    assert_stereo_tones_at(&samples, frames - 9_600 - 2_400);
}

#[test]
fn the_pcm_channel_marks_the_pcm_frames_alone() {
    let pcm = s16(&stereo_tones(0.4, 48_000.0));
    let ac3 = include_bytes!("fixtures/tones-ac3-5.1.spdif");
    let input = [&pcm[..], ac3, &pcm[..]].concat();
    let args = ["--layout", "5.1", "--input-rate", "48000"];
    let plain = run(&args, &input);
    let marked = run(&[&args[..], &["--pcm-channel"]].concat(), &input);
    // The speakers as without the channel, and one more channel of 0 or 1.
    assert_eq!(marked.len(), plain.len() / 6 * 7);
    let frames: Vec<&[f32; 7]> = marked.as_chunks::<7>().0.iter().collect();
    assert!(frames.iter().zip(plain.as_chunks::<6>().0).all(|(m, p)| m[..6] == p[..]));
    assert!(frames.iter().all(|f| f[6] == 0.0 || f[6] == 1.0));
    // PCM at the start and the end, the AC-3 tones between are not.
    assert!(frames[4_800..19_000].iter().all(|f| f[6] == 1.0));
    assert!(frames[19_200 + 4_800..19_200 + 14_400].iter().all(|f| f[6] == 0.0));
    // The PCM at the end is shorter: the 50 ms after the bitstream is dropped.
    assert!(frames[frames.len() - 6_000..].iter().all(|f| f[6] == 1.0));
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
