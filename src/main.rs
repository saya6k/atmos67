//! Reads HDMI capture (S16LE stereo PCM carrying IEC 61937 bursts) on stdin,
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

use iec61937::{DATA_TYPE_EAC3, Depacketizer};
use decode::joc::Joc;
use openjoc_api::OpenJocConfig;
use openjoc_scene::SpeakerLayout;

const USAGE: &str = "usage: atmos67 (--layout PRESET | --layout-file LAYOUT.json) < capture.s16le > speakers.f32le
       atmos67 (--layout PRESET | --layout-file LAYOUT.json) --aes67-sources > status.json";

/// HDMI carries E-AC-3 in a stereo 16-bit stream at four times the audio
/// rate, so one output frame corresponds to 4 input frames of 4 bytes.
const INPUT_BYTES_PER_OUTPUT_FRAME: u64 = 16;

/// How far output may trail the input clock before silence is inserted. Two
/// access units: steady decoding trails by about one and never triggers it.
const MAX_LAG_FRAMES: u64 = 2 * 1536;

/// Input without an E-AC-3 burst that counts as the stream having stopped:
/// one and a half burst periods (a burst repeats every 1,536 output frames).
const GAP_BYTES: u64 = 1536 * INPUT_BYTES_PER_OUTPUT_FRAME * 3 / 2;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let print_sources = args.last().is_some_and(|arg| arg == "--aes67-sources");
    if print_sources {
        args.pop();
    }
    let config = match args.as_slice() {
        [flag, name] if flag == "--layout" => OpenJocConfig {
            speaker_layout: name.clone(),
            ..OpenJocConfig::default()
        },
        [flag, path] if flag == "--layout-file" => {
            OpenJocConfig::default().with_speaker_layout(SpeakerLayout::from_json_file(path)?)
        }
        _ => return Err(USAGE.into()),
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
    let mut depacketizer = Depacketizer::default();
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
        depacketizer.feed(&chunk[..n]);
        while let Some((data_type, payload)) = depacketizer.next_burst() {
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
        if last_burst_at.is_some_and(|at| input_bytes - at > GAP_BYTES) {
            joc.flush(&mut decoded);
            last_burst_at = None;
        }
        output.write(&decoded)?;
        decoded.clear();
        // The sink downstream runs on the capture clock. Whenever no audio was
        // decoded (pause, PCM source, dropped bursts), fill the gap with silence.
        let due = (input_bytes / INPUT_BYTES_PER_OUTPUT_FRAME).saturating_sub(MAX_LAG_FRAMES);
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
