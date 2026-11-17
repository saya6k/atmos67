//! Runs the AES67 sender against sockets joined to its multicast groups.
//! Needs a network interface with a multicast route.

use std::io::Write;
use std::net::{Ipv4Addr, UdpSocket};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CHANNELS: usize = 12;
const PACKETS: usize = 300;

#[test]
fn sends_stdin_as_announced_l24_streams_on_ptp_time() {
    let first = Ipv4Addr::new(239, 69, 67, 201);
    let rtp = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 5004)).unwrap();
    for group in [first, Ipv4Addr::new(239, 69, 67, 202)] {
        rtp.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED).unwrap();
    }
    rtp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let sap = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 9875)).unwrap();
    sap.join_multicast_v4(&Ipv4Addr::new(239, 255, 255, 255), &Ipv4Addr::UNSPECIFIED).unwrap();
    sap.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

    // Every sample is distinct and none is zero, unlike the idle silence.
    let input: Vec<f32> = (0..PACKETS * 48 * CHANNELS)
        .map(|i| (i % 65_521 + 1) as f32 / 70_000.0)
        .collect();
    let mut child = Command::new(env!("CARGO_BIN_EXE_atmos67"))
        .args(["--layout", "7.1.4", "--aes67-send", "--aes67-address", "239.69.67.201"])
        .args(["--ptp-gmid", "00-11-22-FF-FE-33-44-55", "--aes67-buffer-ms", "50"])
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let bytes: Vec<u8> = input.iter().flat_map(|s| s.to_le_bytes()).collect();
    let writer = std::thread::spawn(move || stdin.write_all(&bytes).unwrap());

    // (sequence, timestamp, payload) of the packets that carry audio.
    let mut streams: [Vec<(u16, u32, Vec<u8>)>; 2] = [Vec::new(), Vec::new()];
    let mut packet = [0u8; 2048];
    while streams.iter().any(|stream| stream.len() < PACKETS) {
        let (n, _) = rtp.recv_from(&mut packet).expect("sender stopped early");
        let packet = &packet[..n];
        assert_eq!(packet[..2], [0x80, 98]);
        let stream = match n - 12 {
            1152 => 0, // 48 frames of 8 channels
            576 => 1,  // 48 frames of 4 channels
            size => panic!("{size}-byte payload"),
        };
        if packet[12..].iter().any(|b| *b != 0) {
            streams[stream].push((
                u16::from_be_bytes(packet[2..4].try_into().unwrap()),
                u32::from_be_bytes(packet[4..8].try_into().unwrap()),
                packet[12..].to_vec(),
            ));
        }
    }
    writer.join().unwrap();
    assert!(child.wait().unwrap().success());

    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap() + Duration::from_secs(37);
    let now = (now.as_nanos() * 48_000 / 1_000_000_000) as u32;
    for (stream, channels) in [(0, 0..8), (1, 8..12)] {
        let packets = &streams[stream];
        for pair in packets.windows(2) {
            assert_eq!(pair[1].0, pair[0].0.wrapping_add(1));
            assert_eq!(pair[1].1, pair[0].1.wrapping_add(48));
        }
        // The audio was sent within the last few seconds of PTP time.
        assert!(now.wrapping_sub(packets[0].1) < 5 * 48_000);
        let received: Vec<u8> = packets.iter().flat_map(|p| p.2.clone()).collect();
        let expected: Vec<u8> = input
            .as_chunks::<CHANNELS>()
            .0
            .iter()
            .flat_map(|frame| frame[channels.clone()].to_vec())
            .flat_map(|s| ((s * 8_388_607.0).round() as i32).to_be_bytes()[1..].to_vec())
            .collect();
        assert!(received == expected, "stream {stream} differs from the input");
    }
    // Both streams leave with the same timestamps, so receivers stay aligned.
    assert_eq!(streams[0][0].1, streams[1][0].1);

    let mut announced = Vec::new();
    while announced.len() < 2 {
        let (n, _) = sap.recv_from(&mut packet).expect("no SAP announcement");
        let text = String::from_utf8_lossy(&packet[8..n]).into_owned();
        if packet[0] == 0x20 && !announced.contains(&text) {
            announced.push(text);
        }
    }
    assert!(announced.iter().any(|sdp| sdp.contains("c=IN IP4 239.69.67.201/15")
        && sdp.contains("a=rtpmap:98 L24/48000/8")));
    assert!(announced.iter().any(|sdp| sdp.contains("c=IN IP4 239.69.67.202/15")
        && sdp.contains("a=rtpmap:98 L24/48000/4")));
}
