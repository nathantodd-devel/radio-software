//! Command-line front end for the scanner.

use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Arc;

use chrono::Local;
use scanner::db::Db;
use scanner::engine::{self, Config, Controls, Event, ScanMode, Source};
use scanner::plan::{Kind, Plan};
use scanner::radioreference::{self, Credentials, Imported};
use scanner::update;

const USAGE: &str = "\
usage: scanner [options] SYSTEM...     scan one or more systems
       scanner systems                 list the systems in the channel database
       scanner channels SYSTEM...      list the channels of systems
       scanner import FILE [--name NAME] [--county COUNTY] [--sites FILE]
       scanner remove SYSTEM
       scanner counties                list the counties and how many systems each has
       scanner remove-county COUNTY    remove a county and every system in it
       scanner radioreference system ID    import a trunked system by its system ID
       scanner radioreference county ID    import a county's conventional channels
       scanner update [--check] [--yes]    update to the newest release
       scanner --version

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

      --device KIND  auto (every receiver attached, sharing the bands between
                     them; the default), or one kind only: airspy or rtlsdr
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
      --db FILE      channel database (default: dmps/channels.db beside
                     this program if that folder exists, otherwise in your
                     user data directory; ~/.local/share on Linux)
  -v, --version      say which version this is

`update` asks GitHub for the newest release and, if it is newer than this
one, downloads the package for this computer and replaces the programs where
this one is (scanner, scanner-ui, scanner-web and what comes with them).
Channels and settings are not touched. Nothing looks for updates unasked.
      --check        only say whether there is a newer release (exit status
                     10 if there is)
  -y, --yes          don't ask before replacing the programs
      --force        install the newest release even if it isn't newer
      --dir DIR      update the copy in DIR instead of this one";

struct Options {
    words: Vec<String>,
    squelch_db: f32,
    volume: f32,
    db: PathBuf,
    name: Option<String>,
    location: Option<String>,
    sites: Option<PathBuf>,
    /// For `update`: only say whether there is one; don't ask first;
    /// install the newest release even if it isn't newer; and where.
    check: bool,
    yes: bool,
    force: bool,
    dir: Option<PathBuf>,
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
        check: false,
        yes: false,
        force: false,
        dir: None,
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
                let mode = value();
                o.config.scan = ScanMode::from_id(&mode)
                    .unwrap_or_else(|| die(format!("--scan takes hop, band or channel, not {mode:?}")));
            }
            "--dwell" => o.config.dwell_secs = num(&a, value()),
            "--stay" => o.config.max_stay_secs = num(&a, value()),
            "--db" => o.db = PathBuf::from(value()),
            "--name" => o.name = Some(value()),
            // A system belongs to a county; "location" is the old word for it.
            "--county" | "--location" => o.location = Some(value()),
            "--sites" => o.sites = Some(PathBuf::from(value())),
            "--check" => o.check = true,
            "-y" | "--yes" => o.yes = true,
            "--force" => o.force = true,
            "--dir" => o.dir = Some(PathBuf::from(value())),
            "-v" | "--version" => {
                println!("scanner {} (Digital Mobile Police Scanner)", update::VERSION);
                exit(0)
            }
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
                    "(no county)"
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
            Kind::Analog { dcs: Some(code), .. } => format!(" D{code:03o}"),
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
    let (mut several, mut announced_for) = (false, 0);
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
            Event::Receivers(names) if names.len() > 1 => {
                several = true;
                eprintln!("Scanning with {} receivers: {}.", names.len(), names.join(", "));
            }
            // With several receivers, what each one covers.
            Event::Band {
                receiver,
                index: 0,
                count,
                lo_hz,
                hi_hz,
            } if several && announced_for <= receiver => {
                announced_for = receiver + 1;
                let turns = if count > 1 {
                    format!(", the first of {count} it takes turns on")
                } else {
                    String::new()
                };
                eprintln!(
                    "  receiver {}: {:.4}–{:.4} MHz{turns}",
                    receiver + 1,
                    lo_hz / 1e6,
                    hi_hz / 1e6
                );
            }
            // Announced once, not on every hop.
            Event::Band { index: 0, count, .. } if count > 1 && !announced && !several => {
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

/// `scanner update`: look for a newer release and install it.
fn update_program(o: &Options) {
    if o.words.len() > 1 {
        die(format!("wrong arguments\n{USAGE}"));
    }
    let release = update::latest().unwrap_or_else(|e| die(e));
    let newer = update::is_newer(&release.version, update::VERSION);
    if newer {
        println!("Version {} is out; this is {}.", release.version, update::VERSION);
    } else {
        println!(
            "This is version {}, and the newest release is {}.",
            update::VERSION,
            release.version
        );
    }
    if o.check || !(newer || o.force) {
        // For scripts: 0 when up to date, 10 when there is something newer.
        exit(if newer { 10 } else { 0 })
    }
    let Some(package) = release.package else {
        die("the release has no package for this kind of computer; build it from source instead")
    };
    let dir = o.dir.clone().unwrap_or_else(|| {
        let program = std::env::current_exe().unwrap_or_else(|e| die(format!("can't tell where this program is: {e}")));
        let dir = program.parent().map(Path::to_path_buf).unwrap_or_default();
        if update::is_build_dir(&dir) {
            die("this copy was built from source: update it with `git pull` and `cargo build --release`")
        }
        dir
    });
    println!(
        "{} ({:.1} MB) will replace the programs in {}.",
        package.name,
        package.size as f64 / 1e6,
        dir.display()
    );
    if !o.yes {
        eprint!("Go ahead? [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).ok();
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            die("not updated")
        }
    }
    let archive = std::env::temp_dir().join(format!("dmps-update-{}-{}", std::process::id(), package.name));
    let mut shown = 0;
    let result = update::download(&package, &archive, |bytes| {
        // A line every tenth of the way.
        let tenth = (bytes * 10).checked_div(package.size).unwrap_or(0);
        if tenth > shown {
            shown = tenth;
            eprintln!("  downloaded {}%", tenth * 10);
        }
    })
    .and_then(|()| update::install(&archive, &dir));
    std::fs::remove_file(&archive).ok();
    let written = result.unwrap_or_else(|e| die(e));
    println!(
        "Updated to version {}: {} files replaced. Restart any of the programs that are running.",
        release.version,
        written.len()
    );
}

fn main() {
    let o = parse_args();
    // Updating has nothing to do with the channel database.
    if o.words.first().map(String::as_str) == Some("update") {
        return update_program(&o);
    }
    let mut db = Db::open(&o.db).unwrap_or_else(|e| die(e));
    let words: Vec<&str> = o.words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["systems"] => list_systems(&db),
        ["channels", _, ..] => list_channels(&load_plan(&db, &o.words[1..])),
        ["import", file] => import(&mut db, &o, file),
        ["counties"] => {
            for county in db.counties().unwrap_or_else(|e| die(e)) {
                println!("{:>5}  {:<36} {} systems", county.id, county.name, county.systems);
            }
        }
        ["remove-county", county] => {
            let county = db.find_county(county).unwrap_or_else(|e| die(e));
            db.remove_county(county.id).unwrap_or_else(|e| die(e));
            println!("Removed {} and its {} systems", county.name, county.systems);
        }
        ["remove", system] => {
            let system = db.find(system).unwrap_or_else(|e| die(e));
            db.remove_system(system.id).unwrap_or_else(|e| die(e));
            println!("Removed system {}, {:?}", system.id, system.name);
        }
        ["radioreference", what @ ("system" | "county"), id] => import_radioreference(&mut db, &o, what, id),
        [
            "channels" | "import" | "remove" | "remove-county" | "counties" | "radioreference",
            ..,
        ] => die(format!("wrong arguments\n{USAGE}")),
        [] => {
            eprintln!("scanner: which system? These are in the channel database:\n");
            list_systems(&db);
            eprintln!("\nFor example: scanner 1    (scanner --help for more)");
            exit(1)
        }
        _ => scan(&o, load_plan(&db, &o.words)),
    }
}
