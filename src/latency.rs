//! `atmos67 calibrate latency`: how long sound takes from leaving the sender
//! to reaching the microphone, through the receivers, amplifiers and room.
//! Kodi's picture is held back by this plus what the sender reports itself.
//!
//! The sender answers `play` with the PTP time its first frame left at. The
//! microphone has no timestamps, so each read is stamped with the system
//! clock, which the sender's PTP time is read from: the earliest of "read
//! time minus the samples read so far" is when the first sample was taken,
//! give or take the microphone's own converter latency.

use std::error::Error;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use realfft::RealFftPlanner;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const RATE: f64 = 48_000.0;
/// PTP time is TAI, 37 s ahead of UTC.
const TAI_MINUS_UTC: f64 = 37.0;
const SWEEP_SECONDS: f64 = 0.5;
/// Recording kept after the sweep has left, for the sound to arrive.
const TAIL: Duration = Duration::from_millis(800);

/// An exponential sweep from 200 Hz to 8 kHz at -12 dBFS, faded in and out.
pub fn sweep() -> Vec<f32> {
    let n = (SWEEP_SECONDS * RATE) as usize;
    let (f0, f1) = (200f64, 8000f64);
    let k = (f1 / f0).ln();
    (0..n)
        .map(|i| {
            let t = i as f64 / RATE;
            let phase = 2.0 * std::f64::consts::PI * f0 * SWEEP_SECONDS / k * ((t / SWEEP_SECONDS * k).exp() - 1.0);
            let fade = (i.min(n - 1 - i) as f64 / 480.0).min(1.0);
            (0.25 * fade * phase.sin()) as f32
        })
        .collect()
}

/// The index in `recording` where `signal` best lines up.
pub fn arrival(recording: &[f32], signal: &[f32]) -> usize {
    let size = (recording.len() + signal.len()).next_power_of_two();
    let mut planner = RealFftPlanner::<f64>::new();
    let (forward, inverse) = (planner.plan_fft_forward(size), planner.plan_fft_inverse(size));
    let spectrum = |samples: &[f32]| {
        let mut time = vec![0.0; size];
        time.iter_mut().zip(samples).for_each(|(t, s)| *t = f64::from(*s));
        let mut spectrum = forward.make_output_vec();
        forward.process(&mut time, &mut spectrum).expect("sizes from the plan");
        spectrum
    };
    let (r, s) = (spectrum(recording), spectrum(signal));
    let mut product: Vec<_> = r.iter().zip(&s).map(|(r, s)| r * s.conj()).collect();
    product[0].im = 0.0;
    product[size / 2].im = 0.0;
    let mut correlation = vec![0.0; size];
    inverse.process(&mut product, &mut correlation).expect("sizes from the plan");
    correlation[..recording.len()]
        .iter()
        .enumerate()
        .fold((0, f64::MIN), |best, (i, c)| if *c > best.1 { (i, *c) } else { best })
        .0
}

/// PTP time in samples of the first sample read, from (system time of a
/// read, samples read up to and including it) pairs.
pub fn first_sample_time(reads: &[(f64, usize)]) -> f64 {
    reads.iter().map(|(seconds, total)| (seconds + TAI_MINUS_UTC) * RATE - *total as f64).fold(f64::MAX, f64::min)
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

pub(crate) fn ask(socket: &mut UnixStream, message: &[u8]) -> Result<String> {
    socket.write_all(message)?;
    let mut answer = String::new();
    BufReader::new(socket).read_line(&mut answer)?;
    let answer = answer.trim_end();
    Ok(answer.strip_prefix("ok").ok_or_else(|| format!("sender: {answer}"))?.trim().to_string())
}

/// Measures from output channel `channel` (0 first) of the sender at
/// `control`, recording with `capture` (a command printing mono FLOAT32LE),
/// and returns the latency in milliseconds.
pub fn measure(control: &Path, capture: &[String], channel: usize) -> Result<f64> {
    let channels: usize = ask(&mut UnixStream::connect(control)?, b"channels\n")?.parse()?;
    if channel >= channels {
        return Err(format!("the sender has {channels} channels").into());
    }
    let (program, args) = capture.split_first().ok_or("no capture command")?;
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let overrun = crate::measure::watch_overruns(&mut child);
    let mut stdout = child.stdout.take().expect("piped");
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let recorder = thread::spawn(move || {
        let (mut bytes, mut reads, mut chunk) = (Vec::new(), Vec::new(), [0u8; 4096]);
        while stop_rx.try_recv().is_err() {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    bytes.extend_from_slice(&chunk[..n]);
                    reads.push((now(), bytes.len() / 4));
                }
            }
        }
        (bytes, reads)
    });
    // Let the recording settle before the sweep.
    thread::sleep(Duration::from_millis(500));

    let signal = sweep();
    let mut message = format!("play {channels} {}\n", signal.len()).into_bytes();
    for sample in &signal {
        for c in 0..channels {
            message.extend(if c == channel { *sample } else { 0.0 }.to_le_bytes());
        }
    }
    let started: f64 = ask(&mut UnixStream::connect(control)?, &message)?.parse()?;
    thread::sleep(TAIL);
    let _ = stop_tx.send(());
    let _ = child.kill();
    let _ = child.wait();
    let (bytes, reads) = recorder.join().map_err(|_| "recorder failed")?;
    if overrun.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("the microphone's capture overran; measure again".into());
    }
    let recording: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    if recording.iter().all(|s| s.abs() < 1e-4) {
        return Err("the microphone heard nothing".into());
    }
    let arrived = first_sample_time(&reads) + arrival(&recording, &signal) as f64;
    Ok((arrived - started) / RATE * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_delayed_quiet_sweep_in_noise() {
        let signal = sweep();
        let mut recording: Vec<f32> = (0..60_000).map(|i| ((i * 7919 % 101) as f32 - 50.0) / 5000.0).collect();
        for (i, s) in signal.iter().enumerate() {
            recording[12_345 + i] += 0.1 * s;
        }
        assert_eq!(arrival(&recording, &signal), 12_345);
    }

    #[test]
    fn the_earliest_read_dates_the_first_sample() {
        // 480 samples a read, delivered 1 to 6 ms after they were taken.
        let start = 1_000.0;
        let reads: Vec<(f64, usize)> =
            (1..=50).map(|k| (start + k as f64 * 0.01 + 0.001 + (k % 6) as f64 * 0.001, k * 480)).collect();
        let first = first_sample_time(&reads);
        let want = (start + TAI_MINUS_UTC) * RATE;
        assert!((first - want - 48.0).abs() < 1.0, "{}", first - want);
    }
}
