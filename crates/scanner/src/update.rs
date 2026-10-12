//! Updating an installed copy from the project's releases on GitHub.
//!
//! A release carries one package per platform, the same archives the
//! [releases page](https://github.com/nathantodd-devel/radio-software/releases)
//! offers. Updating fetches the newest, checks it against the checksum
//! GitHub publishes for it, and puts its files in place of the installed
//! ones. Nothing here runs unless asked: the program never looks for
//! updates by itself.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// This program's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The key releases are signed with, and its fingerprint: an update is
/// installed only if this key signed it.
const PGP_KEY: &str = include_str!("../../../signing-key.asc");
const PGP_KEY_ID: [u8; 20] = [
    0xE5, 0x4C, 0x10, 0x41, 0x23, 0xA2, 0x2A, 0x44, 0xD2, 0x69, 0x56, 0xF7, 0x4B, 0x86, 0xF8, 0xE2, 0x88, 0x88, 0x92,
    0xDC,
];

const LATEST_RELEASE: &str = "https://api.github.com/repos/nathantodd-devel/radio-software/releases/latest";
/// GitHub wants to know who is asking.
const USER_AGENT: &str = concat!("dmps/", env!("CARGO_PKG_VERSION"));
/// What an installed file is renamed to while its replacement is put in.
const OLD: &str = "old";

/// The newest release.
pub struct Release {
    /// Its version, as "1.2.3".
    pub version: String,
    /// The package for this platform, if the release has one.
    pub package: Option<Package>,
}

/// One downloadable archive of a release.
pub struct Package {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// Its SHA-256 as hex, when GitHub says.
    pub sha256: Option<String>,
    /// Where its signature is, if the release has one.
    pub signature_url: Option<String>,
}

/// How release packages for this platform are named: "linux-x86_64". `None`
/// where there are no packages, and the program is built from source.
pub fn platform() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x86_64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("windows-x86_64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("macos-arm64")
    } else {
        None
    }
}

/// A version's numbers, for comparing: "v1.10.0" is [1, 10, 0].
fn numbers(version: &str) -> Vec<u64> {
    let version = version.trim_start_matches('v');
    // Anything after the numbers ("-beta") doesn't count.
    let numbers = version
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .next()
        .unwrap_or("");
    numbers.split('.').map(|n| n.parse().unwrap_or(0)).collect()
}

/// Whether version `candidate` is later than version `than`.
pub fn is_newer(candidate: &str, than: &str) -> bool {
    numbers(candidate) > numbers(than)
}

/// Ask GitHub what the newest release is.
pub fn latest() -> Result<Release, String> {
    let failed = |e: &dyn std::fmt::Display| format!("can't look up the newest release: {e}");
    let reply = ureq::get(LATEST_RELEASE)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| failed(&e))?
        .into_string()
        .map_err(|e| failed(&e))?;
    release(&reply, platform()).map_err(|e| failed(&e))
}

/// Read GitHub's description of a release, picking the package for
/// `platform`.
fn release(json: &str, platform: Option<&str>) -> Result<Release, String> {
    let reply: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let version = reply["tag_name"].as_str().ok_or("the reply names no version")?;
    let assets = reply["assets"].as_array().map_or(&[][..], Vec::as_slice);
    // Packages are "<name>-<version>-<platform>.tar.gz" (or .zip).
    let package = platform.and_then(|platform| {
        let ours = |name: &str| {
            [".tar.gz", ".zip"]
                .iter()
                .any(|ext| name.ends_with(&format!("-{platform}{ext}")))
        };
        let asset = assets.iter().find(|asset| asset["name"].as_str().is_some_and(ours))?;
        let name = asset["name"].as_str()?;
        let signature = assets
            .iter()
            .find(|asset| asset["name"].as_str() == Some(&format!("{name}.asc")));
        Some(Package {
            signature_url: signature.and_then(|asset| Some(asset["browser_download_url"].as_str()?.to_string())),
            name: name.to_string(),
            url: asset["browser_download_url"].as_str()?.to_string(),
            size: asset["size"].as_u64().unwrap_or(0),
            sha256: asset["digest"]
                .as_str()
                .and_then(|d| d.strip_prefix("sha256:"))
                .map(str::to_string),
        })
    });
    Ok(Release {
        version: version.trim_start_matches('v').to_string(),
        package,
    })
}

/// Download `package` to the file `to`, telling `progress` how many bytes
/// have arrived so far. Fails, leaving no file, if what arrives isn't what
/// GitHub says the package is, or wasn't signed with the project's key.
pub fn download(package: &Package, to: &Path, mut progress: impl FnMut(u64)) -> Result<(), String> {
    let failed = |e: &dyn std::fmt::Display| format!("can't download {}: {e}", package.name);
    let reply = ureq::get(&package.url)
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| failed(&e))?;
    let mut file = fs::File::create(to).map_err(|e| failed(&e))?;
    let (mut reader, mut hash, mut total) = (reply.into_reader(), Sha256::new(), 0u64);
    let mut chunk = vec![0u8; 1 << 16];
    let result = loop {
        match reader.read(&mut chunk) {
            Ok(0) => break Ok(()),
            Ok(n) => {
                hash.update(&chunk[..n]);
                total += n as u64;
                progress(total);
                if let Err(e) = file.write_all(&chunk[..n]) {
                    break Err(failed(&e));
                }
            }
            Err(e) => break Err(failed(&e)),
        }
    };
    let got: String = hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
    let result = result.and_then(|()| match &package.sha256 {
        Some(expected) if !expected.eq_ignore_ascii_case(&got) => {
            Err(failed(&"what arrived doesn't match the checksum GitHub gives for it"))
        }
        _ if package.size != 0 && total != package.size => Err(failed(&"it arrived incomplete")),
        _ => Ok(()),
    });
    drop(file);
    // The checksum is GitHub's word; the signature is the project's own, so
    // a package put there by anyone else is turned away.
    let result = result.and_then(|()| {
        let url = package
            .signature_url
            .as_ref()
            .ok_or_else(|| failed(&"the release isn't signed (yet), so it won't be installed"))?;
        let signature = ureq::get(url)
            .set("User-Agent", USER_AGENT)
            .call()
            .map_err(|e| failed(&e))?
            .into_string()
            .map_err(|e| failed(&e))?;
        let content = fs::read(to).map_err(|e| failed(&e))?;
        verify(&content, &signature, PGP_KEY, &PGP_KEY_ID).map_err(|e| failed(&e))
    });
    if result.is_err() {
        fs::remove_file(to).ok();
    }
    result
}

/// Check that `signature` (a detached OpenPGP signature, as text) is of
/// `content` and was made by the key `key`, whose fingerprint is `key_id`.
fn verify(content: &[u8], signature: &str, key: &str, key_id: &[u8]) -> Result<(), String> {
    use pgp::composed::{Deserializable, DetachedSignature, SignedPublicKey};
    use pgp::types::KeyDetails;

    let (key, _) = SignedPublicKey::from_string(key).map_err(|e| format!("the signing key can't be read: {e}"))?;
    if key.fingerprint().as_bytes() != key_id {
        return Err("the signing key isn't the project's".into());
    }
    let (signature, _) =
        DetachedSignature::from_string(signature).map_err(|e| format!("its signature can't be read: {e}"))?;
    signature
        .verify(&key, content)
        .map_err(|_| "it isn't signed with the project's key".to_string())
}

/// Bring the copy installed in `dir` up to the newest release, if there is a
/// newer one, saying nothing and asking nothing: for running unattended.
/// Returns the version installed, or `None` if it was up to date already.
/// What happened, when anything did, is added to `update.log` in `dir`.
pub fn update_quietly(dir: &Path) -> Result<Option<String>, String> {
    let result = (|| {
        let release = latest()?;
        if !is_newer(&release.version, VERSION) {
            return Ok(None);
        }
        let package = release
            .package
            .ok_or("the release has no package for this kind of computer")?;
        let archive = std::env::temp_dir().join(format!("dmps-update-{}-{}", std::process::id(), package.name));
        let installed = download(&package, &archive, |_| {}).and_then(|()| install(&archive, dir));
        fs::remove_file(&archive).ok();
        installed.map(|_| Some(release.version))
    })();
    let entry = match &result {
        Ok(None) => None,
        Ok(Some(version)) => Some(format!("updated from {VERSION} to {version}")),
        Err(e) => Some(format!("not updated: {e}")),
    };
    if let Some(entry) = entry {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("update.log"));
        if let Ok(mut log) = log {
            let when = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            writeln!(log, "{when}  {entry}").ok();
        }
    }
    result
}

/// Whether `dir` looks like where cargo builds the program, not an install.
pub fn is_build_dir(dir: &Path) -> bool {
    dir.join(".fingerprint").is_dir() || dir.join("build").join(".rustc_info.json").exists()
}

/// Put the files of the package `archive` into `dir`, in place of the ones
/// there. Returns the files written, relative to `dir`. The data folder is
/// not in a package and is left alone.
pub fn install(archive: &Path, dir: &Path) -> Result<Vec<PathBuf>, String> {
    let unpacked = std::env::temp_dir().join(format!("dmps-update-{}", std::process::id()));
    fs::remove_dir_all(&unpacked).ok();
    fs::create_dir_all(&unpacked).map_err(|e| format!("{}: {e}", unpacked.display()))?;
    let result = unpack(archive, &unpacked).and_then(|()| {
        // A package is one folder holding everything.
        let mut top: Vec<PathBuf> = entries(&unpacked)?;
        let root = match (top.pop(), top.is_empty()) {
            (Some(only), true) if only.is_dir() => only,
            _ => unpacked.clone(),
        };
        let mut written = Vec::new();
        copy_over(&root, &root, dir, &mut written)?;
        Ok(written)
    });
    fs::remove_dir_all(&unpacked).ok();
    result
}

/// Unpack a .tar.gz or .zip with the system's `tar`, which reads both where
/// each is used: bsdtar on Windows and macOS, and .tar.gz on Linux.
fn unpack(archive: &Path, into: &Path) -> Result<(), String> {
    let status = Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(into)
        .status()
        .map_err(|e| format!("can't run tar to unpack the update: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("tar couldn't unpack the update".into())
    }
}

fn entries(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let failed = |e: std::io::Error| format!("{}: {e}", dir.display());
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).map_err(failed)? {
        found.push(entry.map_err(failed)?.path());
    }
    found.sort();
    Ok(found)
}

/// Copy everything under `from` to the same place under `to`.
fn copy_over(root: &Path, from: &Path, to: &Path, written: &mut Vec<PathBuf>) -> Result<(), String> {
    for path in entries(from)? {
        let relative = path.strip_prefix(root).expect("found under the root");
        let dest = to.join(relative);
        if path.is_dir() {
            fs::create_dir_all(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
            copy_over(root, &path, to, written)?;
            // A folder that comes in the package is the package's own: what
            // the new one no longer has (the last version's web page, whose
            // files are named after their contents) is cleared out.
            for old in entries(&dest)? {
                if old.is_file() && !path.join(old.file_name().unwrap_or_default()).exists() {
                    fs::remove_file(&old).ok();
                }
            }
        } else {
            replace(&path, &dest).map_err(|e| format!("can't replace {}: {e}", dest.display()))?;
            written.push(relative.to_path_buf());
        }
    }
    Ok(())
}

/// Put the file `new` at `dest`. A file already there is moved aside
/// first, which works even for a program that is running, and put back if
/// the new one can't be written.
fn replace(new: &Path, dest: &Path) -> std::io::Result<()> {
    let mut aside = dest.as_os_str().to_owned();
    aside.push(".");
    aside.push(OLD);
    let aside = PathBuf::from(aside);
    fs::remove_file(&aside).ok();
    let was_there = dest.exists();
    if was_there {
        fs::rename(dest, &aside)?;
    }
    match fs::copy(new, dest) {
        Ok(_) => {
            // Windows won't delete a running program: it goes next time.
            fs::remove_file(&aside).ok();
            Ok(())
        }
        Err(e) => {
            if was_there {
                fs::rename(&aside, dest).ok();
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("1.0.1", "1.0.0"));
        assert!(is_newer("v1.10.0", "1.9.3"));
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("0.3.0", "1.0.0"));
        assert!(is_newer("1.1.0-beta", "1.0.0"));
    }

    #[test]
    fn reads_a_release() {
        let json = r#"{"tag_name": "v1.2.0", "assets": [
            {"name": "dmps-v1.2.0-android-arm64.apk", "browser_download_url": "https://example/apk", "size": 5},
            {"name": "dmps-v1.2.0-linux-x86_64.tar.gz", "browser_download_url": "https://example/linux",
             "size": 7, "digest": "sha256:abc123"},
            {"name": "dmps-v1.2.0-linux-x86_64.tar.gz.asc", "browser_download_url": "https://example/linux.asc"},
            {"name": "dmps-v1.2.0-windows-x86_64.zip", "browser_download_url": "https://example/windows", "size": 9}
        ]}"#;
        let found = release(json, Some("linux-x86_64")).unwrap();
        assert_eq!(found.version, "1.2.0");
        let package = found.package.unwrap();
        assert_eq!((package.url.as_str(), package.size), ("https://example/linux", 7));
        assert_eq!(package.sha256.as_deref(), Some("abc123"));
        assert_eq!(package.signature_url.as_deref(), Some("https://example/linux.asc"));
        // No checksum given, and a platform with no package.
        assert!(
            release(json, Some("windows-x86_64"))
                .unwrap()
                .package
                .unwrap()
                .sha256
                .is_none()
        );
        assert!(release(json, Some("macos-arm64")).unwrap().package.is_none());
        assert!(release(json, None).unwrap().package.is_none());
        assert!(release("{}", Some("linux-x86_64")).is_err());
    }

    const PACKAGE: &[u8] = include_bytes!("testdata/package.bin");
    const SIGNATURE: &str = include_str!("testdata/package.bin.asc");
    /// A key made for these tests, which signed the package above.
    const TEST_KEY: &str = include_str!("testdata/test-key.asc");
    const TEST_KEY_ID: [u8; 20] = [
        0x2F, 0x10, 0x2D, 0xBE, 0x28, 0x7C, 0x93, 0x16, 0xBC, 0xFC, 0xAD, 0x07, 0xC4, 0xDE, 0xE5, 0x2F, 0x04, 0x17,
        0x19, 0x5D,
    ];

    #[test]
    fn checks_signatures() {
        assert_eq!(verify(PACKAGE, SIGNATURE, TEST_KEY, &TEST_KEY_ID), Ok(()));
        // Changed since it was signed.
        assert!(verify(b"another package\n", SIGNATURE, TEST_KEY, &TEST_KEY_ID).is_err());
        assert!(verify(PACKAGE, "not a signature", TEST_KEY, &TEST_KEY_ID).is_err());
        // The key swapped for another, the signature with it.
        assert_eq!(
            verify(PACKAGE, SIGNATURE, TEST_KEY, &PGP_KEY_ID).unwrap_err(),
            "the signing key isn't the project's"
        );
    }

    #[test]
    fn only_the_projects_key_will_do() {
        // The key shipped is the one named, and what another key signed fails.
        assert_eq!(
            verify(PACKAGE, SIGNATURE, PGP_KEY, &PGP_KEY_ID).unwrap_err(),
            "it isn't signed with the project's key"
        );
    }

    #[test]
    fn installs_a_package_over_an_old_one() {
        let work = std::env::temp_dir().join(format!("dmps-update-test-{}", std::process::id()));
        fs::remove_dir_all(&work).ok();
        let (package, installed) = (work.join("dmps-v9-test"), work.join("installed"));
        fs::create_dir_all(package.join("web")).unwrap();
        fs::create_dir_all(installed.join("dmps")).unwrap();
        fs::write(package.join("scanner"), "new program").unwrap();
        fs::write(package.join("web/index.html"), "new page").unwrap();
        fs::write(installed.join("scanner"), "old program").unwrap();
        fs::create_dir_all(installed.join("web")).unwrap();
        fs::write(installed.join("web/page-0123.wasm"), "the last version's page").unwrap();
        fs::write(installed.join("notes.txt"), "mine").unwrap();
        fs::write(installed.join("dmps/channels.db"), "my channels").unwrap();
        let archive = work.join("package.tar.gz");
        let packed = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&work)
            .arg("dmps-v9-test")
            .status()
            .unwrap();
        assert!(packed.success());

        let mut written = install(&archive, &installed).unwrap();
        written.sort();
        assert_eq!(written, [PathBuf::from("scanner"), PathBuf::from("web/index.html")]);
        assert_eq!(fs::read_to_string(installed.join("scanner")).unwrap(), "new program");
        assert_eq!(
            fs::read_to_string(installed.join("web/index.html")).unwrap(),
            "new page"
        );
        // The data folder is untouched, and nothing is left lying about.
        assert_eq!(
            fs::read_to_string(installed.join("dmps/channels.db")).unwrap(),
            "my channels"
        );
        assert!(!installed.join("scanner.old").exists());
        // What the last version left in the package's own folders is gone;
        // other files beside the programs are not.
        assert!(!installed.join("web/page-0123.wasm").exists());
        assert!(installed.join("notes.txt").exists());
        fs::remove_dir_all(&work).ok();
    }
}
