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
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect()
}

const SPDIF: &[u8] = include_bytes!("fixtures/joc.spdif");
/// OpenJOC flushes its internal delay as extra frames when the stream ends.
const DRAIN_TAIL_FRAMES: usize = 32;
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
    // Whole access units on both sides of the damage, plus the end-of-stream tail.
    assert!(frames >= 3 * 1536, "only {frames} frames survived");
    assert_eq!(frames % 1536, DRAIN_TAIL_FRAMES);
}
