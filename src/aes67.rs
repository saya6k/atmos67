//! Stream layout for aes67-linux-daemon: its status file lists the RTP sources
//! and which channels of the RAVENNA ALSA playback device feed each one.

/// AES67 receivers are only required to accept streams of up to 8 channels.
const CHANNELS_PER_STREAM: usize = 8;

/// Returns a daemon status file sending `channels` ALSA playback channels, in
/// order, as 48 kHz L24 streams with 1 ms packets (the AES67 baseline).
pub fn status_json(channels: usize) -> String {
    let all: Vec<usize> = (0..channels).collect();
    let sources: Vec<String> = all
        .chunks(CHANNELS_PER_STREAM)
        .enumerate()
        .map(|(id, map)| {
            format!(
                r#"    {{
      "id": {id},
      "enabled": true,
      "name": "atmos67 ch {first}-{last}",
      "io": "Audio Device",
      "max_samples_per_packet": 48,
      "codec": "L24",
      "address": "",
      "ttl": 15,
      "payload_type": 98,
      "dscp": 34,
      "refclk_ptp_traceable": false,
      "map": {map:?}
    }}"#,
                first = map[0] + 1,
                last = map[map.len() - 1] + 1,
            )
        })
        .collect();
    format!(
        "{{\n  \"sources\": [\n{}\n  ],\n  \"sinks\": [ ]\n}}\n",
        sources.join(",\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn twelve_channels_split_into_two_streams() {
        let json = status_json(12);
        assert_eq!(json.matches("\"id\"").count(), 2);
        assert!(json.contains("\"name\": \"atmos67 ch 1-8\""));
        assert!(json.contains("\"map\": [0, 1, 2, 3, 4, 5, 6, 7]"));
        assert!(json.contains("\"name\": \"atmos67 ch 9-12\""));
        assert!(json.contains("\"map\": [8, 9, 10, 11]"));
    }
}
