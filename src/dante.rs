//! Dante receivers (in AES67 mode) found and subscribed to the sender's
//! streams, without Dante Controller (`atmos67 dante …`).
//!
//! Dante's control protocol is Audinate's own; netaudio's `netaudio-core`
//! (Unlicense, reverse engineered) builds and sends its packets. Devices
//! are found by their `_netaudio-arc._udp` mDNS service, whose `arcp_vers`
//! property names the protocol revision. A receiver takes an AES67 stream
//! by an "external RTP" subscription naming the stream by the source
//! address and SDP session ID its SAP announcement carries, and mapping
//! stream channels to receiver channels.
//!
//! None of this has met a Dante device yet (tasks/ui.md U4).

use std::collections::BTreeMap;
use std::error::Error;
use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent};
use netaudio_core::client::Client;
use netaudio_core::protocol::{SERVICE_ARC, arc_protocol};
use netaudio_core::spec::{ExternalSubscriptionPlanRequest, plan_external_subscription};
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const TIMEOUT: Duration = Duration::from_millis(800);
const ATTEMPTS: u32 = 3;

pub struct Device {
    pub name: String,
    pub ipv4: Ipv4Addr,
    pub port: u16,
    /// The `arcp_vers` mDNS property: the control protocol revision.
    pub protocol_version: Option<String>,
}

/// A stream of the sender as a receiver must name it.
#[derive(Debug, PartialEq)]
pub struct Flow {
    pub source: Ipv4Addr,
    pub session: u64,
    pub address: Ipv4Addr,
    pub port: u16,
    pub channels: u16,
}

/// The Dante devices that answer on the network within `wait`.
pub fn discover(wait: Duration) -> Result<Vec<Device>> {
    let daemon = ServiceDaemon::new()?;
    let events = daemon.browse(SERVICE_ARC)?;
    let suffix = format!(".{SERVICE_ARC}");
    let deadline = Instant::now() + wait;
    let mut found = BTreeMap::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(event) = events.recv_timeout(left) else { break };
        let ServiceEvent::ServiceResolved(service) = event else { continue };
        let fullname = service.get_fullname();
        let name = fullname.strip_suffix(&suffix).unwrap_or(fullname).to_string();
        // Routable addresses first; without DHCP Dante devices have only a
        // link-local one. The lowest otherwise, so the choice is the same each time.
        let Some(ipv4) = service
            .get_addresses_v4()
            .into_iter()
            .map(|address| address.to_owned())
            .filter(|address: &Ipv4Addr| !address.is_unspecified())
            .min_by_key(|address| (address.is_link_local(), *address))
        else {
            continue;
        };
        let protocol_version = service
            .get_properties()
            .iter()
            .find(|property| property.key() == "arcp_vers")
            .and_then(|property| property.val())
            .and_then(|value| std::str::from_utf8(value).ok())
            .map(String::from);
        found.insert(name.clone(), Device { name, ipv4, port: service.get_port(), protocol_version });
    }
    let _ = daemon.shutdown();
    Ok(found.into_values().collect())
}

/// The device called `name`, looked for for up to `wait`.
pub fn find(name: &str, wait: Duration) -> Result<Device> {
    discover(wait)?
        .into_iter()
        .find(|device| device.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("no Dante device called {name} answered").into())
}

/// netaudio's client errors implement Display only.
fn failed(device: &Device, error: impl std::fmt::Display) -> Box<dyn Error> {
    format!("{}: {error}", device.name).into()
}

fn client(device: &Device) -> Result<Client> {
    Client::new(IpAddr::V4(device.ipv4), None, device.port, TIMEOUT, ATTEMPTS).map_err(|e| failed(device, e))
}

/// Turns AES67 mode on or off. Dante devices apply it after a reboot.
/// netaudio sends the command without waiting for an answer, so the
/// configured setting is read back to confirm the device took it.
pub fn set_aes67(device: &Device, enabled: bool) -> Result<()> {
    let command = json!({ "command": "enable_aes67", "enabled": enabled }).to_string();
    let mut client = client(device)?;
    client.execute(&command).map_err(|e| failed(device, e))?;
    let mut configured = None;
    for _ in 0..ATTEMPTS {
        std::thread::sleep(Duration::from_millis(300));
        configured = client.get_aes67_configured().map_err(|e| failed(device, e))?;
        if configured == Some(enabled) {
            return Ok(());
        }
    }
    Err(match configured {
        Some(_) => format!("{} still has AES67 {}", device.name, if enabled { "off" } else { "on" }),
        None => format!("{} does not report its AES67 setting", device.name),
    }
    .into())
}

/// The subscription request for `flow` onto receiver channels from
/// `first_channel` on, one per stream channel in order.
pub fn plan_request(flow: &Flow, protocol_id: u16, first_channel: u16) -> Value {
    let slots: Vec<u16> = (1..=flow.channels).collect();
    let receivers: Vec<u16> = slots.iter().map(|slot| first_channel + slot - 1).collect();
    json!({
        "device_protocol": protocol_id,
        "receiver": {},
        "source_sample_rate": 48_000,
        "source_encoding": "L24",
        "source_direction": "recvonly",
        "receiver_channel_ids": receivers,
        "flow_slot_assignments": slots,
        "advertised_flow_slot_count": flow.channels,
        "source_address": flow.source.to_string(),
        "session_id": flow.session,
        // The streams' SDP says a=mediaclk:direct=0.
        "clock_offset": 0,
        "primary_destination": { "address": flow.address.to_string(), "port": flow.port },
        "secondary_address": null,
        "secondary_port": null,
        "receiver_supports_multiple_interfaces": false,
    })
}

/// The command that subscribes `flow`, checked by netaudio before it is sent.
fn command(flow: &Flow, protocol_id: u16, first_channel: u16) -> Result<String> {
    if first_channel.checked_add(flow.channels.saturating_sub(1)).is_none() {
        return Err(format!("{} channels from receiver channel {first_channel} on run past 65535", flow.channels).into());
    }
    let request: ExternalSubscriptionPlanRequest = serde_json::from_value(plan_request(flow, protocol_id, first_channel))?;
    let command = plan_external_subscription(request).map_err(|e| format!("{e:?}"))?;
    Ok(serde_json::to_string(&command)?)
}

/// Subscribes receiver channels from `first_channel` on to `flow`.
pub fn subscribe(device: &Device, flow: &Flow, first_channel: u16) -> Result<()> {
    let protocol = arc_protocol(device.protocol_version.as_deref(), false)?
        .ok_or_else(|| format!("{} advertises no control protocol revision", device.name))?;
    let command = command(flow, protocol.protocol_id, first_channel)?;
    client(device)?.execute(&command).map_err(|e| failed(device, e))?;
    Ok(())
}

/// The stream called `name`, from the status of the sender at `control`.
pub fn flow(control: &Path, name: &str) -> Result<Flow> {
    let mut socket = UnixStream::connect(control).map_err(|e| format!("sender control {}: {e}", control.display()))?;
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    socket.set_write_timeout(Some(Duration::from_secs(5)))?;
    let status: Value = serde_json::from_str(&crate::latency::ask(&mut socket, b"status\n")?)?;
    let streams = status["streams"].as_array().ok_or("the sender reports no streams")?;
    let stream = streams.iter().find(|s| s["name"] == name).ok_or_else(|| {
        let names: Vec<&str> = streams.iter().filter_map(|s| s["name"].as_str()).collect();
        format!("no stream called {name}; the sender has {}", names.join(", "))
    })?;
    let text = |value: &Value, what: &str| -> Result<String> {
        Ok(value.as_str().ok_or_else(|| format!("the sender's status has no {what}"))?.to_string())
    };
    Ok(Flow {
        source: text(&status["source"], "source")?.parse()?,
        session: text(&stream["session"], "session")?.parse()?,
        address: text(&stream["address"], "address")?.parse()?,
        port: stream["port"].as_u64().ok_or("the sender's status has no port")? as u16,
        channels: stream["channels"].as_array().map_or(0, Vec::len) as u16,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow() -> Flow {
        Flow {
            source: Ipv4Addr::new(10, 0, 0, 2),
            session: 0x1234_5678_9abc,
            address: Ipv4Addr::new(239, 69, 67, 1),
            port: 5004,
            channels: 4,
        }
    }

    #[test]
    fn netaudio_accepts_the_subscription_of_a_stream() {
        let command: Value = serde_json::from_str(&command(&flow(), 0x2809, 9).unwrap()).unwrap();
        assert_eq!(command["command"], "subscribe_external_rtp");
        assert_eq!(command["receiver_channel_ids"], json!([9, 10, 11, 12]));
        assert_eq!(command["flow_slot_assignments"], json!([1, 2, 3, 4]));
        assert_eq!(command["primary_destination"], json!({ "address": "239.69.67.1", "port": 5004 }));
    }

    #[test]
    fn refuses_receiver_channels_past_65535() {
        assert!(command(&flow(), 0x2809, 65533).is_err());
    }

    #[test]
    fn netaudio_refuses_receiver_channel_zero() {
        assert!(command(&flow(), 0x2809, 0).is_err());
    }
}
