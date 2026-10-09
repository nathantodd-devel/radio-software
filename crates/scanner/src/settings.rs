//! The choices a front end remembers between runs, kept in the channel
//! database so every front end sees the same ones.

use std::str::FromStr;

use crate::db::Db;
use crate::engine::{Config, ScanMode, Source};

/// The receiver setting that means "whichever is found first".
pub const AUTO_DEVICE: &str = "auto";

pub struct Settings {
    pub volume: f32,
    pub squelch_db: f32,
    /// Which kind of receiver to use: a driver's id, or [`AUTO_DEVICE`].
    pub device: String,
    /// Receiver gain, 0-21.
    pub gain: u8,
    /// Frequency correction for an RTL-SDR, in parts per million.
    pub ppm: i32,
    /// Power an antenna amplifier through the coax.
    pub bias_tee: bool,
    /// How the channels are covered.
    pub scan: ScanMode,
    /// Sample rate, or 0 for the fastest the receiver offers.
    pub sample_rate: u32,
    /// Seconds on a quiet band before moving on.
    pub dwell_secs: f32,
    /// Longest turn, in seconds, for a band that stays busy.
    pub max_stay_secs: f32,
    /// Keep recordings after the program closes.
    pub save_recordings: bool,
    /// Name of the desktop app's colour theme.
    pub theme: String,
    /// The systems being scanned together, in the order they were picked.
    pub selected: Vec<i64>,
}

impl Settings {
    /// The receiver to look for.
    pub fn source(&self) -> Source {
        match self.device.as_str() {
            AUTO_DEVICE => Source::Auto,
            id => Source::Kind(id.to_string()),
        }
    }

    /// The saved settings, or the defaults for any never saved (all of
    /// them, without a database).
    pub fn load(db: Option<&Db>) -> Self {
        fn get<T: FromStr>(db: Option<&Db>, key: &str, default: T) -> T {
            db.and_then(|db| db.setting(key))
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        }
        let defaults = Config::default();
        Self {
            volume: get(db, "volume", 3.0),
            squelch_db: get(db, "squelch_db", 6.0),
            device: get(db, "device", AUTO_DEVICE.to_string()),
            gain: get(db, "gain", defaults.gain),
            ppm: get(db, "ppm", 0),
            bias_tee: get(db, "bias_tee", false),
            scan: ScanMode::from_id(&get(db, "scan_mode", String::new())).unwrap_or(defaults.scan),
            sample_rate: get(db, "sample_rate", 0),
            dwell_secs: get(db, "dwell_secs", defaults.dwell_secs),
            max_stay_secs: get(db, "max_stay_secs", defaults.max_stay_secs),
            save_recordings: get(db, "save_recordings", false),
            theme: get(db, "theme", "Default".to_string()),
            selected: get(db, "selected", String::new())
                .split(',')
                .filter_map(|id| id.parse().ok())
                .collect(),
        }
    }

    pub fn save(&self, db: &Db) {
        let selected: Vec<String> = self.selected.iter().map(i64::to_string).collect();
        let values = [
            ("volume", self.volume.to_string()),
            ("squelch_db", self.squelch_db.to_string()),
            ("device", self.device.clone()),
            ("gain", self.gain.to_string()),
            ("ppm", self.ppm.to_string()),
            ("bias_tee", self.bias_tee.to_string()),
            ("scan_mode", self.scan.id().to_string()),
            ("sample_rate", self.sample_rate.to_string()),
            ("dwell_secs", self.dwell_secs.to_string()),
            ("max_stay_secs", self.max_stay_secs.to_string()),
            ("save_recordings", self.save_recordings.to_string()),
            ("theme", self.theme.clone()),
            ("selected", selected.join(",")),
        ];
        for (key, value) in values {
            db.set_setting(key, &value).ok();
        }
    }
}
