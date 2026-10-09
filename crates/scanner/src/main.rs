//! Command-line front end for the scanner.

use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;

use chrono::Local;
use scanner::db::Db;
use scanner::engine::{self, Config, Controls, Event, ScanMode, Source};
use scanner::plan::{Kind, Plan};
use scanner::radioreference::{self, Credentials, Imported};

const USAGE: &str = "\
usage: scanner [options] SYSTEM...     scan one or more systems
       scanner systems                 list the systems in the channel database
       scanner channels SYSTEM...      list the channels of systems
       scanner import FILE [--name NAME] [--location PLACE] [--sites FILE]
       scanner remove SYSTEM
       scanner radioreference system ID    import a trunked system by its system ID
       scanner radioreference county ID    import a county's conventional channels

SYSTEM is a system's number or name (or part of it) from `scanner systems`,
or the path of a channel file to scan without importing it. The receiver
takes in a whole band at once: about 9 MHz on an Airspy R2 at its fastest
rate, about 1.9 MHz on an RTL-SDR. When the systems don't fit in one band
the scanner takes turns on each, staying while there is traffic; see --scan.

`import` reads this program's own CSV format or a RadioReference CSV export:
a conventional frequency list, or a trunked system's talkgroup list together
with its site list (--sites) for P25 Phase 1 systems.

`radioreference` fetches from RadioReference.com directly. It needs a premium
account and an application key from RadioReference, given in the environment
as RR_USERNAME, RR_PASSWORD and RR_APP_KEY. The IDs are the numbers in the
page addresses: radioreference.com/db/sid/ID and .../db/browse/ctid/ID.

      --device KIND  auto (an Airspy if there is one, otherwise an RTL-SDR; the
                     default), or one kind of receiver: airspy or rtlsdr
  -g, --gain N       receiver gain 0-21 (default 17): the Airspy's linearity
                     gain, or that far up an RTL-SDR tuner's range
      --ppm N        frequency correction for an RTL-SDR's crystal, in parts
                     per million (default 0)
      --bias-tee     power an antenna amplifier through the coax
  -s, --squelch DB   carrier level over the noise floor to open (default 6)
      --hold SECS    stay on a channel this long after it goes quiet (default 1.5)
      --volume X     audio gain (default 3)
      --record DIR   save every transmission on every channel as a WAV file
      --record-marked DIR
                     save the transmissions of channels marked for recording
                     (the Rec button in scanner-ui), even without --record
      --rate HZ      sample rate (default: the fastest the receiver offers); a
                     lower rate covers a narrower band with less load
      --scan MODE    hop      whole bands at once, taking turns if there are
                              several (default)
                     band     whole band at once; refuse if it doesn't fit
                     channel  one channel at a time, at the lowest rate
      --stdin        read 16-bit I/Q from stdin instead of a receiver
      --no-audio     don't play audio (useful with --record)
      --dwell SECS   time to listen to a quiet band before hopping (default 1.5)
      --stay SECS    longest turn for a band that stays busy (default 20)
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
            "--device" => {
                let kind = value();
                let known: Vec<&str> = engine::drivers().iter().map(|d| d.id()).collect();
                o.config.source = match kind.as_str() {
                    "auto" => Source::Auto,
                    id if known.contains(&id) => Source::Kind(kind),
                    _ => die(format!("--device takes auto, {}, not {kind:?}", known.join(" or "))),
                }
            }
            "-g" | "--gain" => o.config.gain = num(&a, value()),
            "--ppm" => o.config.ppm = num(&a, value()),
            "--bias-tee" => o.config.bias_tee = true,
            "-s" | "--squelch" => o.squelch_db = num(&a, value()),
            "--hold" => o.config.hold_secs = num(&a, value()),
            "--volume" => o.volume = num(&a, value()),
            "--record" => o.config.record = Some(PathBuf::from(value())),
            "--record-marked" => o.config.record_marked = Some(PathBuf::from(value())),
            "--rate" => o.config.rate = Some(num(&a, value())),
            "--stdin" => o.config.source = Source::Stdin,
            "--no-audio" => o.config.audio = false,
            "--scan" => {
                o.config.scan = match value().as_str() {
                    "hop" => ScanMode::HopBands,
                    "band" => ScanMode::OneBand,
                    "channel" => ScanMode::Channels,
                    other => die(format!("--scan takes hop, band or channel, not {other:?}")),
                }
            }
            "--dwell" => o.config.dwell_secs = num(&a, value()),
            "--stay" => o.config.max_stay_secs = num(&a, value()),
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
            Kind::Digital { nac: Some(nac), .. } => format!("  {nac:03X}"),
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

fn import_radioreference(db: &mut Db, o: &Options, what: &str, id: &str) {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| die(format!("set {name} (see scanner --help)")));
    let creds = Credentials {
        username: var("RR_USERNAME"),
        password: var("RR_PASSWORD"),
        app_key: var("RR_APP_KEY"),
    };
    let id: i64 = id
        .parse()
        .unwrap_or_else(|_| die(format!("{id:?} is not a RadioReference ID number")));
    let systems = match what {
        "system" => radioreference::trunked_system(&creds, id).map(|system| vec![system]),
        _ => radioreference::county_systems(&creds, id),
    }
    .unwrap_or_else(|e| die(e));
    for Imported {
        name,
        location,
        plan,
        notes,
    } in systems
    {
        let name = o.name.clone().unwrap_or(name);
        let location = o.location.clone().unwrap_or(location);
        let added = db.add_system(&name, &location, &plan).unwrap_or_else(|e| die(e));
        println!("Added system {added}, {name:?}: {} channels", plan.entries.len());
        notes.iter().for_each(|n| println!("  note: {n}"));
    }
}

fn scan(o: &Options, plan: Plan) {
    let (lo, hi, _) = plan.span();
    let controls = Arc::new(Controls::new(&plan, o.volume, o.squelch_db));
    {
        // Stop cleanly, so the receiver is released and recordings are closed.
        let controls = controls.clone();
        ctrlc::set_handler(move || controls.stop()).ok();
    }
    eprintln!(
        "Monitoring {} channels, {:.4}-{:.4} MHz. Ctrl-C to quit.",
        plan.entries.len(),
        lo / 1e6,
        hi / 1e6
    );
    let mut announced = false;
    let result = engine::run(&plan, &o.config, &controls, |event| {
        match event {
            Event::Opened {
                channel,
                snr_db,
                playing,
                talkgroup,
                unit,
            } => {
                let e = &plan.entries[channel];
                println!(
                    "{} {} {:>9}  {:<17} {:<40} {:+3.0} dB{}",
                    Local::now().format("%H:%M:%S"),
                    if playing { '>' } else { ' ' },
                    talkgroup.map_or(e.id(), |tg| format!("TG {tg}")),
                    e.tag,
                    e.desc,
                    snr_db,
                    unit.map_or(String::new(), |unit| format!("  unit {unit}"))
                );
            }
            // Announced once, not on every hop.
            Event::Band { index: 0, count, .. } if count > 1 && !announced => {
                announced = true;
                let what = if o.config.scan == ScanMode::Channels {
                    "channels"
                } else {
                    "bands"
                };
                eprintln!("Taking turns between {count} {what}.");
            }
            _ => {}
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
        ["radioreference", what @ ("system" | "county"), id] => import_radioreference(&mut db, &o, what, id),
        ["channels" | "import" | "remove" | "radioreference", ..] => die(format!("wrong arguments\n{USAGE}")),
        [] => {
            eprintln!("scanner: which system? These are in the channel database:\n");
            list_systems(&db);
            eprintln!("\nFor example: scanner 1    (scanner --help for more)");
            exit(1)
        }
        _ => scan(&o, load_plan(&db, &o.words)),
    }
}
