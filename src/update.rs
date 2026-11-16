//! Updating Featherplay itself from its latest stable GitHub release.
//!
//! GitHub's "latest release" is the newest one that is neither a draft nor a
//! pre-release. A newer version there carries the bare Windows executable as an asset,
//! along with `SHA256SUMS.txt` and that file's signature, `SHA256SUMS.txt.sig`. The
//! signature must verify with the release key built into the app (`RELEASE_KEY`, whose
//! private half only the release workflow holds), so a release published by anyone else,
//! even through the project's own GitHub account, is refused. The executable is then
//! downloaded beside the running one, checked against its sum, and swapped in: Windows lets a running program's file be
//! renamed, though not deleted, so the old one steps aside as `featherplay.exe.old`
//! and is removed by the next start. The app then restarts into the new version.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The release asset holding the bare Windows executable.
pub const WINDOWS_ASSET: &str = "featherplay-windows-x64.exe";
const SUMS_ASSET: &str = "SHA256SUMS.txt";
const SIGNATURE_ASSET: &str = "SHA256SUMS.txt.sig";
/// The public half of the key that signs releases (Ed25519, as hex). Made by
/// `examples/release_keygen.rs`; the private half is the repository secret
/// `FEATHERPLAY_SIGNING_KEY`, used by the release workflow.
const RELEASE_KEY: &str = "f8a03ee31b47146e8de052997c6af0a055f291a13ec18aedeec05b3d2b40d892";
// A whole executable can take a while on a slow connection; a stall still fails.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    /// The release's page on GitHub, for reading its notes.
    pub page: String,
    /// Where to get the update; absent where a release has no build for this platform,
    /// which is then updated by hand from `page`.
    pub download: Option<Download>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Download {
    pub program: String,
    pub sums: String,
    pub signature: String,
}

/// The newer stable release, if there is one.
pub fn check() -> Result<Option<Release>, String> {
    let url = std::env::var("FEATHERPLAY_UPDATE_URL").unwrap_or_else(|_| latest_release_url());
    let mut response =
        crate::net::agent().get(&url).header("Accept", "application/vnd.github+json").call().map_err(|e| match e {
            ureq::Error::StatusCode(404) => "no public release was found".to_string(),
            e => e.to_string(),
        })?;
    let body = response.body_mut().read_to_string().map_err(|e| e.to_string())?;
    parse_release(&body, env!("CARGO_PKG_VERSION"))
}

fn latest_release_url() -> String {
    let repository = env!("CARGO_PKG_REPOSITORY").trim_end_matches('/');
    let path = repository.trim_start_matches("https://github.com/");
    format!("https://api.github.com/repos/{path}/releases/latest")
}

/// Reads GitHub's answer: the release if it is newer than `current`.
fn parse_release(json: &str, current: &str) -> Result<Option<Release>, String> {
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("unreadable answer: {e}"))?;
    let tag = value["tag_name"].as_str().ok_or("the release has no version")?;
    let version = tag.trim_start_matches('v').to_string();
    if !is_newer(&version, current) {
        return Ok(None);
    }
    let asset = |name: &str| {
        value["assets"].as_array()?.iter().find(|a| a["name"] == name)?["browser_download_url"]
            .as_str()
            .map(str::to_string)
    };
    let download = match (cfg!(windows), asset(WINDOWS_ASSET), asset(SUMS_ASSET), asset(SIGNATURE_ASSET)) {
        (true, Some(program), Some(sums), Some(signature)) => Some(Download { program, sums, signature }),
        _ => None,
    };
    let page = value["html_url"].as_str().unwrap_or(env!("CARGO_PKG_REPOSITORY")).to_string();
    Ok(Some(Release { version, page, download }))
}

/// Whether version `a` (like "0.2.1") comes after `b`. Anything after a `-` is ignored:
/// pre-releases never reach here, as GitHub's latest release excludes them.
fn is_newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split('-').next().unwrap_or("").split('.').map(|p| p.parse().unwrap_or(0)).collect()
    };
    let (a, b) = (parts(a), parts(b));
    (0..a.len().max(b.len()))
        .map(|i| (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0)))
        .find(|(x, y)| x != y)
        .is_some_and(|(x, y)| x > y)
}

/// Downloads `release` beside `exe`, checks it, and puts it in `exe`'s place, keeping
/// the old file as `<exe>.old`. `progress` is told the fraction done as it downloads.
pub fn install(release: &Release, exe: &Path, progress: impl Fn(f32)) -> Result<(), String> {
    install_signed_by(release, exe, &unhex(RELEASE_KEY).expect("the release key is hex"), progress)
}

/// `install`, trusting releases signed with `key`.
fn install_signed_by(release: &Release, exe: &Path, key: &[u8], progress: impl Fn(f32)) -> Result<(), String> {
    let files = release.download.as_ref().ok_or("this release has no build for this system")?;
    let sums = get(&files.sums)?.body_mut().read_to_vec().map_err(|e| e.to_string())?;
    let signature = get(&files.signature)?.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let signature = unhex(signature.trim()).ok_or("the release's signature is unreadable")?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(&sums, &signature)
        .map_err(|_| "the release isn't signed by Featherplay's release key".to_string())?;
    let sums = String::from_utf8(sums).map_err(|_| "the release's checksums are unreadable")?;
    let expected = expected_sum(&sums, WINDOWS_ASSET).ok_or("the release's checksums don't list the program")?;
    let exe_url = &files.program;

    let new = with_suffix(exe, ".new");
    let result = download(exe_url, &new, &progress).and_then(|actual| match actual == expected {
        true => Ok(()),
        false => Err("the download doesn't match its checksum".to_string()),
    });
    if let Err(e) = result {
        std::fs::remove_file(&new).ok();
        return Err(e);
    }

    let old = with_suffix(exe, ".old");
    std::fs::remove_file(&old).ok();
    std::fs::rename(exe, &old).map_err(|e| format!("couldn't set the old version aside: {e}"))?;
    if let Err(e) = std::fs::rename(&new, exe) {
        // Put the old one back rather than leave no program at all.
        std::fs::rename(&old, exe).ok();
        return Err(format!("couldn't put the new version in place: {e}"));
    }
    Ok(())
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
    crate::net::agent()
        .get(url)
        .config()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .build()
        .call()
        .map_err(|e| e.to_string())
}

/// Streams `url` into `to`, returning the SHA-256 of what was written, in hex.
fn download(url: &str, to: &Path, progress: &impl Fn(f32)) -> Result<String, String> {
    let mut response = get(url)?;
    let total = response.body().content_length();
    let mut reader = response.body_mut().with_config().limit(200 * 1024 * 1024).reader();
    let mut file = std::fs::File::create(to).map_err(|e| e.to_string())?;
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let (mut buffer, mut done) = (vec![0u8; 64 * 1024], 0u64);
    loop {
        let n = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        hash.update(&buffer[..n]);
        done += n as u64;
        if let Some(total) = total.filter(|&t| t > 0) {
            progress(done as f32 / total as f32);
        }
    }
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(hash.finish().as_ref().iter().map(|b| format!("{b:02x}")).collect())
}

/// The sum `sums` (in `sha256sum`'s format) gives for the file `name`.
fn expected_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (sum, file) = line.split_once(char::is_whitespace)?;
        (file.trim().trim_start_matches('*') == name).then(|| sum.to_lowercase())
    })
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Removes the version an update set aside. Its file stays locked while that version
/// is still closing, so `patience` says how long to keep trying.
pub fn remove_old(exe: &Path, patience: Duration) {
    let old = with_suffix(exe, ".old");
    let started = std::time::Instant::now();
    while old.exists() {
        if std::fs::remove_file(&old).is_ok() || started.elapsed() >= patience {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    #[test]
    fn compares_versions_by_number() {
        assert!(is_newer("0.1.1", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.9"), "numbers, not text");
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("0.1.0-rc1", "0.1.0"));
    }

    fn answer(tag: &str, signed: bool) -> String {
        let signature =
            if signed { r#", {"name": "SHA256SUMS.txt.sig", "browser_download_url": "https://x/sig"}"# } else { "" };
        format!(
            r#"{{"tag_name": "{tag}", "html_url": "https://github.com/o/r/releases/tag/{tag}", "prerelease": false,
                "assets": [
                  {{"name": "Featherplay-0.2.0-windows-x64.zip", "browser_download_url": "https://x/zip"}},
                  {{"name": "{WINDOWS_ASSET}", "browser_download_url": "https://x/exe"}},
                  {{"name": "SHA256SUMS.txt", "browser_download_url": "https://x/sums"}}{signature}
                ]}}"#
        )
    }

    #[test]
    fn a_newer_release_is_offered_with_its_download() {
        let release = parse_release(&answer("v0.2.0", true), "0.1.0").unwrap().expect("newer");
        assert_eq!(release.version, "0.2.0");
        assert_eq!(release.page, "https://github.com/o/r/releases/tag/v0.2.0");
        if cfg!(windows) {
            let expected = Download {
                program: "https://x/exe".into(),
                sums: "https://x/sums".into(),
                signature: "https://x/sig".into(),
            };
            assert_eq!(release.download, Some(expected));
        } else {
            assert_eq!(release.download, None, "no build for this system: updated by hand");
        }
        let unsigned = parse_release(&answer("v0.2.0", false), "0.1.0").unwrap().expect("newer");
        assert_eq!(unsigned.download, None, "an unsigned release is never installed automatically");
        assert_eq!(parse_release(&answer("v0.1.0", true), "0.1.0").unwrap(), None, "the same version is up to date");
        assert!(parse_release("{}", "0.1.0").is_err());
    }

    #[test]
    fn finds_a_files_checksum() {
        let sums = "AB12  Featherplay-0.2.0-windows-x64.zip\ncd34 *featherplay-windows-x64.exe\n";
        assert_eq!(expected_sum(sums, WINDOWS_ASSET).as_deref(), Some("cd34"));
        assert_eq!(expected_sum(sums, "other"), None);
    }

    #[test]
    fn the_built_in_release_key_is_a_public_key() {
        assert_eq!(unhex(RELEASE_KEY).map(|k| k.len()), Some(32));
    }

    /// A key pair for a test: the private half, and the public half.
    fn key_pair() -> (Ed25519KeyPair, Vec<u8>) {
        let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).unwrap();
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap();
        let public = pair.public_key().as_ref().to_vec();
        (pair, public)
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// A tiny server standing in for GitHub, serving `files` by the end of their path;
    /// returns its address and a count of the requests it answered.
    fn serve(files: Vec<(&'static str, Vec<u8>)>) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::Write as _;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let answered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = answered.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut request = [0u8; 2048];
                let n = stream.read(&mut request).unwrap();
                let line = String::from_utf8_lossy(&request[..n]).lines().next().unwrap_or("").to_string();
                let body = files.iter().find(|(path, _)| line.contains(&format!("/{path} "))).map_or(&[][..], |f| &f.1);
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len())
                    .unwrap();
                stream.write_all(body).unwrap();
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        (address, answered)
    }

    fn release_at(address: &str) -> Release {
        Release {
            version: "9.9.9".into(),
            page: String::new(),
            download: Some(Download {
                program: format!("{address}/exe"),
                sums: format!("{address}/sums"),
                signature: format!("{address}/sig"),
            }),
        }
    }

    /// A folder with an "old program" in it, for an update to replace.
    fn old_program(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("featherplay-update-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("featherplay.exe");
        std::fs::write(&exe, b"old program").unwrap();
        (dir, exe)
    }

    fn sums_for(program: &[u8]) -> Vec<u8> {
        format!("{}  {WINDOWS_ASSET}\n", hex(ring::digest::digest(&ring::digest::SHA256, program).as_ref()))
            .into_bytes()
    }

    #[test]
    fn swaps_in_a_signed_checked_download_and_keeps_the_old_aside() {
        let (dir, exe) = old_program("good");
        let (key, public) = key_pair();
        let program = b"new program".to_vec();
        let sums = sums_for(&program);
        let signature = hex(key.sign(&sums).as_ref()).into_bytes();
        let (address, _) = serve(vec![("exe", program), ("sums", sums), ("sig", signature)]);

        let fractions = std::cell::RefCell::new(Vec::new());
        install_signed_by(&release_at(&address), &exe, &public, |f| fractions.borrow_mut().push(f)).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new program");
        assert_eq!(std::fs::read(dir.join("featherplay.exe.old")).unwrap(), b"old program");
        assert_eq!(fractions.borrow().last().copied(), Some(1.0));
        remove_old(&exe, Duration::ZERO);
        assert!(!dir.join("featherplay.exe.old").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_program_that_doesnt_match_its_signed_checksum_changes_nothing() {
        let (dir, exe) = old_program("tampered");
        let (key, public) = key_pair();
        let sums = sums_for(b"new program");
        let signature = hex(key.sign(&sums).as_ref()).into_bytes();
        let (address, _) = serve(vec![("exe", b"tampered program".to_vec()), ("sums", sums), ("sig", signature)]);

        let error = install_signed_by(&release_at(&address), &exe, &public, |_| {}).unwrap_err();
        assert!(error.contains("checksum"), "{error}");
        assert_eq!(std::fs::read(&exe).unwrap(), b"old program", "the running program is untouched");
        assert!(!dir.join("featherplay.exe.new").exists() && !dir.join("featherplay.exe.old").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn checksums_signed_by_another_key_are_refused_before_downloading() {
        let (dir, exe) = old_program("foreign");
        let (ours, _) = key_pair();
        let (_, theirs) = key_pair();
        let program = b"someone else's program".to_vec();
        let sums = sums_for(&program);
        let signature = hex(ours.sign(&sums).as_ref()).into_bytes();
        let (address, answered) = serve(vec![("exe", program), ("sums", sums), ("sig", signature)]);

        // The app trusts `theirs`; the release was signed with another key.
        let error = install_signed_by(&release_at(&address), &exe, &theirs, |_| {}).unwrap_err();
        assert!(error.contains("isn't signed"), "{error}");
        assert_eq!(
            answered.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "only the checksums and signature were fetched"
        );
        assert_eq!(std::fs::read(&exe).unwrap(), b"old program");
        std::fs::remove_dir_all(dir).ok();
    }
}
