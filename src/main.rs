//! Reads HDMI capture (S16LE PCM, stereo or 8-channel, carrying IEC 61937
//! bursts) on stdin, decodes the audio and renders it to the given speaker
//! layout, writing interleaved FLOAT32LE PCM on stdout.

mod aes67;
mod decode;
mod dsp;
mod iec61937;
mod layout;
mod mat;
mod render;
mod send;

use std::error::Error;
use std::io::{self, Read, Write};

use decode::eac3::Eac3;
use decode::ffmpeg::{Ffmpeg, Id};
use decode::joc::Joc;
use decode::truehd::TrueHd;
use iec61937::{
    Capture, DATA_TYPE_AC3, DATA_TYPE_DTS, DATA_TYPE_DTS_HD, DATA_TYPE_EAC3, DATA_TYPE_MAT,
    dts_hd_frame,
};
use layout::Layout;
use render::Panner;
use openjoc_api::OpenJocConfig;
use openjoc_scene::SpeakerLayout;

const USAGE: &str = "usage: atmos67 (--layout PRESET | --layout-file LAYOUT.json) [--input-channels 2|8] [--input-rate HZ] < capture.s16le > speakers.f32le
       atmos67 (--layout PRESET | --layout-file LAYOUT.json) --aes67-sources > status.json
       atmos67 (--layout PRESET | --layout-file LAYOUT.json) --aes67-send --ptp-gmid ID [--ptp-domain N] [--aes67-address A.B.C.D] [--aes67-buffer-ms MS]
           [--dsp SECS_EXPORT_DIR] [--volume DB] [--control SOCKET] < speakers.f32le
       atmos67 (--layout PRESET | --layout-file LAYOUT.json) --write-design DIR [--base SECS_EXPORT_DIR]
           [--subs N] [--crossover front|center|surround|back|top=HZ]... [--slope 12|24]
           [--trim ROLE=DB]... [--delay ROLE=MS]...";

const OUTPUT_RATE: u64 = 48_000;

/// How far output may trail the input clock before silence is inserted. Two
/// E-AC-3 access units: steady decoding trails by about one and never
/// triggers it.
const MAX_LAG_FRAMES: u64 = 2 * 1536;

/// Output frames without a decodable burst that count as the stream having
/// stopped: one and a half E-AC-3 burst periods.
const GAP_FRAMES: u64 = 1536 * 3 / 2;

fn main() -> Result<(), Box<dyn Error>> {
    let mut speakers = None;
    let mut input_channels = 2usize;
    // HDMI carries compressed audio at four times the 48 kHz audio rate.
    let mut input_rate = 192_000u32;
    let mut print_sources = false;
    let mut send = false;
    let mut send_config = send::Config {
        address: std::net::Ipv4Addr::new(239, 69, 67, 1),
        gmid: String::new(),
        domain: 0,
        buffer_ms: 100,
        volume_db: 0.0,
        control: None,
    };
    let mut dsp = None;
    let mut write_design = None;
    let mut base = None;
    let mut manual = dsp::manual::Settings { order: 4, ..Default::default() };
    // `NAME=NUMBER`, as the --crossover, --trim and --delay flags take.
    let pair = |text: String| -> Result<(String, f64), Box<dyn Error>> {
        let (name, number) = text.split_once('=').ok_or(USAGE)?;
        Ok((name.to_string(), number.parse()?))
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(USAGE);
        match flag.as_str() {
            "--layout" => speakers = Some(SpeakerLayout::preset(&value()?)?),
            "--layout-file" => speakers = Some(SpeakerLayout::from_json_file(value()?)?),
            "--input-channels" => input_channels = value()?.parse()?,
            "--input-rate" => input_rate = value()?.parse()?,
            "--aes67-sources" => print_sources = true,
            "--aes67-send" => send = true,
            "--aes67-address" => send_config.address = value()?.parse()?,
            "--aes67-buffer-ms" => send_config.buffer_ms = value()?.parse()?,
            "--ptp-gmid" => send_config.gmid = value()?,
            "--ptp-domain" => send_config.domain = value()?.parse()?,
            "--dsp" => dsp = Some(std::path::PathBuf::from(value()?)),
            "--volume" => send_config.volume_db = value()?.parse()?,
            "--control" => send_config.control = Some(value()?.into()),
            "--write-design" => write_design = Some(std::path::PathBuf::from(value()?)),
            "--base" => base = Some(std::path::PathBuf::from(value()?)),
            "--subs" => manual.subs = value()?.parse()?,
            "--slope" => {
                manual.order = match value()?.as_str() {
                    "12" => 2,
                    "24" => 4,
                    _ => return Err(USAGE.into()),
                }
            }
            "--crossover" => {
                let (group, hz) = pair(value()?)?;
                manual.crossover.insert(group, hz);
            }
            "--trim" => {
                let (role, db) = pair(value()?)?;
                manual.trims.insert(role, db);
            }
            "--delay" => {
                let (role, ms) = pair(value()?)?;
                manual.delays.insert(role, ms);
            }
            _ => return Err(USAGE.into()),
        }
    }
    let speakers = speakers.ok_or(USAGE)?;
    if !matches!(input_channels, 2 | 8) || input_rate == 0 || send && send_config.gmid.is_empty() {
        return Err(USAGE.into());
    }
    let layout = Layout::from_openjoc(&speakers)?;
    let panner = Panner::new(&layout);
    let config = OpenJocConfig::default().with_speaker_layout(speakers);
    // Output frames that `bytes` of capture last for.
    let frames_in = |bytes: u64| {
        bytes / (input_channels as u64 * 2) * OUTPUT_RATE / u64::from(input_rate)
    };
    let joc = Joc::new(config)?;
    let info = joc.output_info();
    let mut eac3 = Eac3::new(joc);
    let mut ac3 = None;
    let mut dts = None;
    let mut dts_x_reported = false;
    let mut truehd = TrueHd::default();
    if print_sources {
        print!("{}", aes67::status_json(info.channel_count));
        return Ok(());
    }
    if let Some(folder) = write_design {
        dsp::manual::write(&folder, &info.channel_labels, base.as_deref(), &manual)?;
        return Ok(());
    }
    if send {
        let design = match dsp {
            Some(folder) => Some(
                dsp::design::Design::load(&folder, &info.channel_labels)
                    .map_err(|e| format!("{}: {e}", folder.display()))?,
            ),
            None => None,
        };
        if let Some(design) = &design {
            eprintln!("atmos67: correcting with {} channels out [{}]", design.outputs.len(), design.outputs.join(" "));
        }
        return send::run(info.channel_count, &send_config, design);
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
            let result = match data_type {
                DATA_TYPE_EAC3 => eac3.decode(payload, &layout, &panner, &mut decoded),
                DATA_TYPE_AC3 => {
                    libav(&mut ac3, Id::AC3)?.decode(payload, &layout, &panner, &mut decoded)
                }
                DATA_TYPE_MAT => truehd.decode(payload, &layout, &panner, &mut decoded),
                DATA_TYPE_DTS_HD => match dts_hd_frame(payload) {
                    Some(frame) => {
                        libav(&mut dts, Id::DTS)?.decode(frame, &layout, &panner, &mut decoded)
                    }
                    None => Err("malformed DTS-HD burst".to_owned()),
                },
                _ if DATA_TYPE_DTS.contains(&data_type) => {
                    libav(&mut dts, Id::DTS)?.decode(payload, &layout, &panner, &mut decoded)
                }
                _ => {
                    // Null/pause bursts (0, 3) are normal filler; report anything else once.
                    if !matches!(data_type, 0 | 3) && last_skipped != Some(data_type) {
                        eprintln!(
                            "atmos67: skipping unsupported IEC 61937 data type {data_type:#04x}"
                        );
                        last_skipped = Some(data_type);
                    }
                    continue;
                }
            };
            if !dts_x_reported && dts.as_ref().is_some_and(Ffmpeg::is_dts_x) {
                eprintln!("atmos67: DTS:X objects are not rendered; playing the channel bed");
                dts_x_reported = true;
            }
            match result {
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
            eac3.flush(&mut decoded);
            for decoder in [&mut ac3, &mut dts].into_iter().flatten() {
                decoder.reset();
            }
            truehd.reset();
            last_burst_at = None;
        }
        output.write(&decoded)?;
        decoded.clear();
        // The sink downstream runs on the capture clock. Whenever no audio was
        // decoded (pause, PCM source, dropped bursts), fill the gap with silence.
        let due = frames_in(input_bytes).saturating_sub(MAX_LAG_FRAMES);
        output.write_silence(due.saturating_sub(output.frames))?;
    }
    eac3.flush(&mut decoded);
    output.write(&decoded)?;
    Ok(())
}

/// The libavcodec decoder in `slot`, created on first use.
fn libav(slot: &mut Option<Ffmpeg>, id: Id) -> Result<&mut Ffmpeg, String> {
    if slot.is_none() {
        *slot = Some(Ffmpeg::new(id)?);
    }
    Ok(slot.as_mut().expect("created above"))
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
