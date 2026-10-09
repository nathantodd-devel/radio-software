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

/// Each step brings a database up one version; `PRAGMA user_version`
/// counts how many have been applied.
const MIGRATIONS: &[&str] = &[
    "CREATE TABLE systems (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        location TEXT NOT NULL
    );
    -- A channel is a frequency (analog, or P25 if `p25` is set) or a
    -- talkgroup on its system's P25 frequencies, never both.
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
    );",
    "ALTER TABLE channels ADD COLUMN p25 INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE channels ADD COLUMN nac INTEGER;
    ALTER TABLE channels ADD COLUMN priority INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );",
    "ALTER TABLE channels ADD COLUMN record INTEGER NOT NULL DEFAULT 0;",
];

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

/// How a kind of channel is stored: frequency, tone, narrow, talkgroup,
/// whether it is P25, and NAC. `None` for the catch-all entry, which isn't.
#[allow(clippy::type_complexity)]
fn columns(kind: &Kind) -> Option<(Option<f64>, Option<f32>, bool, Option<u16>, bool, Option<u16>)> {
    match *kind {
        Kind::Analog {
            freq_hz,
            tone_hz,
            narrow,
        } => Some((Some(freq_hz), tone_hz, narrow, None, false, None)),
        Kind::Digital { freq_hz, nac } => Some((Some(freq_hz), None, false, None, true, nac)),
        Kind::Talkgroup { id, .. } => Some((None, None, false, Some(id), true, None)),
        Kind::OtherTalkgroups { .. } => None,
    }
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
        let version: usize = sql(db.conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0)))? as usize;
        if version > MIGRATIONS.len() {
            return Err(format!("{}: made by a newer version of this program", path.display()));
        }
        for (applied, migration) in MIGRATIONS.iter().enumerate().skip(version) {
            sql(db.conn.execute_batch(&format!(
                "BEGIN; {migration}; PRAGMA user_version = {}; COMMIT;",
                applied + 1
            )))?;
        }
        if version == 0 {
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
            let Some((freq_hz, tone_hz, narrow, talkgroup, p25, nac)) = columns(&e.kind) else {
                continue;
            };
            sql(tx.execute(
                "INSERT INTO channels
                    (system_id, position, freq_hz, tone_hz, narrow, talkgroup, p25, nac, tag, description,
                     skip, priority, record)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    system,
                    position as i64,
                    freq_hz,
                    tone_hz,
                    narrow,
                    talkgroup,
                    p25,
                    nac,
                    e.tag,
                    e.desc,
                    e.skip,
                    e.priority,
                    e.record
                ],
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

    pub fn rename_system(&self, system: i64, name: &str, location: &str) -> Result<(), String> {
        sql(self.conn.execute(
            "UPDATE systems SET name = ?2, location = ?3 WHERE id = ?1",
            params![system, name, location],
        ))
        .map(|_| ())
    }

    /// The frequencies, in Hz, a system's P25 talkgroups are heard on.
    pub fn p25_frequencies(&self, system: i64) -> Result<Vec<f64>, String> {
        let mut stmt = sql(self
            .conn
            .prepare("SELECT freq_hz FROM p25_frequencies WHERE system_id = ?1 ORDER BY freq_hz"))?;
        sql(sql(stmt.query_map([system], |r| r.get(0)))?.collect())
    }

    pub fn set_p25_frequencies(&mut self, system: i64, freqs_hz: &[f64]) -> Result<(), String> {
        let tx = sql(self.conn.transaction())?;
        sql(tx.execute("DELETE FROM p25_frequencies WHERE system_id = ?1", [system]))?;
        for freq_hz in freqs_hz {
            sql(tx.execute(
                "INSERT INTO p25_frequencies (system_id, freq_hz) VALUES (?1, ?2)",
                params![system, freq_hz],
            ))?;
        }
        sql(tx.commit())
    }

    /// One channel and the system it belongs to.
    pub fn channel(&self, channel: i64) -> Result<(i64, Entry), String> {
        let system: i64 = sql(self
            .conn
            .query_row("SELECT system_id FROM channels WHERE id = ?1", [channel], |r| r.get(0)))?;
        let entry = self
            .plan(&[system])?
            .entries
            .into_iter()
            .find(|e| e.channel_id == Some(channel));
        Ok((system, entry.ok_or("channel not found")?))
    }

    /// Store a channel: a new one at the end of `system` if it has no
    /// `channel_id`, otherwise the changes to the one it names. Returns the
    /// channel's id.
    pub fn save_channel(&self, system: i64, e: &Entry) -> Result<i64, String> {
        let (freq_hz, tone_hz, narrow, talkgroup, p25, nac) =
            columns(&e.kind).ok_or("not a channel that can be stored")?;
        match e.channel_id {
            Some(id) => {
                sql(self.conn.execute(
                    "UPDATE channels SET freq_hz = ?2, tone_hz = ?3, narrow = ?4, talkgroup = ?5, p25 = ?6, nac = ?7,
                                         tag = ?8, description = ?9, skip = ?10, priority = ?11, record = ?12
                     WHERE id = ?1",
                    params![
                        id, freq_hz, tone_hz, narrow, talkgroup, p25, nac, e.tag, e.desc, e.skip, e.priority, e.record
                    ],
                ))?;
                Ok(id)
            }
            None => {
                sql(self.conn.execute(
                    "INSERT INTO channels
                        (system_id, position, freq_hz, tone_hz, narrow, talkgroup, p25, nac, tag, description,
                         skip, priority, record)
                     VALUES (?1, (SELECT COALESCE(MAX(position), -1) + 1 FROM channels WHERE system_id = ?1),
                             ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        system, freq_hz, tone_hz, narrow, talkgroup, p25, nac, e.tag, e.desc, e.skip, e.priority,
                        e.record
                    ],
                ))?;
                Ok(self.conn.last_insert_rowid())
            }
        }
    }

    pub fn delete_channel(&self, channel: i64) -> Result<(), String> {
        sql(self.conn.execute("DELETE FROM channels WHERE id = ?1", [channel])).map(|_| ())
    }

    /// Swap a channel with the one before it in its system (`up`) or the
    /// one after; does nothing at the end of the list. Order is what decides
    /// which channel plays when several are active.
    pub fn move_channel(&mut self, channel: i64, up: bool) -> Result<(), String> {
        let tx = sql(self.conn.transaction())?;
        let (system, position): (i64, i64) = sql(tx.query_row(
            "SELECT system_id, position FROM channels WHERE id = ?1",
            [channel],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ))?;
        let neighbour = if up {
            "SELECT id, position FROM channels WHERE system_id = ?1 AND position < ?2 ORDER BY position DESC LIMIT 1"
        } else {
            "SELECT id, position FROM channels WHERE system_id = ?1 AND position > ?2 ORDER BY position LIMIT 1"
        };
        let other: Option<(i64, i64)> = sql(tx
            .query_row(neighbour, params![system, position], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional())?;
        if let Some((other, other_position)) = other {
            sql(tx.execute(
                "UPDATE channels SET position = ?2 WHERE id = ?1",
                params![channel, other_position],
            ))?;
            sql(tx.execute(
                "UPDATE channels SET position = ?2 WHERE id = ?1",
                params![other, position],
            ))?;
        }
        sql(tx.commit())
    }

    /// Remember whether a channel is to be ignored.
    pub fn set_skip(&self, channel: i64, skip: bool) -> Result<(), String> {
        sql(self
            .conn
            .execute("UPDATE channels SET skip = ?2 WHERE id = ?1", params![channel, skip]))
        .map(|_| ())
    }

    /// Remember whether a channel interrupts the others.
    pub fn set_priority(&self, channel: i64, priority: bool) -> Result<(), String> {
        sql(self.conn.execute(
            "UPDATE channels SET priority = ?2 WHERE id = ?1",
            params![channel, priority],
        ))
        .map(|_| ())
    }

    /// Remember whether a channel's calls are always saved. Marking a
    /// channel for recording also makes it a priority channel: one worth
    /// recording is one not to miss.
    pub fn set_record(&self, channel: i64, record: bool) -> Result<(), String> {
        sql(self.conn.execute(
            "UPDATE channels SET record = ?2, priority = priority OR ?2 WHERE id = ?1",
            params![channel, record],
        ))
        .map(|_| ())
    }

    /// A saved setting, if it has ever been set.
    pub fn setting(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .ok()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), String> {
        sql(self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        ))
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
                "SELECT id, freq_hz, tone_hz, narrow, talkgroup, tag, description, skip, p25, nac, priority, record
                 FROM channels WHERE system_id = ?1 ORDER BY position",
            ))?;
            let rows = stmt.query_map([system], |r| {
                let p25: bool = r.get(8)?;
                let kind = match (r.get::<_, Option<f64>>(1)?, r.get::<_, Option<u16>>(4)?) {
                    (Some(freq_hz), _) if p25 => Kind::Digital {
                        freq_hz,
                        nac: r.get(9)?,
                    },
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
                    priority: r.get(10)?,
                    record: r.get(11)?,
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
        let mut db = Db::open(&path).unwrap();
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

        db.set_priority(first, true).unwrap();
        assert_eq!(db.setting("volume"), None);
        db.set_setting("volume", "3").unwrap();
        db.set_setting("volume", "4.5").unwrap();
        let (digital, _) = Plan::parse("453.1, 293, p25, PD 3, Digital\n", "t", None).unwrap();
        let digital_id = db.add_system("Digital", "", &digital).unwrap();
        drop(db);

        // A database from before settings and priorities existed is upgraded.
        let old = dir.join("old.db");
        let conn = Connection::open(&old).unwrap();
        conn.execute_batch(&format!("{}; PRAGMA user_version = 1;", MIGRATIONS[0]))
            .unwrap();
        conn.execute("INSERT INTO systems (name, location) VALUES ('Old', '')", [])
            .unwrap();
        conn.execute(
            "INSERT INTO channels (system_id, position, freq_hz, tone_hz, tag, description) VALUES (1, 0, 155e6, 100.0, 'A', '')",
            [],
        )
        .unwrap();
        drop(conn);
        let upgraded = Db::open(&old).unwrap();
        let plan = upgraded.plan(&[1]).unwrap();
        assert!(matches!(plan.entries[0].kind, Kind::Analog { tone_hz: Some(_), .. }));
        assert!(!plan.entries[0].priority);
        assert_eq!(upgraded.systems().unwrap().len(), 1);
        drop(upgraded);

        // Editing: a new system, channels added, changed, reordered, removed.
        let mut db = Db::open(&path).unwrap();
        let edited = db.add_system("Mine", "Home", &Plan::default()).unwrap();
        db.rename_system(edited, "Renamed", "Away").unwrap();
        let channel = |tag: &str, kind: Kind| Entry {
            channel_id: None,
            skip: false,
            priority: false,
            record: false,
            kind,
            tag: tag.into(),
            desc: String::new(),
        };
        let fm = |freq_hz| Kind::Analog {
            freq_hz,
            tone_hz: None,
            narrow: true,
        };
        let a = db.save_channel(edited, &channel("A", fm(155.0e6))).unwrap();
        let b = db.save_channel(edited, &channel("B", fm(155.1e6))).unwrap();
        let c = db
            .save_channel(edited, &channel("C", Kind::Talkgroup { system: 0, id: 42 }))
            .unwrap();
        db.set_p25_frequencies(edited, &[771.0e6, 772.0e6]).unwrap();
        let tags =
            |db: &Db| -> Vec<String> { db.plan(&[edited]).unwrap().entries.into_iter().map(|e| e.tag).collect() };
        assert_eq!(tags(&db), ["A", "B", "C", "Other"]);
        db.move_channel(c, true).unwrap();
        db.move_channel(a, true).unwrap();
        db.move_channel(a, false).unwrap();
        // The catch-all entry follows the system's last talkgroup.
        assert_eq!(tags(&db), ["C", "Other", "A", "B"]);
        let (system, mut entry) = db.channel(b).unwrap();
        assert_eq!(system, edited);
        entry.tag = "B2".into();
        entry.kind = Kind::Digital {
            freq_hz: 453.1e6,
            nac: Some(0x293),
        };
        assert_eq!(db.save_channel(edited, &entry).unwrap(), b);
        db.delete_channel(a).unwrap();
        assert_eq!(tags(&db), ["C", "Other", "B2"]);
        assert!(matches!(
            db.channel(b).unwrap().1.kind,
            Kind::Digital { nac: Some(0x293), .. }
        ));
        assert_eq!(db.p25_frequencies(edited).unwrap(), [771.0e6, 772.0e6]);
        let renamed = db.systems().unwrap().into_iter().find(|s| s.id == edited).unwrap();
        assert_eq!(
            (renamed.name.as_str(), renamed.location.as_str(), renamed.channels),
            ("Renamed", "Away", 2)
        );
        db.remove_system(edited).unwrap();
        drop(db);

        // Skips persist; reopening doesn't seed a second time.
        let mut db = Db::open(&path).unwrap();
        assert_eq!(db.systems().unwrap().len(), 4);
        let reloaded = db.plan(&[systems[0].id]).unwrap();
        assert!(reloaded.entries[0].skip && reloaded.entries[0].priority && !reloaded.entries[1].priority);
        // Marking for recording brings priority with it; unmarking leaves it.
        let second = reloaded.entries[1].channel_id.unwrap();
        db.set_record(second, true).unwrap();
        let marked = db.plan(&[systems[0].id]).unwrap();
        assert!(marked.entries[1].record && marked.entries[1].priority && !marked.entries[2].record);
        db.set_record(second, false).unwrap();
        let unmarked = db.plan(&[systems[0].id]).unwrap();
        assert!(!unmarked.entries[1].record && unmarked.entries[1].priority);
        assert_eq!(db.setting("volume").as_deref(), Some("4.5"));
        let digital = db.plan(&[digital_id]).unwrap();
        assert!(matches!(
            digital.entries[0].kind,
            Kind::Digital { nac: Some(0x293), .. }
        ));
        db.remove_system(systems[0].id).unwrap();
        assert_eq!(db.systems().unwrap().len(), 3);
        assert!(db.plan(&[systems[0].id]).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
