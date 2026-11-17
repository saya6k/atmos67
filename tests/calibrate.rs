//! Success criterion 3 of tasks/dsp.md: `atmos67 calibrate` measures a room
//! through the running sender. A simulated room receives the sender's RTP,
//! delays and scales each speaker by known amounts and writes their sum, as
//! the microphone, into a FIFO the calibration reads with `cat`, so the
//! microphone lags the sender by no more than a real one would. The correction SECS 2R
//! designs from it must undo those delays and levels.
//!
//! Then `calibrate latency` must find the delay of L in the room.
//!
//! Ignored by default: it measures in real time and designs for minutes.
//! Run it natively; under qemu the simulated room falls behind real time,
//! the microphone lags and the measurement goes wrong.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::net::{Ipv4Addr, UdpSocket};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_atmos67");
/// The sender's channels: the rendered 7.1.4 mains, then two subwoofers.
const SENT: [&str; 13] = ["L", "R", "C", "Lb", "Rb", "Ls", "Rs", "Tfl", "Tfr", "Tbl", "Tbr", "SW1", "SW2"];
/// The designed correction's outputs, in SECS 2R's order.
const OUTPUTS: [&str; 13] = ["L", "R", "C", "Ls", "Rs", "Lb", "Rb", "Tfl", "Tfr", "Tbl", "Tbr", "SW1", "SW2"];
/// The rendered 7.1.4 channel of each main output.
const RENDERED: [usize; 11] = [0, 1, 2, 6, 7, 4, 5, 8, 9, 10, 11];

/// Arrival delay in samples and gain of each sent channel in the simulated room.
fn room(output: usize) -> (usize, f32) {
    (96 + 23 * output, 10f32.powf(-((output % 5) as f32) * 0.8 / 20.0))
}

fn atmos67(args: &[&str]) {
    let status = Command::new(BIN).args(args).status().unwrap();
    assert!(status.success(), "atmos67 {args:?}");
}

/// `O_NONBLOCK` on Linux, which the test does not take libc for.
const O_NONBLOCK: i32 = 0o4000;

/// Writes `bytes` to the FIFO while something reads it, keeping in
/// `backlog` what the pipe cannot take yet, so the microphone has no gaps;
/// with no reader they are dropped, as a microphone nobody records is lost.
/// Pieces of at most PIPE_BUF bytes are written whole or not at all, so
/// samples stay aligned.
fn feed(fifo: &mut Option<File>, backlog: &mut Vec<u8>, path: &Path, bytes: &[u8]) {
    if fifo.is_none() {
        backlog.clear();
        *fifo = OpenOptions::new().write(true).custom_flags(O_NONBLOCK).open(path).ok();
    }
    let Some(file) = fifo else { return };
    backlog.extend_from_slice(bytes);
    let mut written = 0;
    for piece in backlog.chunks(4096) {
        match file.write(piece) {
            Ok(_) => written += piece.len(),
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(_) => {
                *fifo = None;
                backlog.clear();
                return;
            }
        }
    }
    backlog.drain(..written);
}

/// Hears the sender's two streams (8 and 5 channels) and feeds the room's
/// response to the FIFO `mic` until `stop` is set.
fn simulate(first: Ipv4Addr, mic: &Path, stop: &AtomicBool) {
    let rtp = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 5004)).unwrap();
    for index in 0..2 {
        rtp.join_multicast_v4(&Ipv4Addr::from(u32::from(first) + index), &Ipv4Addr::UNSPECIFIED).unwrap();
    }
    rtp.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let (mut fifo, mut backlog) = (None, Vec::new());
    // Microphone samples from `base` on, still being added to.
    let (mut base, mut latest) = (None::<u64>, 0u64);
    let mut pending: VecDeque<f32> = VecDeque::new();
    let mut packet = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        let Ok((n, _)) = rtp.recv_from(&mut packet) else { continue };
        let (channels, offset) = match n - 12 {
            1152 => (8, 0),
            720 => (5, 8),
            _ => continue,
        };
        let timestamp = u64::from(u32::from_be_bytes(packet[4..8].try_into().unwrap()));
        let start = *base.get_or_insert(timestamp);
        latest = latest.max(timestamp);
        for (frame, samples) in packet[12..n].chunks_exact(channels * 3).enumerate() {
            for (channel, bytes) in samples.as_chunks::<3>().0.iter().enumerate() {
                let value = (i32::from_be_bytes([bytes[0], bytes[1], bytes[2], 0]) >> 8) as f32 / 8_388_607.0;
                let (delay, gain) = room(offset + channel);
                // A packet later than the margin is lost, as a dropout would be.
                let Some(index) = (timestamp + frame as u64 + delay as u64).checked_sub(start) else { continue };
                let index = index as usize;
                if pending.len() <= index {
                    pending.resize(index + 1, 0.0);
                }
                pending[index] += gain * value;
            }
        }
        // Samples a packet behind the latest are final: both streams of
        // a timestamp arrive together, and delays only push sound later.
        let ready = (latest.saturating_sub(start).saturating_sub(48) as usize).min(pending.len());
        let bytes: Vec<u8> = pending.drain(..ready).flat_map(f32::to_le_bytes).collect();
        feed(&mut fifo, &mut backlog, mic, &bytes);
        base = Some(start + ready as u64);
    }
}

#[test]
#[ignore = "measures in real time and designs with SECS 2R for minutes"]
fn calibrates_a_simulated_room() {
    let work = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/calibrate-e2e");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let (manual, project, design) = (work.join("manual"), work.join("project"), work.join("design"));
    let (socket, mic) = (work.join("control"), work.join("mic.fifo"));
    assert!(Command::new("mkfifo").arg(&mic).status().unwrap().success());
    atmos67(&["--layout", "7.1.4", "--write-design", manual.to_str().unwrap(), "--subs", "2"]);

    let first = Ipv4Addr::new(239, 69, 67, 241);
    let stop = Arc::new(AtomicBool::new(false));
    let simulation = {
        let (stop, mic) = (Arc::clone(&stop), mic.clone());
        std::thread::spawn(move || simulate(first, &mic, &stop))
    };
    let mut sender = Command::new(BIN)
        .args(["--layout", "7.1.4", "--aes67-send", "--aes67-address", &first.to_string()])
        .args(["--ptp-gmid", "00-11-22-FF-FE-33-44-55", "--dsp", manual.to_str().unwrap()])
        .args(["--control", socket.to_str().unwrap()])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(1));

    let capture = format!("cat {}", mic.display());
    let common = [
        "--project", project.to_str().unwrap(),
        "--control", socket.to_str().unwrap(),
        "--mic", "simulated",
        "--capture-command", &capture,
    ];
    atmos67(&[&["calibrate", "start", "--layout", "7.1.4", "--subs", "2"][..], &common].concat());
    atmos67(&[&["calibrate", "measure", "--position", "1"][..], &common].concat());

    // The room delays L by 96 samples (2 ms); the simulation hands the
    // microphone on a packet or so later than a real one would.
    let output = Command::new(BIN)
        .args(["calibrate", "latency", "--control", socket.to_str().unwrap(), "--mic", "simulated"])
        .args(["--capture-command", &capture, "--channel", "1"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let line = String::from_utf8_lossy(&output.stdout).into_owned();
    let ms: f64 = line.split('"').nth(7).and_then(|v| v.parse().ok()).unwrap_or_else(|| panic!("{line}"));
    eprintln!("latency {ms} ms");
    assert!((1.5..8.0).contains(&ms), "latency {ms} ms");
    stop.store(true, Ordering::Relaxed);
    drop(sender.stdin.take());
    sender.wait().unwrap();
    simulation.join().unwrap();
    atmos67(&[&["calibrate", "finish", "--out", design.to_str().unwrap()][..], &common].concat());

    // Each main's impulse response through the design, then the room.
    let length = 48_000;
    let mut arrivals = Vec::new();
    for (output, &rendered) in RENDERED.iter().enumerate() {
        let mut input = vec![0f32; length * 12];
        input[rendered] = 1.0;
        let mut child = Command::new(BIN)
            .args(["--layout", "7.1.4", "--dsp", design.to_str().unwrap(), "--dsp-only"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let bytes: Vec<u8> = input.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut stdin = child.stdin.take().unwrap();
        let writer = std::thread::spawn(move || stdin.write_all(&bytes).unwrap());
        let out = child.wait_with_output().unwrap().stdout;
        writer.join().unwrap();
        let samples: Vec<f32> = out.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        let response: Vec<f32> = samples.as_chunks::<{ OUTPUTS.len() }>().0.iter().map(|f| f[output]).collect();
        let (peak, _) = response.iter().enumerate().fold((0, 0f32), |m, (i, s)| if s.abs() > m.1 { (i, s.abs()) } else { m });
        let energy: f32 = response.iter().map(|s| s * s).sum();
        let (delay, gain) = room(SENT.iter().position(|r| *r == OUTPUTS[output]).unwrap());
        arrivals.push((peak + delay, 10.0 * energy.log10() + 20.0 * gain.log10()));
        eprintln!("{:>3}: peak at {peak}, arrival {}, level {:.2} dB", OUTPUTS[output], peak + delay, arrivals[output].1);
    }
    let (first_arrival, first_level) = arrivals[0];
    for (output, (arrival, level)) in arrivals.iter().enumerate() {
        assert!(arrival.abs_diff(first_arrival) <= 1, "{} arrives at {arrival}, L at {first_arrival}", OUTPUTS[output]);
        assert!((level - first_level).abs() < 0.5, "{} is {level:.2} dB, L {first_level:.2} dB", OUTPUTS[output]);
    }
}
