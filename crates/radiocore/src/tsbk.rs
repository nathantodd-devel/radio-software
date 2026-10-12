//! P25 trunking signalling blocks (TSBKs): what a system's control channel
//! says, in particular which frequency each call is being put on.

/// A constellation point (two dibits) for every state and input dibit of
/// the rate-1/2 trellis code. The state is the previous input.
const TRELLIS: [[u8; 4]; 4] = [
    [0x2, 0xC, 0x1, 0xF],
    [0xE, 0x0, 0xD, 0x3],
    [0x9, 0x7, 0xA, 0x4],
    [0x5, 0xB, 0x6, 0x8],
];
/// Constellation points in a block: 48 dibits of data and one to flush.
const POINTS: usize = 49;
/// Bits in a block as sent, and as decoded.
pub const CODED_BITS: usize = POINTS * 4;
const DATA_BITS: usize = 96;

/// Where in a block as sent each constellation point of the code is: they
/// are dealt out in four runs so that a burst of errors is spread around.
fn point_position(point: usize) -> usize {
    point / 4 + [0, 13, 25, 37][point % 4]
}

/// Undo the trellis code of one block, given as 196 bits in the order
/// received, giving the 96 bits it carries. The likeliest reading is always
/// found; whether it is right is for the checksum to say.
pub fn decode_block(coded: &[u8]) -> [u8; DATA_BITS / 8] {
    debug_assert_eq!(coded.len(), CODED_BITS);
    let point = |k: usize| {
        let at = point_position(k) * 4;
        coded[at..at + 4].iter().fold(0u8, |v, &b| v << 1 | b)
    };
    // Viterbi: the fewest bit errors by which each state can be reached.
    const UNREACHED: u32 = u32::MAX / 2;
    let mut cost = [0, UNREACHED, UNREACHED, UNREACHED];
    let mut came_from = [[0u8; 4]; POINTS];
    for (k, back) in came_from.iter_mut().enumerate() {
        let received = point(k);
        let mut next = [UNREACHED; 4];
        for state in 0..4 {
            for input in 0..4 {
                let c = cost[state] + (TRELLIS[state][input] ^ received).count_ones();
                if c < next[input] {
                    next[input] = c;
                    back[input] = state as u8;
                }
            }
        }
        cost = next;
    }
    // The flushing dibit is zero, so the path ends in state 0.
    let mut dibits = [0u8; POINTS];
    let mut state = 0usize;
    for k in (0..POINTS).rev() {
        dibits[k] = state as u8;
        state = came_from[k][state] as usize;
    }
    let mut data = [0u8; DATA_BITS / 8];
    for (k, dibit) in dibits[..POINTS - 1].iter().enumerate() {
        data[k / 4] |= dibit << (6 - 2 * (k % 4));
    }
    data
}

/// The 16-bit checksum of a block's first ten bytes, as sent in its last
/// two.
fn checksum(data: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &byte in data {
        for bit in (0..8).rev() {
            let top = crc >> 15 ^ u16::from(byte >> bit & 1);
            crc <<= 1;
            if top == 1 {
                crc ^= 0x1021;
            }
        }
    }
    !crc
}

/// Something a control channel said that the scanner acts on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Message {
    /// A call for a talkgroup is on (or about to be on) a channel.
    Grant {
        talkgroup: u16,
        channel: u16,
        /// The radio calling, when the message says.
        source: Option<u32>,
    },
    /// How the channels of one of the system's sixteen bands are numbered.
    Band { id: u8, band: Band },
    /// Which network and system this is: what Phase 2 traffic is scrambled
    /// with, along with the network access code.
    Network { wacn: u32, system: u16 },
}

/// One of a system's bands: where its channel 0 is and how far apart
/// channels are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub base_hz: f64,
    pub spacing_hz: f64,
    /// Time slots sharing each frequency: 1, or 2 for Phase 2 channels.
    pub slots: u16,
}

impl Band {
    /// The frequency of a channel number in this band.
    pub fn frequency(&self, number: u16) -> f64 {
        self.base_hz + self.spacing_hz * f64::from(number / self.slots.max(1))
    }
}

/// A decoded block: whether it is the last of its frame, and what it says
/// if that is something of use here.
pub struct Block {
    pub last: bool,
    pub messages: [Option<Message>; 2],
}

/// Read a decoded block; `None` if its checksum says it was garbled.
pub fn read(data: &[u8; DATA_BITS / 8]) -> Option<Block> {
    if checksum(&data[..10]) != u16::from_be_bytes([data[10], data[11]]) {
        return None;
    }
    let (last, opcode, manufacturer) = (data[0] & 0x80 != 0, data[0] & 0x3f, data[1]);
    let word = |at: usize| u16::from_be_bytes([data[at], data[at + 1]]);
    let band = |slots: u16| {
        let (spacing, base) = (
            word(4) & 0x3ff,
            u32::from_be_bytes([data[6], data[7], data[8], data[9]]),
        );
        Message::Band {
            id: data[2] >> 4,
            band: Band {
                base_hz: f64::from(base) * 5.0,
                spacing_hz: f64::from(spacing) * 125.0,
                slots,
            },
        }
    };
    // Only the standard's own messages: a manufacturer's use the same
    // numbers for other things.
    let messages = match (manufacturer, opcode) {
        // Group voice channel grant: options, channel, talkgroup, source.
        (0, 0x00) => [
            Some(Message::Grant {
                channel: word(3),
                talkgroup: word(5),
                source: Some(u32::from_be_bytes([0, data[7], data[8], data[9]])).filter(|&id| id != 0),
            }),
            None,
        ],
        // Grant update: two channel and talkgroup pairs, of calls under way.
        (0, 0x02) => {
            let grant = |at: usize| Message::Grant {
                channel: word(at),
                talkgroup: word(at + 2),
                source: None,
            };
            let second = (word(6) != word(2) || word(8) != word(4)).then_some(grant(6));
            [Some(grant(2)), second]
        }
        // Identifier updates: for 700/800 MHz, VHF and UHF, and TDMA, whose
        // channel type says how many slots share a frequency.
        (0, 0x3d) | (0, 0x34) => [Some(band(1)), None],
        // Network status: location area, WACN (20 bits), system (12 bits).
        (0, 0x3b) => [
            Some(Message::Network {
                wacn: u32::from(data[3]) << 12 | u32::from(data[4]) << 4 | u32::from(data[5] >> 4),
                system: u16::from(data[5] & 0xf) << 8 | u16::from(data[6]),
            }),
            None,
        ],
        (0, 0x33) => {
            let slots = match data[2] & 0xf {
                0 | 1 => 1,
                4 => 4,
                _ => 2,
            };
            [Some(band(slots)), None]
        }
        _ => [None, None],
    };
    Some(Block { last, messages })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Put a block's 96 bits through the trellis code and interleaver.
    pub(crate) fn encode_block(data: &[u8; 12]) -> Vec<u8> {
        let mut coded = vec![0u8; CODED_BITS];
        let mut state = 0usize;
        for k in 0..POINTS {
            let input = if k < 48 {
                usize::from(data[k / 4] >> (6 - 2 * (k % 4)) & 3)
            } else {
                0
            };
            let point = TRELLIS[state][input];
            state = input;
            for bit in 0..4 {
                coded[point_position(k) * 4 + bit] = point >> (3 - bit) & 1;
            }
        }
        coded
    }

    /// A block with its checksum filled in.
    pub(crate) fn block(mut data: [u8; 12]) -> [u8; 12] {
        let crc = checksum(&data[..10]);
        data[10..].copy_from_slice(&crc.to_be_bytes());
        data
    }

    #[test]
    fn spreads_the_code_over_the_whole_block() {
        let mut seen = [false; POINTS];
        (0..POINTS).for_each(|k| seen[point_position(k)] = true);
        assert!(seen.iter().all(|&s| s));
        // The first points of the code, where the interleaver puts them.
        assert_eq!((0..5).map(point_position).collect::<Vec<_>>(), [0, 13, 25, 37, 1]);
    }

    #[test]
    fn decodes_through_errors() {
        // A grant: talkgroup 865 on channel 0x1234 for radio 0x00ABCD.
        let data = block([0x80, 0x00, 0x00, 0x12, 0x34, 0x03, 0x61, 0x00, 0xab, 0xcd, 0, 0]);
        let mut coded = encode_block(&data);
        assert_eq!(decode_block(&coded), data);
        // Errors in points far apart in the code are put right.
        for bit in [8, 60, 131] {
            coded[bit] ^= 1;
        }
        assert_eq!(decode_block(&coded), data);
        let read = read(&data).unwrap();
        assert!(read.last);
        assert_eq!(
            read.messages[0],
            Some(Message::Grant {
                talkgroup: 865,
                channel: 0x1234,
                source: Some(0xabcd)
            })
        );
        // A garbled block is known by its checksum.
        let mut garbled = data;
        garbled[5] ^= 0x10;
        assert!(super::read(&garbled).is_none());
    }

    #[test]
    fn reads_band_plans_and_updates() {
        // Identifier 1: base 851.00625 MHz, 6.25 kHz apart.
        let base = (851_006_250u32 / 5).to_be_bytes();
        let data = block([
            0x3d,
            0,
            0x10 | 0x0c,
            0x80 | 0x5a,
            0x80,
            50,
            base[0],
            base[1],
            base[2],
            base[3],
            0,
            0,
        ]);
        let Some(Message::Band { id, band }) = read(&data).unwrap().messages[0] else {
            panic!("not a band plan");
        };
        assert_eq!(id, 1);
        assert_eq!((band.base_hz, band.spacing_hz), (851_006_250.0, 6250.0));
        assert_eq!(band.frequency(100), 851_631_250.0);
        // Two calls in one update.
        let data = block([0x02, 0, 0x10, 0x64, 0x03, 0x61, 0x10, 0x65, 0x03, 0x62, 0, 0]);
        let block = read(&data).unwrap();
        assert!(!block.last);
        assert_eq!(
            block.messages.map(|m| matches!(m, Some(Message::Grant { .. }))),
            [true, true]
        );
        // A manufacturer's own message with the same number means nothing here.
        assert!(
            read(&super::tests::block([0x00, 0x90, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]))
                .unwrap()
                .messages[0]
                .is_none()
        );
    }
}
