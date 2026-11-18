//! AES67 sender for systems that cannot load the RAVENNA ALSA driver. Reads
//! interleaved FLOAT32LE speaker PCM at 48 kHz on stdin and sends it as L24
//! RTP streams of up to 8 channels in 1 ms packets, each announced by SAP.
//!
//! RTP timestamps count samples of PTP time (`a=mediaclk:direct=0`). PTP time
//! is taken from the system clock, so the system clock has to be the one PTP
//! synchronises: either disciplined by the grandmaster (`ptp4l -S`, `phc2sys`)
//! or itself the grandmaster's time source.
//!
//! The streams never stop: while stdin has nothing to play they carry
//! silence, so receivers stay locked between titles.
//!
//! The sender ends when stdin does: it plays out what it holds, withdraws
//! its streams with SAP deletions and exits. The service stops it that
//! way, by ending its input, rather than by a signal.
//!
//! A SECS 2R design, when given, corrects the speakers before they are sent
//! and may add subwoofer channels. The volume is applied to each packet as it
//! leaves, ramped across the packet, so a change is heard at once without a
//! click; a Unix socket takes `volume <dB>` and `mute 0|1` lines.
//!
//! Without a stream plan the channels go out in order, 8 to a stream, on
//! consecutive addresses. A plan (`--streams`, JSON) names each stream, its
//! address and the output channels it carries, by speaker name in either
//! spelling (`L` or `FL`); a name the outputs lack is reported and left out,
//! since the outputs change with the correction mode.
//!
//! For measurement the socket also takes `play <channels> <frames>` followed
//! by that many interleaved FLOAT32LE frames of the output channels. They are
//! sent from the next packet on, contiguously and as they are, in place of
//! the program, and `ok <sample>` answers once the last of them has left,
//! with the PTP time (in samples) the first one was sent at.
//!
//! `status` answers `ok` and one line of JSON: the PTP grandmaster, the
//! streams, whether the program is playing, send errors, the volume and the
//! latency. `channels` answers `ok <count>` of the channels sent, and `latency`
//! answers `ok <ms>`: how long the sender holds the program back,
//! its playout buffer and the correction's block and filters together.

use std::collections::VecDeque;
use std::error::Error;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::aes67::CHANNELS_PER_STREAM;
use crate::dsp::design::{Design, speaker};
use crate::layout::canonical;

const RATE: u64 = 48_000;
/// 1 ms, the packet time every AES67 receiver has to accept.
const PACKET_FRAMES: usize = 48;
const RTP_PORT: u16 = 5004;
const PAYLOAD_TYPE: u8 = 98;
const TTL: u32 = 15;
const SAP_ADDRESS: (Ipv4Addr, u16) = (Ipv4Addr::new(239, 255, 255, 255), 9875);
const SAP_INTERVAL: Duration = Duration::from_secs(30);
/// PTP time is TAI, which has been this many seconds ahead of UTC since 2017.
const TAI_MINUS_UTC: Duration = Duration::from_secs(37);
/// Packets the reader may queue beyond the playout buffer before it blocks.
const QUEUE_SLACK: usize = 100;
/// Packets over a second (1000 of them) the queue never drained below.
const TRIM_WINDOW: u32 = 1000;
/// How far above the playout buffer the queue may stay for a whole window
/// before the excess is dropped, so the delay stays what `latency` reports.
const TRIM_MARGIN: usize = 10;
/// The largest `play`: ten minutes of audio. SECS 2R plays a whole
/// position's sweeps, every channel in turn, as one signal of minutes.
const MAX_PLAY_FRAMES: usize = 600 * RATE as usize;
/// A send time this far from the clock means the clock was stepped or the
/// process was stalled; the timeline restarts from the clock.
const MAX_CLOCK_ERROR: u64 = RATE / 10;

pub struct Config {
    /// Multicast address of the first stream; later streams count up from it.
    pub address: Ipv4Addr,
    /// Grandmaster identity as SDP writes it, `00-11-22-FF-FE-33-44-55`.
    pub gmid: String,
    pub domain: u8,
    /// Audio held back before playing, to ride out the decoder's burstiness.
    pub buffer_ms: usize,
    /// Starting volume in dB.
    pub volume_db: f64,
    /// Where to listen for volume commands.
    pub control: Option<PathBuf>,
    /// Which channels go in which stream; 8 at a time in order without one.
    pub streams: Option<PathBuf>,
}

struct Stream {
    socket: UdpSocket,
    destination: SocketAddrV4,
    channels: Vec<usize>,
    ssrc: u32,
    sequence: u16,
    sdp: String,
}

/// Packets of `PACKET_FRAMES` interleaved frames waiting to be sent.
struct Queue {
    packets: VecDeque<Vec<f32>>,
    /// stdin reached its end.
    closed: bool,
}

/// What `status` reports: fixed facts, and what the send loop updates.
struct Status {
    info: serde_json::Value,
    playing: AtomicBool,
    send_errors: AtomicU64,
}

/// A measurement signal waiting to be sent in place of the program.
#[derive(Default)]
struct Measurement {
    packets: VecDeque<Vec<f32>>,
    /// PTP time, in samples, the first packet was sent at.
    started: Option<u64>,
    /// Told the start when the last packet has been sent, or why the
    /// signal did not go out whole.
    done: Option<mpsc::Sender<Result<u64, String>>>,
    /// The timeline restarted while the signal was going out.
    broken: bool,
}

/// A stream to send: its name, multicast address and output channels.
type Planned = (String, Ipv4Addr, Vec<usize>);

/// The streams for outputs called `names`: from the plan in `file`, or 8
/// channels at a time on addresses counting up from `first`.
fn plan(names: &[String], first: Ipv4Addr, file: Option<&Path>) -> Result<Vec<Planned>, Box<dyn Error>> {
    let Some(file) = file else {
        let all: Vec<usize> = (0..names.len()).collect();
        return Ok(all
            .chunks(CHANNELS_PER_STREAM)
            .enumerate()
            .map(|(i, map)| {
                let name = format!("atmos67 ch {}-{}", map[0] + 1, map[map.len() - 1] + 1);
                (name, Ipv4Addr::from(u32::from(first) + i as u32), map.to_vec())
            })
            .collect());
    };
    let same = |a: &str, b: &str| canonical(speaker(a)) == canonical(speaker(b));
    let entries: Vec<serde_json::Value> = serde_json::from_slice(&std::fs::read(file)?)?;
    let mut planned = Vec::new();
    for entry in &entries {
        let name = entry["name"].as_str().ok_or("a stream without a name")?.to_string();
        let address: Ipv4Addr = entry["address"].as_str().ok_or_else(|| format!("{name}: no address"))?.parse()?;
        let wanted = entry["channels"].as_array().ok_or_else(|| format!("{name}: no channels"))?;
        if wanted.len() > CHANNELS_PER_STREAM {
            return Err(format!("{name}: {} channels; a stream carries at most {CHANNELS_PER_STREAM}", wanted.len()).into());
        }
        let mut channels = Vec::new();
        for channel in wanted {
            let channel = channel.as_str().ok_or_else(|| format!("{name}: a channel is not a name"))?;
            match names.iter().position(|n| same(n, channel)) {
                Some(index) => channels.push(index),
                None => eprintln!("atmos67: stream {name}: no output {channel} (outputs: {}); left out", names.join(" ")),
            }
        }
        if channels.is_empty() {
            eprintln!("atmos67: stream {name} has no channel to send; not sent");
        } else {
            planned.push((name, address, channels));
        }
    }
    Ok(planned)
}

/// Sends `channels_in` rendered channels, through `design` when there is one.
/// `names` names the channels sent: the design's outputs, or the rendered ones.
pub fn run(channels_in: usize, names: &[String], config: &Config, design: Option<Design>) -> Result<(), Box<dyn Error>> {
    let channels = design.as_ref().map_or(channels_in, |d| d.outputs.len());
    let gain = Arc::new(AtomicU32::new(Volume { db: config.volume_db, muted: false }.gain().to_bits()));
    let measurement = Arc::new(Mutex::new(Measurement::default()));
    let latency_ms = config.buffer_ms as f64 + design.as_ref().map_or(0.0, |d| d.latency_ms);

    // The address the route to the streams' destination sends from. Binding
    // to it also makes multicast leave through that interface.
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    probe.connect((config.address, RTP_PORT))?;
    let source = match probe.local_addr()?.ip() {
        std::net::IpAddr::V4(ip) => ip,
        std::net::IpAddr::V6(_) => return Err("no IPv4 source address".into()),
    };
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64
        ^ u64::from(std::process::id()) << 32;

    let mut streams = Vec::new();
    let mut described = Vec::new();
    for (index, (name, address, map)) in plan(names, config.address, config.streams.as_deref())?.into_iter().enumerate() {
        if !address.is_multicast() {
            return Err(format!("{address} is not a multicast address").into());
        }
        let socket = UdpSocket::bind((source, 0))?;
        socket.set_multicast_ttl_v4(TTL)?;
        let session = fnv(name.bytes().chain(address.octets()));
        let carried: Vec<&str> = map.iter().map(|&c| names.get(c).map_or("?", String::as_str)).collect();
        eprintln!("atmos67: sending {name} [{}] to {address}:{RTP_PORT} from {source}", carried.join(" "));
        described.push(serde_json::json!({
            "name": name,
            "address": address.to_string(),
            "port": RTP_PORT,
            // The SDP origin's session ID, which identifies the stream to receivers.
            "session": session.to_string(),
            "channels": carried,
        }));
        streams.push(Stream {
            socket,
            destination: SocketAddrV4::new(address, RTP_PORT),
            channels: map.clone(),
            ssrc: (nonce >> 16) as u32 ^ index as u32,
            sequence: 0,
            sdp: sdp(&name, session, source, address, map.len(), config),
        });
    }

    let status = Arc::new(Status {
        info: serde_json::json!({
            "grandmaster": config.gmid,
            "domain": config.domain,
            "source": source.to_string(),
            "outputs": names,
            "streams": described,
            "latency_ms": latency_ms,
        }),
        playing: AtomicBool::new(false),
        send_errors: AtomicU64::new(0),
    });
    if let Some(path) = &config.control {
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let controls = Arc::new(Controls {
            volume: Mutex::new(Volume { db: config.volume_db, muted: false }),
            gain: Arc::clone(&gain),
            measurement: Arc::clone(&measurement),
            channels,
            status: Arc::clone(&status),
        });
        thread::spawn(move || control(&listener, &controls));
    }

    let announcer = UdpSocket::bind((source, 0))?;
    announcer.set_multicast_ttl_v4(TTL)?;
    // The SAP message hash changes exactly when the description does.
    let announcements: Vec<(u16, String)> = streams
        .iter()
        .map(|stream| (fnv(stream.sdp.bytes()) as u16, stream.sdp.clone()))
        .collect();
    {
        let announcer = announcer.try_clone()?;
        let announcements = announcements.clone();
        thread::spawn(move || {
            loop {
                for (hash, sdp) in &announcements {
                    // Announcements are best effort; RTP reports a dead network.
                    let _ = announcer.send_to(&sap(false, *hash, source, sdp), SAP_ADDRESS);
                }
                thread::sleep(SAP_INTERVAL);
            }
        });
    }

    let buffer = config.buffer_ms * PACKET_FRAMES * 1000 / RATE as usize;
    let queue = Arc::new((
        Mutex::new(Queue { packets: VecDeque::new(), closed: false }),
        Condvar::new(),
    ));
    {
        let queue = Arc::clone(&queue);
        thread::spawn(move || read_packets(channels_in, design, buffer + QUEUE_SLACK, &queue));
    }

    let silence = vec![0f32; PACKET_FRAMES * channels];
    let mut current = 1f32;
    let mut scaled = Vec::new();
    let mut playing = false;
    let mut send_failed = false;
    // The least the queue held while playing in the current window.
    let (mut lowest, mut window) = (usize::MAX, 0u32);
    let mut next = ptp_samples(SystemTime::now());
    loop {
        let now = ptp_samples(SystemTime::now());
        if now.abs_diff(next) > MAX_CLOCK_ERROR {
            let ms = (now as f64 - next as f64) * 1000.0 / RATE as f64;
            eprintln!("atmos67: the timeline was {ms:.0} ms off the clock; restarted from it");
            // A measurement signal with a gap in it measures the gap.
            let mut measurement = measurement.lock().expect("control does not panic");
            if measurement.started.is_some() {
                measurement.broken = true;
            }
            next = now;
        }
        if next > now {
            thread::sleep(Duration::from_nanos((next - now) * 1_000_000_000 / RATE));
        }
        let packet = {
            let (lock, wake) = &*queue;
            let mut queue = lock.lock().expect("reader does not panic");
            if queue.packets.len() >= buffer.max(1) || queue.closed {
                playing = true;
            }
            if playing && !queue.closed {
                lowest = lowest.min(queue.packets.len());
                window += 1;
                if window == TRIM_WINDOW {
                    if lowest != usize::MAX && lowest > buffer + TRIM_MARGIN {
                        let excess = lowest - buffer;
                        queue.packets.drain(..excess);
                        eprintln!("atmos67: input ran {excess} ms ahead; dropped it");
                    }
                    (lowest, window) = (usize::MAX, 0);
                }
            } else {
                (lowest, window) = (usize::MAX, 0);
            }
            let packet = if playing { queue.packets.pop_front() } else { None };
            if packet.is_none() {
                if queue.closed {
                    break;
                }
                playing = false;
            }
            wake.notify_one();
            packet
        };
        let (measured, finished) = {
            let mut measurement = measurement.lock().expect("control does not panic");
            let packet = measurement.packets.pop_front();
            if packet.is_some() && measurement.started.is_none() {
                measurement.started = Some(next);
            }
            if packet.is_none() {
                measurement.broken = false;
            }
            let last = packet.is_some() && measurement.packets.is_empty();
            let finished = if last {
                let started = measurement.started.take();
                let result = match (std::mem::take(&mut measurement.broken), started) {
                    (true, _) | (_, None) => Err("the sender fell behind the clock while playing; play again".to_string()),
                    (false, Some(started)) => Ok(started),
                };
                measurement.done.take().map(|done| (done, result))
            } else {
                None
            };
            (packet, finished)
        };
        status.playing.store(packet.is_some(), Ordering::Relaxed);
        let target = f32::from_bits(gain.load(Ordering::Relaxed));
        let samples = match (measured.as_deref(), packet.as_deref()) {
            (Some(measured), _) => measured,
            (None, packet) => match packet {
            Some(packet) if current != 1.0 || target != 1.0 => {
                scaled.clear();
                scaled.extend_from_slice(packet);
                ramp(&mut scaled, channels, current, target);
                &scaled
            }
            Some(packet) => packet,
            None => &silence,
            },
        };
        current = target;
        for stream in &mut streams {
            let data = rtp_packet(
                stream.sequence,
                next as u32,
                stream.ssrc,
                samples,
                channels,
                &stream.channels,
            );
            stream.sequence = stream.sequence.wrapping_add(1);
            let sent = stream.socket.send_to(&data, stream.destination);
            if sent.is_err() {
                status.send_errors.fetch_add(1, Ordering::Relaxed);
            }
            if sent.is_err() != send_failed {
                send_failed = sent.is_err();
                match sent {
                    Err(error) => eprintln!("atmos67: cannot send RTP: {error}"),
                    Ok(_) => eprintln!("atmos67: sending RTP again"),
                }
            }
        }
        if let Some((done, result)) = finished {
            let _ = done.send(result);
        }
        next += PACKET_FRAMES as u64;
    }
    for (hash, sdp) in &announcements {
        let _ = announcer.send_to(&sap(true, *hash, source, sdp), SAP_ADDRESS);
    }
    Ok(())
}

/// Moves stdin into `queue` one packet at a time, through `design` when
/// there is one, waiting while it holds `limit` packets. Input short of a
/// whole packet, or of a whole design block, at the end is dropped.
fn read_packets(channels: usize, mut design: Option<Design>, limit: usize, queue: &(Mutex<Queue>, Condvar)) {
    let (lock, wake) = queue;
    let mut stdin = io::stdin().lock();
    let mut bytes = vec![0u8; PACKET_FRAMES * channels * 4];
    let mut corrected = Vec::new();
    let mut taken = 0;
    while stdin.read_exact(&mut bytes).is_ok() {
        let input: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        let packets = match &mut design {
            None => vec![input],
            Some(design) => {
                design.push(&input, &mut corrected);
                let size = PACKET_FRAMES * design.outputs.len();
                let mut packets = Vec::new();
                while corrected.len() - taken >= size {
                    packets.push(corrected[taken..taken + size].to_vec());
                    taken += size;
                }
                corrected.drain(..taken);
                taken = 0;
                packets
            }
        };
        for packet in packets {
            let mut queue = lock.lock().expect("sender does not panic");
            while queue.packets.len() >= limit {
                queue = wake.wait(queue).expect("sender does not panic");
            }
            queue.packets.push_back(packet);
        }
    }
    lock.lock().expect("sender does not panic").closed = true;
}

#[derive(Clone, Copy)]
struct Volume {
    db: f64,
    muted: bool,
}

impl Volume {
    fn gain(self) -> f32 {
        if self.muted { 0.0 } else { 10f64.powf(self.db / 20.0) as f32 }
    }
}

/// Reads the frames of a `play` command, waits until they have been sent
/// and returns the PTP time, in samples, the first was sent at.
fn play(
    reader: &mut impl Read,
    count: &str,
    frames: &str,
    channels: usize,
    measurement: &Mutex<Measurement>,
) -> Result<String, String> {
    let (Ok(count), Ok(frames)) = (count.parse::<usize>(), frames.parse::<usize>()) else {
        return Err("play takes a channel count and a frame count".into());
    };
    // Checked before the frames are read, so a bad size cannot exhaust memory.
    if count != channels {
        return Err(format!("{count} channels given, {channels} sent"));
    }
    if frames > MAX_PLAY_FRAMES {
        return Err(format!("{frames} frames given; at most {MAX_PLAY_FRAMES}"));
    }
    let mut bytes = vec![0u8; count * frames * 4];
    reader.read_exact(&mut bytes).map_err(|e| format!("reading frames: {e}"))?;
    let mut samples: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
    let size = PACKET_FRAMES * channels;
    samples.resize(samples.len().div_ceil(size) * size, 0.0);
    let (done, finished) = mpsc::channel();
    {
        let mut measurement = measurement.lock().expect("sender does not panic");
        if !measurement.packets.is_empty() {
            return Err("already playing".into());
        }
        measurement.packets = samples.chunks(size).map(<[f32]>::to_vec).collect();
        if measurement.packets.is_empty() {
            return Ok(String::new());
        }
        measurement.started = None;
        measurement.done = Some(done);
        measurement.broken = false;
    }
    finished.recv().map_err(|_| "the sender stopped".to_string())?.map(|started| started.to_string())
}

/// Scales `samples` from `from` to `to` across the packet, frame by frame.
fn ramp(samples: &mut [f32], channels: usize, from: f32, to: f32) {
    for (n, frame) in samples.chunks_exact_mut(channels).enumerate() {
        let gain = from + (to - from) * (n + 1) as f32 / PACKET_FRAMES as f32;
        frame.iter_mut().for_each(|s| *s *= gain);
    }
}

/// What the control socket's connections share.
struct Controls {
    volume: Mutex<Volume>,
    gain: Arc<AtomicU32>,
    measurement: Arc<Mutex<Measurement>>,
    channels: usize,
    status: Arc<Status>,
}

/// Serves each connection on its own thread, so a long `play` or a client
/// that keeps its connection open does not hold up the others.
fn control(listener: &UnixListener, controls: &Arc<Controls>) {
    for connection in listener.incoming().flatten() {
        let controls = Arc::clone(controls);
        thread::spawn(move || serve(connection, &controls));
    }
}

/// Answers commands, one per line, with `ok`, `ok <value>` or `error: …`.
fn serve(connection: std::os::unix::net::UnixStream, controls: &Controls) {
    let Ok(mut writer) = connection.try_clone() else { return };
    let mut reader = BufReader::new(connection);
    let mut line = String::new();
    loop {
        line.clear();
        if !matches!(reader.read_line(&mut line), Ok(1..)) {
            return;
        }
        let line = line.trim_end();
        let status = &controls.status;
        let set_volume = |change: &dyn Fn(&mut Volume)| {
            let mut volume = controls.volume.lock().expect("no panic holds the volume");
            change(&mut volume);
            controls.gain.store(volume.gain().to_bits(), Ordering::Relaxed);
        };
        // After a failed `play` its frames may be left unread: the connection ends.
        let mut close = false;
        let reply = match line.split_whitespace().collect::<Vec<_>>()[..] {
            ["play", count, frames] => {
                let reply = play(&mut reader, count, frames, controls.channels, &controls.measurement);
                close = reply.is_err();
                reply
            }
            ["latency"] => Ok(format!("{:.1}", status.info["latency_ms"].as_f64().unwrap_or_default())),
            ["status"] => {
                let volume = *controls.volume.lock().expect("no panic holds the volume");
                let mut report = status.info.clone();
                report["playing"] = status.playing.load(Ordering::Relaxed).into();
                report["send_errors"] = status.send_errors.load(Ordering::Relaxed).into();
                report["volume_db"] = volume.db.into();
                report["muted"] = volume.muted.into();
                Ok(report.to_string())
            }
            ["channels"] => Ok(controls.channels.to_string()),
            ["volume", db] => match db.parse::<f64>() {
                Ok(db) if db.is_finite() => {
                    set_volume(&|v| v.db = db);
                    Ok(String::new())
                }
                _ => Err(format!("bad volume {db}")),
            },
            ["mute", state @ ("0" | "1")] => {
                set_volume(&|v| v.muted = state == "1");
                Ok(String::new())
            }
            _ => Err(format!("unknown command {line:?}")),
        };
        let reply = match reply {
            Ok(value) if value.is_empty() => "ok\n".to_string(),
            Ok(value) => format!("ok {value}\n"),
            Err(e) => format!("error: {e}\n"),
        };
        if writer.write_all(reply.as_bytes()).is_err() || close {
            return;
        }
    }
}

/// Samples of PTP time at `time` on the system clock.
fn ptp_samples(time: SystemTime) -> u64 {
    let tai = time.duration_since(UNIX_EPOCH).unwrap_or_default() + TAI_MINUS_UTC;
    (tai.as_nanos() * u128::from(RATE) / 1_000_000_000) as u64
}

/// An RTP packet carrying `stream` of the `channels` interleaved in `samples`.
fn rtp_packet(
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
    samples: &[f32],
    channels: usize,
    stream: &[usize],
) -> Vec<u8> {
    let mut packet = Vec::with_capacity(12 + PACKET_FRAMES * stream.len() * 3);
    packet.extend([0x80, PAYLOAD_TYPE]);
    packet.extend(sequence.to_be_bytes());
    packet.extend(timestamp.to_be_bytes());
    packet.extend(ssrc.to_be_bytes());
    for frame in samples.chunks_exact(channels) {
        for sample in stream.iter().map(|&c| frame[c]) {
            let value = (sample.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
            packet.extend(&value.to_be_bytes()[1..]);
        }
    }
    packet
}

/// FNV-1a of `bytes`, in 62 bits, as some SDP parsers read numbers signed.
/// The session ID of a stream is that of its name and address: receivers
/// such as Dante keep their subscriptions by it, so it stays the same
/// across restarts.
fn fnv(bytes: impl Iterator<Item = u8>) -> u64 {
    bytes.fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
        >> 2
}

/// The session description aes67-linux-daemon publishes for such a stream.
/// Its origin's version is a hash of the rest, so receivers see a change.
fn sdp(
    name: &str,
    session: u64,
    source: Ipv4Addr,
    address: Ipv4Addr,
    channels: usize,
    config: &Config,
) -> String {
    let body = format!(
        "s={name}\r\n\
         c=IN IP4 {address}/{TTL}\r\n\
         t=0 0\r\n\
         a=clock-domain:PTPv2 {domain}\r\n\
         m=audio {RTP_PORT} RTP/AVP {PAYLOAD_TYPE}\r\n\
         c=IN IP4 {address}/{TTL}\r\n\
         a=rtpmap:{PAYLOAD_TYPE} L24/{RATE}/{channels}\r\n\
         a=sync-time:0\r\n\
         a=framecount:{PACKET_FRAMES}\r\n\
         a=ptime:1\r\n\
         a=mediaclk:direct=0\r\n\
         a=ts-refclk:ptp=IEEE1588-2008:{gmid}:{domain}\r\n\
         a=recvonly\r\n",
        gmid = config.gmid,
        domain = config.domain,
    );
    let version = fnv(source.octets().into_iter().chain(body.bytes()));
    format!("v=0\r\no=- {session} {version} IN IP4 {source}\r\n{body}")
}

/// A SAP (RFC 2974) announcement of `sdp`, or its deletion.
fn sap(deletion: bool, hash: u16, origin: Ipv4Addr, sdp: &str) -> Vec<u8> {
    // Version 1, IPv4 origin, no authentication data.
    let mut packet = vec![if deletion { 0x24 } else { 0x20 }, 0];
    packet.extend(hash.to_be_bytes());
    packet.extend(origin.octets());
    packet.extend(b"application/sdp\0");
    packet.extend(sdp.as_bytes());
    packet
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtp_packet_carries_one_streams_channels_as_l24() {
        // Two frames of three channels; the stream is channels 1 and 2.
        let samples = [0.0, 1.0, -1.0, 0.0, 0.5, 2.0];
        let packet = rtp_packet(0x0102, 0x0304_0506, 0x0708_090A, &samples, 3, &[1, 2]);
        assert_eq!(packet[..12], [0x80, 98, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(
            packet[12..],
            [0x7F, 0xFF, 0xFF, 0x80, 0x00, 0x01, 0x40, 0x00, 0x00, 0x7F, 0xFF, 0xFF]
        );
    }

    #[test]
    fn session_ids_survive_restarts_and_differ_between_streams() {
        let id = |name: &str, last: u8| fnv(name.bytes().chain([239, 69, 67, last]));
        assert_eq!(id("atmos67 ch 1-8", 1), id("atmos67 ch 1-8", 1));
        assert_ne!(id("atmos67 ch 1-8", 1), id("atmos67 ch 9-12", 1));
        assert_ne!(id("atmos67 ch 1-8", 1), id("atmos67 ch 1-8", 2));
    }

    #[test]
    fn ramp_reaches_the_new_gain_at_the_end_of_the_packet() {
        let mut samples = vec![1.0; PACKET_FRAMES * 2];
        ramp(&mut samples, 2, 1.0, 0.5);
        assert!((samples[0] - (1.0 - 0.5 / 48.0)).abs() < 1e-6);
        assert_eq!(samples[PACKET_FRAMES * 2 - 2..], [0.5, 0.5]);
    }

    #[test]
    fn plans_streams_by_speaker_name_in_either_spelling() {
        let names: Vec<String> = ["L", "R", "C", "Ls", "Rs", "SW1"].iter().map(|s| s.to_string()).collect();
        let file = std::env::temp_dir().join(format!("atmos67-plan-{}", std::process::id()));
        std::fs::write(
            &file,
            r#"[{"name": "front", "address": "239.1.1.1", "channels": ["FL", "FR", "FC", "SW1"]},
                {"name": "rear", "address": "239.1.1.2", "channels": ["Rs", "Ls", "TFL"]},
                {"name": "empty", "address": "239.1.1.3", "channels": ["LFE"]}]"#,
        )
        .unwrap();
        let planned = plan(&names, Ipv4Addr::new(239, 0, 0, 1), Some(&file)).unwrap();
        assert_eq!(
            planned,
            vec![
                ("front".to_string(), Ipv4Addr::new(239, 1, 1, 1), vec![0, 1, 2, 5]),
                ("rear".to_string(), Ipv4Addr::new(239, 1, 1, 2), vec![4, 3]),
            ]
        );
        std::fs::write(&file, r#"[{"name": "big", "address": "239.1.1.1", "channels": ["L","L","L","L","L","L","L","L","L"]}]"#).unwrap();
        assert!(plan(&names, Ipv4Addr::new(239, 0, 0, 1), Some(&file)).is_err());
        std::fs::remove_file(file).unwrap();

        let default = plan(&names, Ipv4Addr::new(239, 0, 0, 1), None).unwrap();
        assert_eq!(default, vec![("atmos67 ch 1-6".to_string(), Ipv4Addr::new(239, 0, 0, 1), (0..6).collect())]);
    }

    #[test]
    fn ptp_time_is_tai() {
        let time = UNIX_EPOCH + Duration::from_millis(1_500);
        assert_eq!(ptp_samples(time), (37 * 1000 + 1_500) * 48);
    }

    #[test]
    fn sdp_describes_the_stream_and_its_clock() {
        let config = Config {
            address: Ipv4Addr::new(239, 69, 67, 1),
            gmid: "00-11-22-FF-FE-33-44-55".to_owned(),
            domain: 3,
            buffer_ms: 100,
            volume_db: 0.0,
            control: None,
            streams: None,
        };
        let sdp = sdp(
            "atmos67 ch 9-12",
            7,
            Ipv4Addr::new(10, 0, 0, 2),
            Ipv4Addr::new(239, 69, 67, 2),
            4,
            &config,
        );
        assert!(sdp.starts_with("v=0\r\no=- 7 "));
        assert!(sdp.contains(" IN IP4 10.0.0.2\r\ns=atmos67 ch 9-12\r\n"));
        assert!(sdp.contains("\r\nc=IN IP4 239.69.67.2/15\r\n"));
        assert!(sdp.contains("\r\nm=audio 5004 RTP/AVP 98\r\n"));
        assert!(sdp.contains("\r\na=rtpmap:98 L24/48000/4\r\n"));
        assert!(sdp.contains("\r\na=ts-refclk:ptp=IEEE1588-2008:00-11-22-FF-FE-33-44-55:3\r\n"));
        assert!(sdp.contains("\r\na=clock-domain:PTPv2 3\r\n"));
    }

    #[test]
    fn sdp_version_changes_with_the_description_only() {
        let config = Config {
            address: Ipv4Addr::new(239, 69, 67, 1),
            gmid: "00-11-22-FF-FE-33-44-55".to_owned(),
            domain: 0,
            buffer_ms: 100,
            volume_db: 0.0,
            control: None,
            streams: None,
        };
        let version = |channels: usize| {
            let sdp = sdp("s", 7, Ipv4Addr::new(10, 0, 0, 2), Ipv4Addr::new(239, 69, 67, 1), channels, &config);
            sdp.lines().nth(1).unwrap().split(' ').nth(2).unwrap().to_string()
        };
        assert_eq!(version(6), version(6));
        assert_ne!(version(6), version(7));
    }

    #[test]
    fn sap_wraps_the_description() {
        let packet = sap(false, 0xABCD, Ipv4Addr::new(10, 0, 0, 2), "v=0\r\n");
        assert_eq!(packet[..8], [0x20, 0, 0xAB, 0xCD, 10, 0, 0, 2]);
        assert_eq!(&packet[8..], b"application/sdp\0v=0\r\n");
        assert_eq!(sap(true, 0, Ipv4Addr::LOCALHOST, "")[0], 0x24);
    }
}
