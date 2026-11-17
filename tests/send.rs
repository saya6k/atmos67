//! Runs the AES67 sender against sockets joined to its multicast groups.
//! Needs a network interface with a multicast route.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, UdpSocket};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 7.1.4 as rendered: FL FR FC LFE Lb Rb Ls Rs TFL TFR TBL TBR.
const CHANNELS: usize = 12;
/// 192 packets of 48 frames are 9 blocks of the correction's 1024.
const PACKETS: usize = 192;
const GMID: &str = "00-11-22-FF-FE-33-44-55";

/// The tests share the RTP and SAP ports.
static PORTS: Mutex<()> = Mutex::new(());

/// Every sample is distinct and none is zero, unlike the idle silence.
fn input() -> Vec<f32> {
    (0..PACKETS * 48 * CHANNELS).map(|i| (i % 65_521 + 1) as f32 / 70_000.0).collect()
}

fn spawn(first: Ipv4Addr, extra: &[&str]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_atmos67"))
        .args(["--layout", "7.1.4", "--aes67-send", "--aes67-address", &first.to_string()])
        .args(["--ptp-gmid", GMID, "--aes67-buffer-ms", "50"])
        .args(extra)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap()
}

struct Received {
    /// Per stream, (sequence, timestamp, samples) of the packets that carry audio.
    streams: Vec<Vec<(u16, u32, Vec<i32>)>>,
    /// Announced session descriptions.
    sdps: Vec<String>,
}

struct Ports {
    rtp: UdpSocket,
    sap: UdpSocket,
}

/// Joins the groups of `streams` streams from `first`, and SAP, before the
/// sender starts, so its first announcement is not missed.
fn listen(first: Ipv4Addr, streams: usize) -> Ports {
    let rtp = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 5004)).unwrap();
    for index in 0..streams {
        let group = Ipv4Addr::from(u32::from(first) + index as u32);
        rtp.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED).unwrap();
    }
    rtp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let sap = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 9875)).unwrap();
    sap.join_multicast_v4(&Ipv4Addr::new(239, 255, 255, 255), &Ipv4Addr::UNSPECIFIED).unwrap();
    sap.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    Ports { rtp, sap }
}

/// Feeds `input` to `child` and collects its streams, told apart by payload
/// size: `sizes[i]` is the channel count of stream `i`.
fn receive(ports: Ports, mut child: Child, sizes: &[usize], input: &[f32]) -> Received {
    let Ports { rtp, sap } = ports;

    let mut stdin = child.stdin.take().unwrap();
    let bytes: Vec<u8> = input.iter().flat_map(|s| s.to_le_bytes()).collect();
    let writer = std::thread::spawn(move || stdin.write_all(&bytes).unwrap());

    let mut streams = vec![Vec::new(); sizes.len()];
    let mut packet = [0u8; 2048];
    while streams.iter().any(|stream| stream.len() < PACKETS) {
        let (n, _) = rtp.recv_from(&mut packet).expect("sender stopped early");
        let packet = &packet[..n];
        assert_eq!(packet[..2], [0x80, 98]);
        let stream = sizes
            .iter()
            .position(|channels| channels * 48 * 3 == n - 12)
            .unwrap_or_else(|| panic!("{}-byte payload", n - 12));
        if packet[12..].iter().any(|b| *b != 0) {
            let samples = packet[12..]
                .as_chunks::<3>()
                .0
                .iter()
                .map(|b| i32::from_be_bytes([b[0], b[1], b[2], 0]) >> 8)
                .collect();
            streams[stream].push((
                u16::from_be_bytes(packet[2..4].try_into().unwrap()),
                u32::from_be_bytes(packet[4..8].try_into().unwrap()),
                samples,
            ));
        }
    }
    writer.join().unwrap();
    assert!(child.wait().unwrap().success());

    let mut sdps = Vec::new();
    while sdps.len() < sizes.len() {
        let (n, _) = sap.recv_from(&mut packet).expect("no SAP announcement");
        let text = String::from_utf8_lossy(&packet[8..n]).into_owned();
        if packet[0] == 0x20 && !sdps.contains(&text) {
            sdps.push(text);
        }
    }
    Received { streams, sdps }
}

/// Checks that the streams carry `expected` (interleaved frames of all the
/// output channels) within `tolerance` L24 steps.
fn check(received: &Received, sizes: &[usize], expected: &[f32], tolerance: i32) {
    let outputs: usize = sizes.iter().sum();
    let mut start = 0;
    for (stream, &width) in sizes.iter().enumerate() {
        let samples: Vec<i32> = received.streams[stream].iter().flat_map(|p| p.2.clone()).collect();
        let want: Vec<i32> = expected
            .chunks_exact(outputs)
            .flat_map(|frame| frame[start..start + width].to_vec())
            .map(|s| (s * 8_388_607.0).round() as i32)
            .collect();
        assert_eq!(samples.len(), want.len(), "stream {stream} length");
        let worst = samples.iter().zip(&want).map(|(a, b)| (a - b).abs()).max().unwrap();
        assert!(worst <= tolerance, "stream {stream} is off by {worst} steps");
        start += width;
    }
}

#[test]
fn sends_stdin_as_announced_l24_streams_on_ptp_time() {
    let _ports = PORTS.lock().unwrap_or_else(|e| e.into_inner());
    let first = Ipv4Addr::new(239, 69, 67, 201);
    let input = input();
    let ports = listen(first, 2);
    let received = receive(ports, spawn(first, &[]), &[8, 4], &input);
    check(&received, &[8, 4], &input, 0);

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap() + Duration::from_secs(37);
    let now = (now.as_nanos() * 48_000 / 1_000_000_000) as u32;
    for packets in &received.streams {
        for pair in packets.windows(2) {
            assert_eq!(pair[1].0, pair[0].0.wrapping_add(1));
            assert_eq!(pair[1].1, pair[0].1.wrapping_add(48));
        }
        // The audio was sent within the last few seconds of PTP time.
        assert!(now.wrapping_sub(packets[0].1) < 5 * 48_000);
    }
    // Both streams leave with the same timestamps, so receivers stay aligned.
    assert_eq!(received.streams[0][0].1, received.streams[1][0].1);
    assert!(received.sdps.iter().any(|sdp| sdp.contains("c=IN IP4 239.69.67.201/15")
        && sdp.contains("a=rtpmap:98 L24/48000/8")));
    assert!(received.sdps.iter().any(|sdp| sdp.contains("c=IN IP4 239.69.67.202/15")
        && sdp.contains("a=rtpmap:98 L24/48000/4")));
}

/// A SECS 2R export in miniature: its inputs in SECS order, the eleven
/// mains passed through and the LFE sent 6 dB down to a subwoofer.
fn write_design(folder: &Path) {
    let inputs = ["L", "R", "C", "Ls", "Rs", "Lb", "Rb", "Tfl", "Tfr", "Tbl", "Tbr", "LFE"];
    let outputs = ["L", "R", "C", "Ls", "Rs", "Lb", "Rb", "Tfl", "Tfr", "Tbl", "Tbr", "SW1"];
    let quote = |names: &[&str]| names.iter().map(|n| format!("\"{n}\"")).collect::<Vec<_>>().join(", ");
    std::fs::create_dir_all(folder.join("configs")).unwrap();
    std::fs::write(
        folder.join("secs-design.json"),
        format!("{{\"inputs\": [{}], \"outputs\": [{}]}}", quote(&inputs), quote(&outputs)),
    )
    .unwrap();
    let mapping: Vec<String> = (0..12)
        .map(|n| {
            let gain = if n == 11 { -6.0206 } else { 0.0 };
            format!("{{\"dest\": {n}, \"sources\": [{{\"channel\": {n}, \"gain\": {gain}, \"inverted\": false}}]}}")
        })
        .collect();
    std::fs::write(
        folder.join("configs/design.yml"),
        format!(
            "devices:\n  samplerate: 48000\n  capture: {{\"channels\": 12}}\nfilters:\nmixers:\n  route: {{\"channels\": {{\"in\": 12, \"out\": 12}}, \"mapping\": [{}]}}\npipeline:\n  - {{\"type\": \"Mixer\", \"name\": \"route\"}}\n",
            mapping.join(", ")
        ),
    )
    .unwrap();
}

#[test]
fn corrects_with_a_secs_design() {
    let _ports = PORTS.lock().unwrap_or_else(|e| e.into_inner());
    let folder = std::env::temp_dir().join(format!("atmos67-design-{}", std::process::id()));
    write_design(&folder);
    let first = Ipv4Addr::new(239, 69, 67, 211);
    let input = input();
    let ports = listen(first, 2);
    let child = spawn(first, &["--dsp", folder.to_str().unwrap()]);
    let received = receive(ports, child, &[8, 4], &input);
    // Rendered FL FR FC LFE Lb Rb Ls Rs TFL … TBR leave in the design's order,
    // L R C Ls Rs Lb Rb Tfl … Tbr, then SW1.
    let expected: Vec<f32> = input
        .as_chunks::<CHANNELS>()
        .0
        .iter()
        .flat_map(|f| [f[0], f[1], f[2], f[6], f[7], f[4], f[5], f[8], f[9], f[10], f[11], f[3] * 0.5])
        .collect();
    check(&received, &[8, 4], &expected, 1);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn takes_volume_commands_on_its_socket() {
    let _ports = PORTS.lock().unwrap_or_else(|e| e.into_inner());
    let socket = std::env::temp_dir().join(format!("atmos67-control-{}", std::process::id()));
    let first = Ipv4Addr::new(239, 69, 67, 221);
    let ports = listen(first, 2);
    let child = spawn(first, &["--volume", "-6.0206", "--control", socket.to_str().unwrap()]);
    let mut connection = (0..50)
        .find_map(|_| {
            std::thread::sleep(Duration::from_millis(100));
            UnixStream::connect(&socket).ok()
        })
        .expect("control socket");
    connection.write_all(b"volume -12.0412\nvolume loud\n").unwrap();
    let mut replies = BufReader::new(connection).lines();
    assert_eq!(replies.next().unwrap().unwrap(), "ok");
    assert!(replies.next().unwrap().unwrap().starts_with("error: "));

    let input = input();
    let received = receive(ports, child, &[8, 4], &input);
    let expected: Vec<f32> = input.iter().map(|s| s * 0.25).collect();
    check(&received, &[8, 4], &expected, 1);
}
