//! Audio devices for SECS 2R's native measurement (tasks/dsp.md D7).
//!
//! The output is the running AES67 sender: a session hands the whole sweep
//! of a measuring position to it over its control socket (`play`), which
//! sends it contiguously, so every speaker of the position is timed on one
//! clock. The input is a USB microphone read through `arecord`, converted to
//! FLOAT32LE by ALSA's plug layer; a session starts recording only once the
//! first frames arrive, so the sweep's start is never missed. When arecord
//! reports an overrun, samples are missing and every later sweep would seem
//! to arrive early, so the recording is rejected as having a gap.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use secs_audio::{
    AudioBackend, AudioError, CancelToken, CaptureRead, Direction, Endpoint, InputSession, OutputSession,
    PlaybackReport, ProbeResult, RecordingIssue, ShareMode, StreamSpec,
};

const RATE: u32 = 48_000;
pub const OUTPUT_ID: &str = "atmos67";

#[derive(Clone)]
pub struct Backend {
    /// The sender's control socket.
    pub control: PathBuf,
    /// Channels the sender sends.
    pub outputs: u16,
    /// ALSA capture device of the microphone, such as `plughw:1,0`.
    pub microphone: String,
    pub microphone_channels: u16,
    /// Replaces `arecord` when set, for tests: a command whose standard
    /// output is the microphone as interleaved FLOAT32LE at 48 kHz.
    pub capture_command: Option<Vec<String>>,
}

fn backend_error(error: impl std::fmt::Display) -> AudioError {
    AudioError::Backend(error.to_string())
}

impl AudioBackend for Backend {
    fn name(&self) -> &'static str {
        "atmos67"
    }

    fn enumerate(&self) -> Result<Vec<Endpoint>, AudioError> {
        Ok(vec![
            Endpoint {
                id: OUTPUT_ID.into(),
                name: "atmos67 (AES67)".into(),
                host_api: "aes67".into(),
                max_input_channels: 0,
                max_output_channels: self.outputs,
                default_samplerate: f64::from(RATE),
                is_default_input: false,
                is_default_output: true,
            },
            Endpoint {
                id: self.microphone.clone(),
                name: format!("microphone ({})", self.microphone),
                host_api: "alsa".into(),
                max_input_channels: self.microphone_channels,
                max_output_channels: 0,
                default_samplerate: f64::from(RATE),
                is_default_input: true,
                is_default_output: false,
            },
        ])
    }

    fn probe(&self, endpoint: &Endpoint, direction: Direction, spec: StreamSpec, mode: ShareMode) -> Result<ProbeResult, AudioError> {
        let limit = match direction {
            Direction::Output => self.outputs,
            Direction::Input => self.microphone_channels,
        };
        let supported = spec.sample_rate == RATE && spec.channels <= limit && self.owns(endpoint, direction);
        Ok(ProbeResult {
            supported,
            mode,
            native_sample_rate: RATE,
            native_channels: spec.channels,
            detail: if supported { String::new() } else { format!("{} takes up to {limit} channels at 48 kHz", endpoint.id) },
        })
    }

    fn open_output(&self, endpoint: &Endpoint, spec: StreamSpec, mode: ShareMode) -> Result<Box<dyn OutputSession>, AudioError> {
        self.check(endpoint, Direction::Output, spec)?;
        Ok(Box::new(Output { control: self.control.clone(), channels: spec.channels, mode }))
    }

    fn open_input(&self, endpoint: &Endpoint, spec: StreamSpec, _mode: ShareMode) -> Result<Box<dyn InputSession>, AudioError> {
        self.check(endpoint, Direction::Input, spec)?;
        let command = self.capture_command.clone().unwrap_or_else(|| {
            ["arecord", "-q", "-D", &self.microphone, "-t", "raw", "-f", "FLOAT_LE", "-r", "48000", "-c"]
                .iter()
                .map(|s| s.to_string())
                .chain([spec.channels.to_string()])
                .collect()
        });
        Ok(Box::new(Input {
            command,
            channels: usize::from(spec.channels),
            child: None,
            stdout: None,
            overrun: Arc::default(),
            pending: Vec::new(),
        }))
    }
}

impl Backend {
    fn owns(&self, endpoint: &Endpoint, direction: Direction) -> bool {
        match direction {
            Direction::Output => endpoint.id == OUTPUT_ID,
            Direction::Input => endpoint.id == self.microphone,
        }
    }

    fn check(&self, endpoint: &Endpoint, direction: Direction, spec: StreamSpec) -> Result<(), AudioError> {
        let probe = self.probe(endpoint, direction, spec, ShareMode::SharedAutoConvert)?;
        if probe.supported { Ok(()) } else { Err(AudioError::UnsupportedFormat(probe.detail)) }
    }
}

/// Passes on what `child` writes to its standard error, which must be
/// piped, and raises the returned flag when arecord reports an overrun.
pub fn watch_overruns(child: &mut Child) -> Arc<AtomicBool> {
    let overrun = Arc::new(AtomicBool::new(false));
    if let Some(stderr) = child.stderr.take() {
        let overrun = Arc::clone(&overrun);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if line.contains("overrun") {
                    overrun.store(true, Ordering::Relaxed);
                }
                eprintln!("{line}");
            }
        });
    }
    overrun
}

fn gap() -> AudioError {
    AudioError::Recording { issue: RecordingIssue::Gap, detail: "the microphone's capture overran".into() }
}

struct Output {
    control: PathBuf,
    channels: u16,
    mode: ShareMode,
}

impl OutputSession for Output {
    fn play_to_completion(&mut self, interleaved: &[f32], cancel: &CancelToken) -> Result<PlaybackReport, AudioError> {
        if cancel.is_cancelled() {
            return Err(AudioError::Cancelled);
        }
        let frames = interleaved.len() / usize::from(self.channels);
        let mut socket = UnixStream::connect(&self.control)
            .map_err(|e| backend_error(format!("sender control {}: {e}", self.control.display())))?;
        let mut message = format!("play {} {frames}\n", self.channels).into_bytes();
        message.extend(interleaved[..frames * usize::from(self.channels)].iter().flat_map(|s| s.to_le_bytes()));
        socket.write_all(&message).map_err(backend_error)?;
        let mut reply = String::new();
        BufReader::new(socket).read_line(&mut reply).map_err(backend_error)?;
        if !reply.starts_with("ok") {
            return Err(backend_error(format!("sender: {}", reply.trim_end())));
        }
        Ok(PlaybackReport {
            frames_submitted: frames as u64,
            frames_drained: frames as u64,
            mode: self.mode,
            underruns: 0,
            cancelled: false,
        })
    }
}

struct Input {
    command: Vec<String>,
    channels: usize,
    child: Option<Child>,
    stdout: Option<ChildStdout>,
    overrun: Arc<AtomicBool>,
    /// Bytes read but not yet handed out.
    pending: Vec<u8>,
}

impl InputSession for Input {
    fn start(&mut self) -> Result<(), AudioError> {
        let (program, args) = self.command.split_first().ok_or_else(|| backend_error("no capture command"))?;
        let mut child = Command::new(program)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| backend_error(format!("{program}: {e}")))?;
        self.overrun = watch_overruns(&mut child);
        let mut stdout = child.stdout.take().expect("piped");
        // Recording counts as started once the device delivers.
        let mut first = vec![0u8; 4 * self.channels];
        if let Err(e) = stdout.read_exact(&mut first) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(backend_error(format!("{program} gave no audio: {e}")));
        }
        self.pending = first;
        self.child = Some(child);
        self.stdout = Some(stdout);
        Ok(())
    }

    fn read_into(&mut self, dst: &mut [f32], cancel: &CancelToken) -> Result<CaptureRead, AudioError> {
        if cancel.is_cancelled() {
            return Err(AudioError::Cancelled);
        }
        if self.overrun.load(Ordering::Relaxed) {
            return Err(gap());
        }
        let stdout = self.stdout.as_mut().ok_or_else(|| backend_error("capture not started"))?;
        let frame = 4 * self.channels;
        let want = dst.len() / self.channels * frame;
        while self.pending.len() < frame.min(want) {
            let mut chunk = vec![0u8; want - self.pending.len()];
            let n = stdout.read(&mut chunk).map_err(backend_error)?;
            if n == 0 {
                return Err(backend_error("the microphone stopped"));
            }
            self.pending.extend_from_slice(&chunk[..n]);
        }
        let frames = (self.pending.len() / frame).min(dst.len() / self.channels);
        for (sample, bytes) in dst.iter_mut().zip(self.pending[..frames * frame].as_chunks::<4>().0) {
            *sample = f32::from_le_bytes(*bytes);
        }
        self.pending.drain(..frames * frame);
        Ok(CaptureRead { frames, discontinuity: false, silent: false })
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        self.stdout = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Reported once the recording had been read through is a gap too.
        if self.overrun.load(Ordering::Relaxed) {
            return Err(gap());
        }
        Ok(())
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn backend(control: PathBuf, capture: Vec<String>) -> Backend {
        Backend { control, outputs: 3, microphone: "plughw:1,0".into(), microphone_channels: 1, capture_command: Some(capture) }
    }

    fn endpoint(backend: &Backend, id: &str) -> Endpoint {
        backend.enumerate().unwrap().into_iter().find(|e| e.id == id).unwrap()
    }

    #[test]
    fn plays_through_the_senders_socket() {
        let path = std::env::temp_dir().join(format!("atmos67-measure-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            let mut data = vec![0u8; 2 * 3 * 4];
            reader.read_exact(&mut data).unwrap();
            (&socket).write_all(b"ok\n").unwrap();
            (header, data)
        });
        let backend = backend(path.clone(), Vec::new());
        let spec = StreamSpec { sample_rate: 48_000, channels: 3 };
        let mut output = backend.open_output(&endpoint(&backend, OUTPUT_ID), spec, ShareMode::SharedAutoConvert).unwrap();
        let report = output.play_to_completion(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], &CancelToken::new()).unwrap();
        assert_eq!(report.frames_drained, 2);
        let (header, data) = server.join().unwrap();
        assert_eq!(header, "play 3 2\n");
        assert_eq!(data[20..], 6f32.to_le_bytes());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn records_the_capture_commands_output_in_order() {
        let file = std::env::temp_dir().join(format!("atmos67-mic-{}", std::process::id()));
        let samples: Vec<f32> = (0..5000).map(|n| n as f32).collect();
        std::fs::write(&file, samples.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<_>>()).unwrap();
        let backend = backend(PathBuf::new(), vec!["cat".into(), file.display().to_string()]);
        let spec = StreamSpec { sample_rate: 48_000, channels: 1 };
        let mut input = backend.open_input(&endpoint(&backend, "plughw:1,0"), spec, ShareMode::SharedAutoConvert).unwrap();
        input.start().unwrap();
        let mut got = Vec::new();
        let mut buffer = [0f32; 1024];
        while got.len() < samples.len() {
            let read = input.read_into(&mut buffer, &CancelToken::new()).unwrap();
            got.extend_from_slice(&buffer[..read.frames]);
        }
        input.stop().unwrap();
        assert_eq!(got, samples);
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn rejects_a_recording_arecord_reports_an_overrun_in() {
        let file = std::env::temp_dir().join(format!("atmos67-overrun-{}", std::process::id()));
        std::fs::write(&file, vec![0u8; 4 * 4800]).unwrap();
        let script = format!("cat {}; echo 'overrun!!! (at least 2.5 ms long)' >&2; sleep 1", file.display());
        let backend = backend(PathBuf::new(), vec!["sh".into(), "-c".into(), script]);
        let spec = StreamSpec { sample_rate: 48_000, channels: 1 };
        let mut input = backend.open_input(&endpoint(&backend, "plughw:1,0"), spec, ShareMode::SharedAutoConvert).unwrap();
        input.start().unwrap();
        let mut buffer = [0f32; 1024];
        let mut read = 0;
        while read < 4800 {
            match input.read_into(&mut buffer, &CancelToken::new()) {
                Ok(r) => read += r.frames,
                Err(_) => break,
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        let failed = input.stop();
        assert!(matches!(failed, Err(AudioError::Recording { issue: RecordingIssue::Gap, .. })), "{failed:?}");
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn refuses_other_rates_and_too_many_channels() {
        let backend = backend(PathBuf::new(), Vec::new());
        let out = endpoint(&backend, OUTPUT_ID);
        let spec = |sample_rate, channels| StreamSpec { sample_rate, channels };
        assert!(backend.open_output(&out, spec(44_100, 2), ShareMode::SharedAutoConvert).is_err());
        assert!(backend.open_output(&out, spec(48_000, 4), ShareMode::SharedAutoConvert).is_err());
        assert!(backend.open_output(&out, spec(48_000, 3), ShareMode::SharedAutoConvert).is_ok());
    }
}
