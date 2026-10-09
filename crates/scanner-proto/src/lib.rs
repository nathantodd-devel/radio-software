//! The messages the scanner's web server and its browser client exchange
//! over their WebSocket.
//!
//! Text frames carry JSON: [`ServerMessage`] one way, [`ClientMessage`] the
//! other. Binary frames, server to client only, carry what is playing as
//! little-endian 16-bit mono audio at [`AUDIO_RATE`], and arrive only while
//! something is.

use serde::{Deserialize, Serialize};

/// The path of the WebSocket on the server.
pub const SOCKET_PATH: &str = "/ws";
/// Samples per second of the audio in binary frames.
pub const AUDIO_RATE: u32 = 16_000;

/// A system in the channel database.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct System {
    pub id: i64,
    pub name: String,
    pub location: String,
    pub channels: usize,
    /// Lowest and highest frequency, in Hz; both 0 for an empty system.
    pub lo_hz: f64,
    pub hi_hz: f64,
    /// Whether it is one of the systems being scanned.
    pub selected: bool,
}

/// A channel being scanned. Its place in [`ServerMessage::Channels`] is the
/// number it goes by everywhere else.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    /// Frequency in MHz or talkgroup, as text: "488.3125", "TG 865".
    pub label: String,
    pub name: String,
    pub description: String,
}

/// A kind of receiver, or a way of scanning, that can be chosen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    /// What to send back to choose it.
    pub id: String,
    pub name: String,
}

/// The settings a listener can change, and what there is to choose from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Options {
    pub volume: f32,
    pub squelch_db: f32,
    pub muted: bool,
    /// Receiver gain, 0-21.
    pub gain: u8,
    /// Frequency correction, in parts per million.
    pub ppm: i32,
    pub bias_tee: bool,
    /// The id of the chosen [`scan_modes`](Self::scan_modes) entry.
    pub scan_mode: String,
    pub scan_modes: Vec<Choice>,
    /// The id of the chosen [`devices`](Self::devices) entry.
    pub device: String,
    pub devices: Vec<Choice>,
    /// The receiver in use, if one was found.
    pub receiver: Option<String>,
    /// Sample rate in use and the ones on offer, in samples per second.
    pub sample_rate: Option<u32>,
    pub sample_rates: Vec<u32>,
    /// Widest spread of frequencies received at once, in Hz.
    pub span_hz: Option<f64>,
}

/// How one channel stands right now. In the same order as
/// [`ServerMessage::Channels`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelState {
    /// Signal over the noise floor, in dB.
    pub level_db: f32,
    /// A transmission is on it.
    pub open: bool,
    pub calls: u32,
    /// Time of day it was last heard, "18:24:44".
    pub last_heard: Option<String>,
    pub skipped: bool,
    pub priority: bool,
    pub recorded: bool,
}

/// One transmission in the activity log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Call {
    /// Names the call in [`ClientMessage::Replay`].
    pub id: u64,
    pub time: String,
    pub channel: usize,
    /// What to show it as: the channel's name, or "Talkgroup 1234".
    pub name: String,
    pub unit: Option<u32>,
    pub snr_db: f32,
    /// Length in seconds, once it has ended.
    pub secs: Option<f32>,
    /// It was the one on the speaker.
    pub played: bool,
    /// It can be replayed.
    pub recorded: bool,
}

/// The band the receiver is tuned to, when taking turns between several.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Band {
    pub index: usize,
    pub count: usize,
    pub lo_hz: f64,
    pub hi_hz: f64,
}

/// Everything that changes from moment to moment.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub channels: Vec<ChannelState>,
    /// The channel on the speaker.
    pub playing: Option<usize>,
    /// The channel being held on.
    pub held: Option<usize>,
    pub band: Option<Band>,
    pub replaying: bool,
    /// Recent calls, newest first.
    pub log: Vec<Call>,
    /// Why nothing is being scanned, if nothing is.
    pub problem: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// The channel database's systems. Sent on connecting and when it or
    /// the selection changes.
    Systems { systems: Vec<System> },
    /// The channels of the selected systems. Sent on connecting and when
    /// the selection changes.
    Channels { channels: Vec<Channel> },
    /// Sent on connecting and when any option changes.
    Options(Options),
    /// Sent a few times a second.
    Status(Status),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Add a system to the scan or take it out.
    ToggleSystem {
        id: i64,
    },
    SetVolume {
        volume: f32,
    },
    SetSquelch {
        db: f32,
    },
    SetMuted {
        muted: bool,
    },
    SetGain {
        gain: u8,
    },
    SetPpm {
        ppm: i32,
    },
    SetBiasTee {
        on: bool,
    },
    SetScanMode {
        id: String,
    },
    SetDevice {
        id: String,
    },
    SetSampleRate {
        rate: u32,
    },
    /// Stay on one channel, or go back to scanning.
    Hold {
        channel: Option<usize>,
    },
    Skip {
        channel: usize,
        on: bool,
    },
    Priority {
        channel: usize,
        on: bool,
    },
    Record {
        channel: usize,
        on: bool,
    },
    /// Play a finished call's recording in place of live audio.
    Replay {
        call: u64,
    },
    StopReplay,
}

impl ServerMessage {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("messages always serialise")
    }
}

impl ClientMessage {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("messages always serialise")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let message = ClientMessage::Hold { channel: Some(3) };
        let json = message.to_json();
        assert_eq!(json, r#"{"type":"hold","channel":3}"#);
        assert_eq!(serde_json::from_str::<ClientMessage>(&json).unwrap(), message);

        let message = ServerMessage::Status(Status {
            playing: Some(1),
            ..Status::default()
        });
        assert_eq!(
            serde_json::from_str::<ServerMessage>(&message.to_json()).unwrap(),
            message
        );
        assert!(serde_json::from_str::<ClientMessage>(r#"{"type":"nonsense"}"#).is_err());
    }
}
