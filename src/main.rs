//! Reads HDMI capture (S16LE PCM, stereo or 8-channel, carrying IEC 61937
//! bursts) on stdin,
//! decodes E-AC-3 / E-AC-3 JOC (Dolby Digital Plus Atmos) and renders it to the
//! given speaker layout, writing interleaved FLOAT32LE PCM on stdout.

mod aes67;
mod decode;
mod iec61937;
// Not wired in until the channel-based decoders arrive (T7).
#[allow(dead_code)]
mod layout;
#[allow(dead_code)]
mod render;

use std::error::Error;
use std::io::{self, Read, Write};

use iec61937::{Capture, DATA_TYPE_EAC3};
use decode::joc::Joc;
use openjoc_api::OpenJocConfig;
use openjoc_scene::SpeakerLayout;

const USAGE: &str = "usage: atmos67 (--layout PRESET | --layout-file LAYOUT.json) [--input-channels 2|8] [--input-rate HZ] < capture.s16le > speakers.f32le
       atmos67 (--layout PRESET | --layout-file LAYOUT.json) --aes67-sources > status.json";

const OUTPUT_RATE: u64 = 48_000;

/// How far output may trail the input clock before silence is inserted. Two
/// E-AC-3 access units: steady decoding trails by about one and never
/// triggers it.
const MAX_LAG_FRAMES: u64 = 2 * 1536;

/// Output frames without a decodable burst that count as the stream having
/// stopped: one and a half E-AC-3 burst periods.
const GAP_FRAMES: u64 = 1536 * 3 / 2;

fn main() -> Result<(), Box<dyn Error>> {
    let mut config = None;
    let mut input_channels = 2usize;
    // HDMI carries compressed audio at four times the 48 kHz audio rate.
    let mut input_rate = 192_000u32;
    let mut print_sources = false;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(USAGE);
        match flag.as_str() {
            "--layout" => {
                config = Some(OpenJocConfig {
                    speaker_layout: value()?,
                    ..OpenJocConfig::default()
                });
            }
            "--layout-file" => {
                let layout = SpeakerLayout::from_json_file(value()?)?;
                config = Some(OpenJocConfig::default().with_speaker_layout(layout));
            }
            "--input-channels" => input_channels = value()?.parse()?,
            "--input-rate" => input_rate = value()?.parse()?,
            "--aes67-sources" => print_sources = true,
            _ => return Err(USAGE.into()),
        }
    }
    let config = config.ok_or(USAGE)?;
    if !matches!(input_channels, 2 | 8) || input_rate == 0 {
        return Err(USAGE.into());
    }
    // Output frames that `bytes` of capture last for.
    let frames_in = |bytes: u64| {
        bytes / (input_channels as u64 * 2) * OUTPUT_RATE / u64::from(input_rate)
    };
    let mut joc = Joc::new(config)?;
    let info = joc.output_info();
    if print_sources {
        print!("{}", aes67::status_json(info.channel_count));
        return Ok(());
    }
    eprintln!(
        "atmos67: output {} channels [{}], FLOAT32LE",
        info.channel_count,
        info.channel_labels.join(" ")
    );

    let mut stdin = io::stdin().lock();
    let mut output = Output {
        out: io::stdout().lock(),
        channels: info.channel_count,
        frames: 0,
    };
    let mut capture = Capture::new(input_channels, input_rate);
    let mut chunk = [0u8; 16384];
    let mut decoded = Vec::new();
    let mut input_bytes = 0u64;
    let mut last_skipped = None;
    let mut last_error = None;
    let mut last_burst_at = None;
    loop {
        let n = stdin.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        input_bytes += n as u64;
        capture.feed(&chunk[..n]);
        while let Some((data_type, payload)) = capture.next_burst() {
            if data_type != DATA_TYPE_EAC3 {
                // Null/pause bursts (0, 3) are normal filler; report anything else once.
                if !matches!(data_type, 0 | 3) && last_skipped != Some(data_type) {
                    eprintln!("atmos67: skipping unsupported IEC 61937 data type {data_type:#04x}");
                    last_skipped = Some(data_type);
                }
                continue;
            }
            match joc.decode(payload, &mut decoded) {
                Ok(()) => last_burst_at = Some(input_bytes),
                // A damaged burst must not end a live stream: it is dropped.
                Err(message) => {
                    if last_error.as_ref() != Some(&message) {
                        eprintln!("atmos67: dropping access units: {message}");
                        last_error = Some(message);
                    }
                }
            }
        }
        if last_burst_at.is_some_and(|at| frames_in(input_bytes - at) > GAP_FRAMES) {
            joc.flush(&mut decoded);
            last_burst_at = None;
        }
        output.write(&decoded)?;
        decoded.clear();
        // The sink downstream runs on the capture clock. Whenever no audio was
        // decoded (pause, PCM source, dropped bursts), fill the gap with silence.
        let due = frames_in(input_bytes).saturating_sub(MAX_LAG_FRAMES);
        output.write_silence(due.saturating_sub(output.frames))?;
    }
    joc.flush(&mut decoded);
    output.write(&decoded)?;
    Ok(())
}

struct Output<W> {
    out: W,
    channels: usize,
    frames: u64,
}

impl<W: Write> Output<W> {
    fn write(&mut self, samples: &[f32]) -> io::Result<()> {
        if !samples.is_empty() {
            let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
            self.out.write_all(&bytes)?;
            self.out.flush()?;
            self.frames += (samples.len() / self.channels) as u64;
        }
        Ok(())
    }

    fn write_silence(&mut self, frames: u64) -> io::Result<()> {
        if frames > 0 {
            self.out.write_all(&vec![0u8; frames as usize * self.channels * 4])?;
            self.out.flush()?;
            self.frames += frames;
        }
        Ok(())
    }
}
