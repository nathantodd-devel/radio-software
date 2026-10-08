//! Command-line front end for the scanner.

use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;

use chrono::Local;
use scanner::db::Db;
use scanner::engine::{self, Config, Controls, Event, Source};
use scanner::plan::{Kind, Plan};

const USAGE: &str = "\
usage: scanner [options] SYSTEM...     scan one or more systems
       scanner systems                 list the systems in the channel database
       scanner channels SYSTEM...      list the channels of systems
       scanner import FILE [--name NAME] [--location PLACE] [--sites FILE]
       scanner remove SYSTEM

SYSTEM is a system's number or name (or part of it) from `scanner systems`,
or the path of a channel file to scan without importing it. Systems scanned
together must fit in one tuning of the Airspy (about 9 MHz on an Airspy R2).

`import` reads this program's own CSV format or a RadioReference CSV export:
a conventional frequency list, or a trunked system's talkgroup list together
with its site list (--sites) for P25 Phase 1 systems.

  -g, --gain N       Airspy linearity gain 0-21 (default 17)
  -s, --squelch DB   carrier level over the noise floor to open (default 6)
      --hold SECS    stay on a channel this long after it goes quiet (default 1.5)
      --volume X     audio gain (default 3)
      --record DIR   save every transmission on every channel as a WAV file
      --rate HZ      Airspy sample rate (default: the fastest it offers)
      --stdin        read 16-bit I/Q from stdin instead of an Airspy
      --no-audio     don't play audio (useful with --record)
      --db FILE      channel database (default: airspy-scanner/channels.db beside
                     this program if that folder exists, otherwise in your
                     user data directory; ~/.local/share on Linux)";

struct Options {
    words: Vec<String>,
    squelch_db: f32,
    volume: f32,
    db: PathBuf,
    name: Option<String>,
    location: Option<String>,
    sites: Option<PathBuf>,
    config: Config,
}

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("scanner: {msg}");
    exit(1)
}

fn parse_args() -> Options {
    let mut o = Options {
        words: Vec::new(),
        squelch_db: 6.0,
        volume: 3.0,
        db: Db::default_path(),
        name: None,
        location: None,
        sites: None,
        config: Config::default(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| die(format!("{a} needs a value")));
        fn num<T: std::str::FromStr>(flag: &str, v: String) -> T {
            v.parse().unwrap_or_else(|_| die(format!("bad value for {flag}: {v}")))
        }
        match a.as_str() {
            "-g" | "--gain" => o.config.gain = num(&a, value()),
            "-s" | "--squelch" => o.squelch_db = num(&a, value()),
            "--hold" => o.config.hold_secs = num(&a, value()),
            "--volume" => o.volume = num(&a, value()),
            "--record" => o.config.record = Some(PathBuf::from(value())),
            "--rate" => o.config.rate = Some(num(&a, value())),
            "--stdin" => o.config.source = Source::Stdin,
            "--no-audio" => o.config.audio = false,
            "--db" => o.db = PathBuf::from(value()),
            "--name" => o.name = Some(value()),
            "--location" => o.location = Some(value()),
            "--sites" => o.sites = Some(PathBuf::from(value())),
            "-h" | "--help" => {
                println!("{USAGE}");
                exit(0)
            }
            _ if a.starts_with('-') => die(format!("unknown option {a}\n{USAGE}")),
            _ => o.words.push(a),
        }
    }
    o
}

fn list_systems(db: &Db) {
    let mut location = None;
    for s in db.systems().unwrap_or_else(|e| die(e)) {
        if location.as_ref() != Some(&s.location) {
            println!(
                "{}",
                if s.location.is_empty() {
                    "(no location)"
                } else {
                    &s.location
                }
            );
            location = Some(s.location.clone());
        }
        println!(
            "  {:>3}  {:<28} {:>4} channels  {:.3}-{:.3} MHz",
            s.id,
            s.name,
            s.channels,
            s.lo_hz / 1e6,
            s.hi_hz / 1e6
        );
    }
}

/// The plan for the systems (or one channel file) named on the command line.
fn load_plan(db: &Db, names: &[String]) -> Plan {
    if let [name] = names
        && Path::new(name).is_file()
    {
        let (plan, notes) = Plan::from_file(Path::new(name), None).unwrap_or_else(|e| die(e));
        notes.iter().for_each(|n| eprintln!("scanner: {name}: {n}"));
        return plan;
    }
    let ids: Vec<i64> = names.iter().map(|n| db.find(n).unwrap_or_else(|e| die(e)).id).collect();
    db.plan(&ids).unwrap_or_else(|e| die(e))
}

fn list_channels(plan: &Plan) {
    for e in &plan.entries {
        let tone = match e.kind {
            Kind::Analog { tone_hz: Some(t), .. } => format!("{t:5.1}"),
            Kind::Analog { tone_hz: None, .. } => "  CSQ".into(),
            _ => "  P25".into(),
        };
        println!(
            "{:>9}  {tone}  {:<17} {}{}",
            e.id(),
            e.tag,
            e.desc,
            if e.skip { "  (skipped)" } else { "" }
        );
    }
}

fn import(db: &mut Db, o: &Options, file: &str) {
    let path = Path::new(file);
    let (plan, notes) = Plan::from_file(path, o.sites.as_deref()).unwrap_or_else(|e| die(e));
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = o.name.clone().unwrap_or(stem);
    let id = db
        .add_system(&name, o.location.as_deref().unwrap_or(""), &plan)
        .unwrap_or_else(|e| die(e));
    let (lo, hi, _) = plan.span();
    println!(
        "Added system {id}, {name:?}: {} channels, {:.3}-{:.3} MHz",
        plan.entries.len(),
        lo / 1e6,
        hi / 1e6
    );
    notes.iter().for_each(|n| println!("  note: {n}"));
}

fn scan(o: &Options, plan: Plan) {
    let (lo, hi, center_hz) = plan.span();
    let controls = Arc::new(Controls::new(&plan, o.volume, o.squelch_db));
    {
        // Stop cleanly, so the Airspy is released and recordings are closed.
        let controls = controls.clone();
        ctrlc::set_handler(move || controls.stop()).ok();
    }
    eprintln!(
        "Monitoring {} channels, {:.4}-{:.4} MHz (tuned to {:.5} MHz). Ctrl-C to quit.",
        plan.entries.len(),
        lo / 1e6,
        hi / 1e6,
        center_hz / 1e6
    );
    let result = engine::run(&plan, &o.config, &controls, |event| {
        if let Event::Opened {
            channel,
            snr_db,
            playing,
            talkgroup,
        } = event
        {
            let e = &plan.entries[channel];
            println!(
                "{} {} {:>9}  {:<17} {:<40} {:+3.0} dB",
                Local::now().format("%H:%M:%S"),
                if playing { '>' } else { ' ' },
                talkgroup.map_or(e.id(), |tg| format!("TG {tg}")),
                e.tag,
                e.desc,
                snr_db
            );
        }
    });
    if let Err(e) = result {
        die(e);
    }
}

fn main() {
    let o = parse_args();
    let mut db = Db::open(&o.db).unwrap_or_else(|e| die(e));
    let words: Vec<&str> = o.words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["systems"] => list_systems(&db),
        ["channels", _, ..] => list_channels(&load_plan(&db, &o.words[1..])),
        ["import", file] => import(&mut db, &o, file),
        ["remove", system] => {
            let system = db.find(system).unwrap_or_else(|e| die(e));
            db.remove_system(system.id).unwrap_or_else(|e| die(e));
            println!("Removed system {}, {:?}", system.id, system.name);
        }
        ["channels" | "import" | "remove", ..] => die(format!("wrong arguments\n{USAGE}")),
        [] => {
            eprintln!("scanner: which system? These are in the channel database:\n");
            list_systems(&db);
            eprintln!("\nFor example: scanner 1    (scanner --help for more)");
            exit(1)
        }
        _ => scan(&o, load_plan(&db, &o.words)),
    }
}
