//! The channel database: every system the scanner knows about, kept in a
//! SQLite file in the user's data directory.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::plan::{Entry, Kind, P25Frequency, Plan};

/// Systems a new database starts out with, as (name, location, channel file).
const STARTER_SYSTEMS: &[(&str, &str, &str)] = &[
    (
        "Police",
        "San Mateo County, CA",
        include_str!("../channels/san-mateo-police.csv"),
    ),
    (
        "Fire",
        "San Mateo County, CA",
        include_str!("../channels/san-mateo-fire.csv"),
    ),
    (
        "County P25",
        "San Mateo County, CA",
        include_str!("../channels/san-mateo-p25.csv"),
    ),
];

const SCHEMA: &str = "
    CREATE TABLE systems (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        location TEXT NOT NULL
    );
    -- A channel is an analog frequency or a P25 talkgroup, never both.
    CREATE TABLE channels (
        id INTEGER PRIMARY KEY,
        system_id INTEGER NOT NULL REFERENCES systems(id) ON DELETE CASCADE,
        position INTEGER NOT NULL,
        freq_hz REAL,
        tone_hz REAL,
        narrow INTEGER NOT NULL DEFAULT 0,
        talkgroup INTEGER,
        tag TEXT NOT NULL,
        description TEXT NOT NULL,
        skip INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE p25_frequencies (
        system_id INTEGER NOT NULL REFERENCES systems(id) ON DELETE CASCADE,
        freq_hz REAL NOT NULL
    );
    PRAGMA user_version = 1;
";

const DATA_DIR: &str = "airspy-scanner";
const DB_FILE: &str = "channels.db";

/// A group of channels that are scanned together: an agency's conventional
/// channels, or one trunked radio system.
#[derive(Clone)]
pub struct System {
    pub id: i64,
    pub name: String,
    pub location: String,
    pub channels: usize,
    /// Lowest and highest frequency, in Hz.
    pub lo_hz: f64,
    pub hi_hz: f64,
}

pub struct Db {
    conn: Connection,
}

fn sql<T>(result: rusqlite::Result<T>) -> Result<T, String> {
    result.map_err(|e| format!("channel database: {e}"))
}

impl Db {
    /// Where the database lives unless told otherwise: in an
    /// `airspy-scanner` directory beside the program if there is one (a
    /// portable install), otherwise in the platform's per-user application
    /// data directory.
    pub fn default_path() -> PathBuf {
        let beside_program = std::env::current_exe()
            .ok()
            .and_then(|exe| Some(exe.parent()?.join(DATA_DIR)));
        if let Some(dir) = beside_program.filter(|dir| dir.is_dir()) {
            return dir.join(DB_FILE);
        }
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = || var("HOME").unwrap_or_default();
        let data = if cfg!(windows) {
            var("APPDATA").unwrap_or_default()
        } else if cfg!(target_os = "macos") {
            home().join("Library/Application Support")
        } else {
            var("XDG_DATA_HOME").unwrap_or_else(|| home().join(".local/share"))
        };
        data.join(DATA_DIR).join(DB_FILE)
    }

    /// Open the database at `path`, creating it with the starter systems if
    /// it doesn't exist yet.
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let conn = Connection::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut db = Self { conn };
        sql(db.conn.execute_batch("PRAGMA foreign_keys = ON;"))?;
        let version: i64 = sql(db.conn.query_row("PRAGMA user_version", [], |r| r.get(0)))?;
        if version == 0 {
            sql(db.conn.execute_batch(SCHEMA))?;
            for (name, location, csv) in STARTER_SYSTEMS {
                let (plan, _) = Plan::parse(csv, name, None)?;
                db.add_system(name, location, &plan)?;
            }
        }
        Ok(db)
    }

    /// All systems, grouped by location.
    pub fn systems(&self) -> Result<Vec<System>, String> {
        let mut stmt = sql(self.conn.prepare(
            "SELECT s.id, s.name, s.location,
                    (SELECT COUNT(*) FROM channels c WHERE c.system_id = s.id),
                    (SELECT MIN(f) FROM (SELECT freq_hz AS f FROM channels WHERE system_id = s.id
                                         UNION ALL SELECT freq_hz FROM p25_frequencies WHERE system_id = s.id)),
                    (SELECT MAX(f) FROM (SELECT freq_hz AS f FROM channels WHERE system_id = s.id
                                         UNION ALL SELECT freq_hz FROM p25_frequencies WHERE system_id = s.id))
             FROM systems s ORDER BY s.location, s.id",
        ))?;
        let rows = stmt.query_map([], |r| {
            Ok(System {
                id: r.get(0)?,
                name: r.get(1)?,
                location: r.get(2)?,
                channels: r.get::<_, i64>(3)? as usize,
                lo_hz: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
                hi_hz: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
            })
        });
        sql(sql(rows)?.collect())
    }

    /// Find a system by number, or by a name (or part of one) that only one
    /// system has. Case doesn't matter.
    pub fn find(&self, what: &str) -> Result<System, String> {
        let systems = self.systems()?;
        if let Some(system) = what
            .parse()
            .ok()
            .and_then(|id: i64| systems.iter().find(|s| s.id == id))
        {
            return Ok(system.clone());
        }
        let lower = what.to_lowercase();
        let exact: Vec<&System> = systems.iter().filter(|s| s.name.to_lowercase() == lower).collect();
        let partial: Vec<&System> = systems
            .iter()
            .filter(|s| s.name.to_lowercase().contains(&lower))
            .collect();
        match (exact.as_slice(), partial.as_slice()) {
            ([system], _) | ([], [system]) => Ok((*system).clone()),
            ([], []) => Err(format!("no system called {what:?}; `scanner systems` lists them")),
            _ => Err(format!(
                "{what:?} matches several systems; use its number from `scanner systems`"
            )),
        }
    }

    /// Store a plan's channels as a new system; returns the system's id.
    pub fn add_system(&mut self, name: &str, location: &str, plan: &Plan) -> Result<i64, String> {
        let tx = sql(self.conn.transaction())?;
        sql(tx.execute(
            "INSERT INTO systems (name, location) VALUES (?1, ?2)",
            params![name, location],
        ))?;
        let system = tx.last_insert_rowid();
        for (position, e) in plan.entries.iter().enumerate() {
            let (freq_hz, tone_hz, narrow, talkgroup) = match e.kind {
                Kind::Analog {
                    freq_hz,
                    tone_hz,
                    narrow,
                } => (Some(freq_hz), tone_hz, narrow, None),
                Kind::Talkgroup { id, .. } => (None, None, false, Some(id)),
                Kind::OtherTalkgroups { .. } => continue,
            };
            sql(tx.execute(
                "INSERT INTO channels (system_id, position, freq_hz, tone_hz, narrow, talkgroup, tag, description, skip)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![system, position as i64, freq_hz, tone_hz, narrow, talkgroup, e.tag, e.desc, e.skip],
            ))?;
        }
        for f in &plan.p25 {
            sql(tx.execute(
                "INSERT INTO p25_frequencies (system_id, freq_hz) VALUES (?1, ?2)",
                params![system, f.freq_hz],
            ))?;
        }
        sql(tx.commit())?;
        Ok(system)
    }

    pub fn remove_system(&mut self, system: i64) -> Result<(), String> {
        sql(self.conn.execute("DELETE FROM systems WHERE id = ?1", [system])).map(|_| ())
    }

    /// Remember whether a channel is to be ignored.
    pub fn set_skip(&self, channel: i64, skip: bool) -> Result<(), String> {
        sql(self
            .conn
            .execute("UPDATE channels SET skip = ?2 WHERE id = ?1", params![channel, skip]))
        .map(|_| ())
    }

    /// The channels of the given systems as one plan, in the order given.
    pub fn plan(&self, systems: &[i64]) -> Result<Plan, String> {
        let mut plan = Plan::default();
        for &system in systems {
            let exists: Option<i64> = sql(self
                .conn
                .query_row("SELECT id FROM systems WHERE id = ?1", [system], |r| r.get(0))
                .optional())?;
            if exists.is_none() {
                return Err(format!("system {system} is no longer in the database"));
            }
            let mut stmt = sql(self.conn.prepare(
                "SELECT id, freq_hz, tone_hz, narrow, talkgroup, tag, description, skip
                 FROM channels WHERE system_id = ?1 ORDER BY position",
            ))?;
            let rows = stmt.query_map([system], |r| {
                let kind = match (r.get::<_, Option<f64>>(1)?, r.get::<_, Option<u16>>(4)?) {
                    (Some(freq_hz), _) => Kind::Analog {
                        freq_hz,
                        tone_hz: r.get(2)?,
                        narrow: r.get(3)?,
                    },
                    (None, id) => Kind::Talkgroup {
                        system,
                        id: id.unwrap_or(0),
                    },
                };
                Ok(Entry {
                    channel_id: Some(r.get(0)?),
                    skip: r.get(7)?,
                    kind,
                    tag: r.get(5)?,
                    desc: r.get(6)?,
                })
            });
            for entry in sql(rows)? {
                plan.entries.push(sql(entry)?);
            }
            let mut stmt = sql(self
                .conn
                .prepare("SELECT freq_hz FROM p25_frequencies WHERE system_id = ?1"))?;
            for freq_hz in sql(stmt.query_map([system], |r| r.get(0)))? {
                plan.p25.push(P25Frequency {
                    system,
                    freq_hz: sql(freq_hz)?,
                });
            }
        }
        plan.add_other_talkgroups();
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_systems_round_trip() {
        let dir = std::env::temp_dir().join(format!("scanner-db-test-{}", std::process::id()));
        let path = dir.join("channels.db");
        let db = Db::open(&path).unwrap();
        let systems = db.systems().unwrap();
        assert_eq!(
            systems.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["Police", "Fire", "County P25"]
        );
        assert_eq!(db.find("police").unwrap().id, systems[0].id);
        assert_eq!(db.find("p25").unwrap().id, systems[2].id);
        assert!(db.find("nowhere").is_err());

        // Two systems in one plan keep their own talkgroups apart.
        let plan = db.plan(&[systems[2].id, systems[2].id]).unwrap();
        assert_eq!(plan.p25.len(), 54);
        let plan = db.plan(&[systems[0].id]).unwrap();
        assert_eq!(plan.entries.len(), 33);
        let first = plan.entries[0].channel_id.unwrap();
        db.set_skip(first, true).unwrap();
        drop(db);

        // Skips persist; reopening doesn't seed a second time.
        let mut db = Db::open(&path).unwrap();
        assert_eq!(db.systems().unwrap().len(), 3);
        assert!(db.plan(&[systems[0].id]).unwrap().entries[0].skip);
        db.remove_system(systems[0].id).unwrap();
        assert_eq!(db.systems().unwrap().len(), 2);
        assert!(db.plan(&[systems[0].id]).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
