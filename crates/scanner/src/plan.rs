//! Plans: the channels monitored together in one Airspy tuning.

use std::fs;
use std::path::Path;

/// Spacing of the frequencies the channelizer can centre a channel on.
pub const BIN_HZ: f64 = 500.0;

pub enum Kind {
    /// An analog FM channel on its own frequency.
    Analog {
        freq_hz: f64,
        tone_hz: Option<f32>,
        narrow: bool,
    },
    /// A P25 channel on its own frequency (conventional, not trunked),
    /// optionally only for transmissions with a given network access code.
    Digital { freq_hz: f64, nac: Option<u16> },
    /// A P25 talkgroup, heard on whichever of its system's frequencies
    /// carries it.
    Talkgroup { system: i64, id: u16 },
    /// Calls on a P25 system to talkgroups the plan doesn't list.
    OtherTalkgroups { system: i64 },
}

/// One thing to listen to: a row in the scanner.
pub struct Entry {
    /// The channel's row in the database, if it came from there.
    pub channel_id: Option<i64>,
    /// Start out ignoring this channel.
    pub skip: bool,
    /// Interrupts whatever else is playing.
    pub priority: bool,
    /// Its calls are always saved, whether or not everything is.
    pub record: bool,
    pub kind: Kind,
    pub tag: String,
    pub desc: String,
}

impl Entry {
    /// Frequency in MHz or talkgroup number, for display.
    pub fn id(&self) -> String {
        match self.kind {
            Kind::Analog { freq_hz, .. } | Kind::Digital { freq_hz, .. } => format!("{:.4}", freq_hz / 1e6),
            Kind::Talkgroup { id, .. } => format!("TG {id}"),
            Kind::OtherTalkgroups { .. } => "TG *".into(),
        }
    }
}

/// A frequency to watch for P25 voice, and the system it belongs to.
pub struct P25Frequency {
    pub system: i64,
    pub freq_hz: f64,
}

#[derive(Default)]
pub struct Plan {
    pub entries: Vec<Entry>,
    pub p25: Vec<P25Frequency>,
}

impl Plan {
    /// Read a channel file: this program's own CSV format or a RadioReference
    /// CSV export. `sites` is a RadioReference trunked-site export, needed
    /// alongside a talkgroup export to know the system's frequencies.
    /// Returns the plan and notes about anything that was left out.
    pub fn from_file(path: &Path, sites: Option<&Path>) -> Result<(Self, Vec<String>), String> {
        let read = |p: &Path| fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
        let sites = sites
            .map(|p| Ok::<_, String>((read(p)?, p.display().to_string())))
            .transpose()?;
        Self::parse(
            &read(path)?,
            &path.display().to_string(),
            sites.as_ref().map(|(t, n)| (t.as_str(), n.as_str())),
        )
    }

    /// As [`from_file`](Self::from_file), from text; `name` is for messages.
    pub fn parse(text: &str, name: &str, sites: Option<(&str, &str)>) -> Result<(Self, Vec<String>), String> {
        let header: Vec<String> = text.lines().next().map(csv_fields).unwrap_or_default();
        let has = |col: &str| header.iter().any(|h| h.eq_ignore_ascii_case(col));
        let (mut plan, mut notes) = if has("Frequency Output") {
            radioreference_conventional(text, name)?
        } else if has("Decimal") && has("Alpha Tag") {
            radioreference_talkgroups(text, name)?
        } else {
            (native(text, name)?, Vec::new())
        };
        if let Some((text, name)) = sites {
            plan.p25.extend(radioreference_sites(text, name)?);
        }
        let talkgroups = plan.entries.iter().any(|e| matches!(e.kind, Kind::Talkgroup { .. }));
        if talkgroups && plan.p25.is_empty() {
            return Err(format!(
                "{name}: talkgroups need their system's frequencies; pass the system's site export with --sites"
            ));
        }
        if !talkgroups && !plan.p25.is_empty() {
            notes.push("no talkgroups listed; every call will show as \"Other\"".into());
        }
        if plan.entries.is_empty() && plan.p25.is_empty() {
            return Err(format!("{name}: no channels this scanner can receive"));
        }
        plan.add_other_talkgroups();
        Ok((plan, notes))
    }

    /// Give every P25 system a catch-all entry after its talkgroups.
    pub(crate) fn add_other_talkgroups(&mut self) {
        let mut systems: Vec<i64> = self.p25.iter().map(|f| f.system).collect();
        systems.dedup();
        for system in systems {
            let after = self
                .entries
                .iter()
                .rposition(|e| matches!(e.kind, Kind::Talkgroup { system: s, .. } if s == system));
            let entry = Entry {
                channel_id: None,
                skip: false,
                priority: false,
                record: false,
                kind: Kind::OtherTalkgroups { system },
                tag: "Other".into(),
                desc: "Talkgroups not in the database".into(),
            };
            self.entries.insert(after.map_or(self.entries.len(), |i| i + 1), entry);
        }
    }

    /// Everything that needs a receiver: (frequency, what to listen for).
    fn listeners(&self) -> impl Iterator<Item = (f64, Listener)> + '_ {
        let fixed = self.entries.iter().enumerate().filter_map(|(row, e)| match e.kind {
            Kind::Analog { freq_hz, .. } | Kind::Digital { freq_hz, .. } => Some((freq_hz, Listener::Row(row))),
            _ => None,
        });
        fixed.chain(
            self.p25
                .iter()
                .map(|f| (f.freq_hz, Listener::Trunked { system: f.system })),
        )
    }

    /// Every frequency the plan listens on.
    pub fn frequencies(&self) -> impl Iterator<Item = f64> + '_ {
        self.listeners().map(|(freq_hz, _)| freq_hz)
    }

    /// Lowest frequency, highest frequency and their midpoint, in Hz.
    pub fn span(&self) -> (f64, f64, f64) {
        let (lo, hi) = self
            .frequencies()
            .fold((f64::MAX, f64::MIN), |(lo, hi), f| (lo.min(f), hi.max(f)));
        (lo, hi, (lo + hi) / 2.0)
    }

    /// Split the plan into as few tunings as cover every frequency, each no
    /// wider than `max_span_hz`, lowest first.
    pub fn bands(&self, max_span_hz: f64) -> Vec<Band> {
        let mut listeners: Vec<(f64, Listener)> = self.listeners().collect();
        listeners.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut bands: Vec<Band> = Vec::new();
        for (freq_hz, listener) in listeners {
            match bands.last_mut() {
                Some(band) if freq_hz - band.lo_hz <= max_span_hz => {
                    band.hi_hz = freq_hz;
                    band.listeners.push((freq_hz, listener));
                }
                _ => bands.push(Band {
                    lo_hz: freq_hz,
                    hi_hz: freq_hz,
                    center_hz: 0.0,
                    listeners: vec![(freq_hz, listener)],
                }),
            }
        }
        for band in &mut bands {
            // Channels sit on a raster of their own; tune on the bin grid or
            // half a bin off it, whichever lands channels closer to bin centres.
            let on_grid = ((band.lo_hz + band.hi_hz) / 2.0 / BIN_HZ).round() * BIN_HZ;
            let worst = |center: f64| {
                let off = |f: f64| ((f - center) / BIN_HZ).fract().abs();
                band.listeners
                    .iter()
                    .map(|(f, _)| off(*f).min(1.0 - off(*f)))
                    .fold(0.0, f64::max)
            };
            let off_grid = on_grid + BIN_HZ / 2.0;
            band.center_hz = if worst(off_grid) < worst(on_grid) {
                off_grid
            } else {
                on_grid
            };
        }
        bands
    }
}

/// What a receiver on one frequency is listening for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Listener {
    /// The analog or digital channel of this plan entry.
    Row(usize),
    /// Calls on a trunked P25 system, for any of its talkgroups.
    Trunked { system: i64 },
}

/// Frequencies near enough to each other to be received in one tuning.
pub struct Band {
    pub lo_hz: f64,
    pub hi_hz: f64,
    /// The frequency to tune to.
    pub center_hz: f64,
    /// (frequency, what to listen for), lowest first.
    pub listeners: Vec<(f64, Listener)>,
}

pub(crate) fn entry(kind: Kind, tag: &str, desc: &str) -> Entry {
    Entry {
        channel_id: None,
        skip: false,
        priority: false,
        record: false,
        kind,
        tag: tag.to_string(),
        desc: desc.to_string(),
    }
}

/// This program's own format; see the files under `channels/`.
fn native(text: &str, name: &str) -> Result<Plan, String> {
    let mut plan = Plan::default();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |what: &str| format!("{name}:{}: {what}", lineno + 1);
        let mhz = |s: &str| s.parse::<f64>().map(|f| f * 1e6).map_err(|_| bad("bad frequency"));
        let cols: Vec<&str> = line.split(',').map(str::trim).collect();
        // Descriptions may contain commas: they are whatever is left.
        let text_cols = |from: usize| {
            let rest: Vec<&str> = line.splitn(from + 2, ',').map(str::trim).collect();
            (
                rest.get(from).copied().unwrap_or(""),
                rest.get(from + 1).copied().unwrap_or(""),
            )
        };
        match cols[0] {
            "p25" if cols.len() == 2 => plan.p25.push(P25Frequency {
                system: 0,
                freq_hz: mhz(cols[1])?,
            }),
            "tg" if cols.len() >= 3 => {
                let (tag, desc) = text_cols(2);
                let id = cols[1].parse().map_err(|_| bad("bad talkgroup"))?;
                plan.entries.push(entry(Kind::Talkgroup { system: 0, id }, tag, desc));
            }
            _ if cols.len() >= 4 => {
                let (tag, desc) = text_cols(3);
                if cols[2] == "p25" {
                    let nac = match cols[1] {
                        "" | "-" => None,
                        n => Some(u16::from_str_radix(n, 16).map_err(|_| bad("bad NAC (three hex digits)"))?),
                    };
                    let freq_hz = mhz(cols[0])?;
                    plan.entries.push(entry(Kind::Digital { freq_hz, nac }, tag, desc));
                    continue;
                }
                let tone_hz = match cols[1] {
                    "" | "-" => None,
                    t => Some(t.parse().map_err(|_| bad("bad CTCSS tone"))?),
                };
                let narrow = match cols[2] {
                    "n" => true,
                    "w" => false,
                    _ => return Err(bad("width must be w (wide FM), n (narrow FM) or p25")),
                };
                plan.entries.push(entry(
                    Kind::Analog {
                        freq_hz: mhz(cols[0])?,
                        tone_hz,
                        narrow,
                    },
                    tag,
                    desc,
                ));
            }
            _ => {
                return Err(bad(
                    "expected `freq_mhz, ctcss_hz, w|n, tag[, description]`, `freq_mhz, nac, p25, tag[, description]`, `p25, freq_mhz` or `tg, id, tag[, description]`",
                ));
            }
        }
    }
    Ok(plan)
}

/// Split one CSV line, honouring double-quoted fields.
fn csv_fields(line: &str) -> Vec<String> {
    let (mut fields, mut field, mut quoted) = (Vec::new(), String::new(), false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => fields.push(std::mem::take(&mut field).trim().to_string()),
            _ => field.push(c),
        }
    }
    fields.push(field.trim().to_string());
    fields
}

/// Rows of a CSV with a header line, each with a by-column-name lookup.
fn csv_rows(text: &str) -> impl Iterator<Item = (usize, impl Fn(&str) -> String)> + '_ {
    let mut lines = text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty());
    let header = lines.next().map(|(_, l)| csv_fields(l)).unwrap_or_default();
    lines.map(move |(lineno, line)| {
        let (header, fields) = (header.clone(), csv_fields(line));
        let get = move |col: &str| {
            let i = header.iter().position(|h| h.eq_ignore_ascii_case(col));
            i.and_then(|i| fields.get(i)).cloned().unwrap_or_default()
        };
        (lineno + 1, get)
    })
}

pub(crate) fn count_note(notes: &mut Vec<String>, count: usize, what: &str) {
    if count > 0 {
        notes.push(format!("{count} {what}"));
    }
}

/// The kind of channel a RadioReference conventional frequency is, from its
/// mode ("FM", "FMN", "P25"...) and tone ("114.8 PL", "CSQ", "023 DPL",
/// "293 NAC" or nothing), and whether it uses a digital squelch code this
/// scanner can't check. `None` for modes that can't be received.
pub(crate) fn conventional_kind(mode: &str, tone: &str, freq_hz: f64) -> Option<(Kind, bool)> {
    let (mode, tone) = (mode.trim().to_ascii_uppercase(), tone.trim().to_ascii_uppercase());
    match mode.as_str() {
        "P25" => {
            let nac = tone.strip_suffix(" NAC").and_then(|n| u16::from_str_radix(n, 16).ok());
            Some((Kind::Digital { freq_hz, nac }, false))
        }
        "FM" | "FMN" => {
            let (tone_hz, coded) = match tone.split_once(' ') {
                Some((hz, "PL")) => (hz.parse().ok(), false),
                Some(_) => (None, true),
                None => (None, false),
            };
            let narrow = mode == "FMN";
            Some((
                Kind::Analog {
                    freq_hz,
                    tone_hz,
                    narrow,
                },
                coded,
            ))
        }
        _ => None,
    }
}

/// RadioReference conventional frequency export.
fn radioreference_conventional(text: &str, name: &str) -> Result<(Plan, Vec<String>), String> {
    let (mut plan, mut other_modes, mut coded) = (Plan::default(), 0, 0);
    for (lineno, get) in csv_rows(text) {
        let freq = get("Frequency Output");
        let freq_hz = freq
            .parse::<f64>()
            .map_err(|_| format!("{name}:{lineno}: bad frequency {freq:?}"))?
            * 1e6;
        let Some((kind, squelch_coded)) = conventional_kind(&get("Mode"), &get("PL Output Tone"), freq_hz) else {
            other_modes += 1;
            continue;
        };
        coded += squelch_coded as usize;
        let (tag, desc) = (get("Alpha Tag"), get("Description"));
        let tag = if tag.is_empty() { desc.clone() } else { tag };
        plan.entries.push(entry(kind, &tag, &desc));
    }
    let mut notes = Vec::new();
    count_note(
        &mut notes,
        other_modes,
        "channels left out: not analog FM or unencrypted P25",
    );
    count_note(
        &mut notes,
        coded,
        "channels use a digital squelch code; they will open on any signal",
    );
    Ok((plan, notes))
}

/// RadioReference trunked talkgroup export.
fn radioreference_talkgroups(text: &str, name: &str) -> Result<(Plan, Vec<String>), String> {
    let (mut plan, mut encrypted, mut other_modes) = (Plan::default(), 0, 0);
    for (lineno, get) in csv_rows(text) {
        // D is digital; a trailing E or e means always or sometimes encrypted.
        match get("Mode").as_str() {
            "D" | "De" => {}
            "DE" => {
                encrypted += 1;
                continue;
            }
            _ => {
                other_modes += 1;
                continue;
            }
        }
        let decimal = get("Decimal");
        let id = decimal
            .parse()
            .map_err(|_| format!("{name}:{lineno}: bad talkgroup {decimal:?}"))?;
        plan.entries.push(entry(
            Kind::Talkgroup { system: 0, id },
            &get("Alpha Tag"),
            &get("Description"),
        ));
    }
    let mut notes = Vec::new();
    count_note(&mut notes, encrypted, "talkgroups left out: encrypted");
    count_note(&mut notes, other_modes, "talkgroups left out: not P25 Phase 1 voice");
    Ok((plan, notes))
}

/// RadioReference trunked site export: every frequency of every site.
fn radioreference_sites(text: &str, name: &str) -> Result<Vec<P25Frequency>, String> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let header = lines.next().map(csv_fields).unwrap_or_default();
    let first = header
        .iter()
        .position(|h| h.eq_ignore_ascii_case("Frequencies"))
        .ok_or(format!("{name}: not a site export (no Frequencies column)"))?;
    let mut freqs: Vec<f64> = Vec::new();
    for line in lines {
        for field in csv_fields(line).iter().skip(first) {
            // Control channels are marked with a trailing letter.
            let mhz = field.trim_end_matches(|c: char| c.is_ascii_alphabetic());
            if let Ok(mhz) = mhz.parse::<f64>() {
                let hz = (mhz * 1e6).round();
                if !freqs.contains(&hz) {
                    freqs.push(hz);
                }
            }
        }
    }
    if freqs.is_empty() {
        return Err(format!("{name}: no site frequencies found"));
    }
    Ok(freqs
        .into_iter()
        .map(|freq_hz| P25Frequency { system: 0, freq_hz })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_radioreference_conventional() {
        let csv = "Frequency Output,Frequency Input,FCC Callsign,Agency/Category,Description,Alpha Tag,PL Output Tone,PL Input Tone,Mode,Class Station Code,Tag\n\
                   488.31250,491.31250,WIL123,San Mateo PD,\"Dispatch, primary\",SMPD 1,114.8 PL,114.8 PL,FM,RM,Law Dispatch\n\
                   154.34000,,,Fire,Coast,Coast Fire,CSQ,,FMN,RM,Fire Dispatch\n\
                   460.02500,,,Sheriff,Car to car,SO C2C,023 DPL,,FMN,M,Law Tac\n\
                   482.88750,,,San Mateo PD,Channel 4,SMPD 4,9EE NAC,,P25E,RM,Law Tac\n\
                   453.10000,,,Example PD,Digital,EX PD 3,293 NAC,,P25,RM,Law Tac\n";
        let (plan, notes) = Plan::parse(csv, "test", None).unwrap();
        assert_eq!(plan.entries.len(), 4);
        assert!(matches!(plan.entries[3].kind, Kind::Digital { nac: Some(0x293), .. }));
        assert!(
            matches!(plan.entries[0].kind, Kind::Analog { freq_hz, tone_hz: Some(t), narrow: false } if freq_hz == 488.3125e6 && t == 114.8)
        );
        assert_eq!(plan.entries[0].desc, "Dispatch, primary");
        assert!(matches!(
            plan.entries[1].kind,
            Kind::Analog {
                tone_hz: None,
                narrow: true,
                ..
            }
        ));
        assert!(matches!(plan.entries[2].kind, Kind::Analog { tone_hz: None, .. }));
        assert_eq!(notes.len(), 2);
    }

    #[test]
    fn groups_frequencies_into_bands() {
        let text = "154.1, 114.8, n, Fire, Fire\n488.3125, 114.8, w, PD 1, PD\n482.5, -, w, PD 2, PD\n\
                    453.1, 293, p25, PD 3, Digital\np25, 772.03125\np25, 773.48125\ntg, 865, SO, Sheriff\n";
        let (plan, _) = Plan::parse(text, "test", None).unwrap();
        assert!(matches!(plan.entries[3].kind, Kind::Digital { nac: Some(0x293), .. }));
        let bands = plan.bands(9e6);
        let rows: Vec<Vec<Listener>> = bands
            .iter()
            .map(|b| b.listeners.iter().map(|l| l.1).collect())
            .collect();
        assert_eq!(
            rows,
            [
                vec![Listener::Row(0)],
                vec![Listener::Row(3)],
                vec![Listener::Row(2), Listener::Row(1)],
                vec![Listener::Trunked { system: 0 }; 2],
            ]
        );
        // P25 frequencies sit half a bin off the grid; the tuning follows.
        assert_eq!(bands[3].center_hz, 772_756_750.0);
        assert_eq!(plan.bands(400e6).len(), 2);
    }

    #[test]
    fn reads_radioreference_trunked() {
        let talkgroups = "Decimal,Hex,Alpha Tag,Mode,Description,Tag,Category\n\
                          865,361,SO Pat 1,D,Patrol Primary 1,Law Dispatch,Sheriff\n\
                          849,351,SO NTF 1,DE,Narcotics 1,Law Tac,Sheriff\n";
        let sites = "RFSS,Site Dec,Site Hex,Site NAC,Description,County Name,Lat,Lon,Range,Frequencies\n\
                     1,13,D,38D,Countywide Simulcast,San Mateo,37.5,-122.3,20,772.03125c,772.40625c,770.03125\n\
                     1,9,9,389,Brisbane,San Mateo,37.6,-122.4,10,772.03125c,771.05625\n";
        assert!(Plan::parse(talkgroups, "tg", None).is_err());
        let (plan, notes) = Plan::parse(talkgroups, "tg", Some((sites, "sites"))).unwrap();
        assert_eq!(plan.p25.len(), 4);
        assert!(matches!(plan.entries[0].kind, Kind::Talkgroup { id: 865, .. }));
        assert!(matches!(plan.entries[1].kind, Kind::OtherTalkgroups { .. }));
        assert_eq!(notes, ["1 talkgroups left out: encrypted"]);
    }
}
