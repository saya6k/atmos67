//! Reads HDMI capture (S16LE stereo PCM carrying IEC 61937 bursts) on stdin,
//! decodes E-AC-3 / E-AC-3 JOC (Dolby Digital Plus Atmos) and renders it to the
//! given speaker layout, writing interleaved FLOAT32LE PCM on stdout.

mod aes67;
mod iec61937;

use std::error::Error;
use std::io::{self, Read, Write};

use iec61937::{DATA_TYPE_EAC3, Depacketizer};
use openjoc_api::{OpenJocConfig, OpenJocPacket, OpenJocSession, OpenJocStatus};
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
    let mut session = OpenJocSession::new(config)?;
    let info = session.output_info();
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
    let mut input_bytes = 0u64;
    let mut last_skipped = None;
    let mut decoding = false;
    let mut last_burst_at = 0u64;
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
            let packet = OpenJocPacket {
                data: payload,
                pts_samples: None,
                discontinuity: false,
                preroll: false,
            };
            loop {
                output.write_decoded(&mut session)?;
                match session.push_packet(packet) {
                    Ok(OpenJocStatus::OutputPending) => {}
                    Ok(_) => {
                        decoding = true;
                        last_burst_at = input_bytes;
                        break;
                    }
                    Err(error) => {
                        // A damaged burst must not end a live stream: drop it and resync.
                        eprintln!("atmos67: dropped access unit: {error}");
                        session.try_reset()?;
                        break;
                    }
                }
            }
            output.write_decoded(&mut session)?;
        }
        if decoding && input_bytes - last_burst_at > GAP_BYTES {
            // Bursts stopped. The decoder holds back the last access unit;
            // play it out now rather than after the stream resumes.
            session.drain()?;
            output.write_decoded(&mut session)?;
            session.try_reset()?;
            decoding = false;
        }
        // The sink downstream runs on the capture clock. Whenever no audio was
        // decoded (pause, PCM source, dropped bursts), fill the gap with silence.
        let due = (input_bytes / INPUT_BYTES_PER_OUTPUT_FRAME).saturating_sub(MAX_LAG_FRAMES);
        output.write_silence(due.saturating_sub(output.frames))?;
    }
    session.drain()?;
    output.write_decoded(&mut session)?;
    Ok(())
}

struct Output<W> {
    out: W,
    channels: usize,
    frames: u64,
}

impl<W: Write> Output<W> {
    fn write_decoded(&mut self, session: &mut OpenJocSession) -> io::Result<()> {
        while let Some(frame) = session.receive_frame() {
            let bytes: Vec<u8> = frame
                .interleaved_f32
                .iter()
                .flat_map(|sample| sample.to_le_bytes())
                .collect();
            self.out.write_all(&bytes)?;
            self.out.flush()?;
            self.frames += frame.sample_count as u64;
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
