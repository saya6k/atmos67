//! Reads HDMI capture (S16LE PCM carrying IEC 61937 bursts) on stdin, decodes
//! E-AC-3 / E-AC-3 JOC (Dolby Digital Plus Atmos) and renders it to the given
//! speaker layout, writing interleaved FLOAT32LE PCM on stdout.

mod iec61937;

use std::error::Error;
use std::io::{self, Read, Write};

use iec61937::{DATA_TYPE_EAC3, Depacketizer};
use openjoc_api::{OpenJocConfig, OpenJocPacket, OpenJocSession, OpenJocStatus};
use openjoc_scene::SpeakerLayout;

const USAGE: &str = "usage: atmos67 (--layout PRESET | --layout-file LAYOUT.json) < capture.s16le > speakers.f32le";

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
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
    eprintln!(
        "atmos67: output {} channels [{}], FLOAT32LE",
        info.channel_count,
        info.channel_labels.join(" ")
    );

    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut depacketizer = Depacketizer::default();
    let mut chunk = [0u8; 16384];
    let mut last_skipped = None;
    loop {
        let n = stdin.read(&mut chunk)?;
        if n == 0 {
            break;
        }
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
                write_frames(&mut session, &mut stdout)?;
                match session.push_packet(packet) {
                    Ok(OpenJocStatus::OutputPending) => {}
                    Ok(_) => break,
                    Err(error) => {
                        // A damaged burst must not end a live stream: drop it and resync.
                        eprintln!("atmos67: dropped access unit: {error}");
                        session.try_reset()?;
                        break;
                    }
                }
            }
            write_frames(&mut session, &mut stdout)?;
        }
    }
    session.drain()?;
    write_frames(&mut session, &mut stdout)?;
    Ok(())
}

fn write_frames(session: &mut OpenJocSession, out: &mut impl Write) -> io::Result<()> {
    while let Some(frame) = session.receive_frame() {
        let bytes: Vec<u8> = frame
            .interleaved_f32
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        out.write_all(&bytes)?;
        out.flush()?;
    }
    Ok(())
}
