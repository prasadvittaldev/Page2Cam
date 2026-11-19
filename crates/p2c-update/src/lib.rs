//! Update checks against GitHub Releases, and verification of downloads.
//!
//! A release carries `Page2Cam-Setup-<version>.exe`, `SHA256SUMS` and
//! `SHA256SUMS.sig` (ed25519 signature of SHA256SUMS, base64). The app only
//! runs an installer whose hash is listed in a correctly signed SHA256SUMS.
use std::path::Path;

/// Where releases are published.
pub const REPO: &str = "prasadvittaldev/Page2Cam";

/// Public half of the release signing key (ed25519, base64). The private half
/// lives only on the release machine (`~/.config/page2cam/release-signing.key`).
pub const RELEASE_PUBLIC_KEY: &str = "yY3GQs5sx8xIMDKQZ8Nc18IlL/oTfE+3mAdoMGF/ZZM=";

/// The newest release on GitHub, if it carries an installer.
/// Uses the system's `curl` (shipped with Windows 10+ and Linux distros).
pub fn fetch_latest() -> anyhow::Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let out = curl(&[
        "-fsSL",
        "--max-time",
        "20",
        "-H",
        "Accept: application/vnd.github+json",
        "--url",
        &url,
    ])?;
    parse_release(&String::from_utf8_lossy(&out))
}

/// A small HTTPS download (at most 2 MB) into memory, given up after
/// `max_secs` (e.g. a video thumbnail for the admin page).
pub fn fetch_small(url: &str, max_secs: u32) -> anyhow::Result<Vec<u8>> {
    let secs = max_secs.to_string();
    curl_as(
        "download failed",
        &[
            "-fsSL",
            "--max-time",
            &secs,
            "--max-filesize",
            "2000000",
            "--url",
            url,
        ],
    )
}

/// Download `url` to `dest`.
pub fn download(url: &str, dest: &Path) -> anyhow::Result<()> {
    let dest = dest.to_string_lossy();
    curl(&[
        "-fsSL",
        "--retry",
        "3",
        "--max-time",
        "1800",
        "-o",
        &dest,
        "--url",
        url,
    ])?;
    Ok(())
}

/// A downloaded installer whose hash matched the signed SHA256SUMS. The file
/// stays open with write/delete sharing denied, so nothing can swap it before
/// [`VerifiedInstaller::launch_elevated`] starts it.
pub struct VerifiedInstaller {
    pub path: std::path::PathBuf,
    _lock: std::fs::File,
}

/// Download a release's installer into `dir`, but only accept it if its hash is
/// listed in a SHA256SUMS signed with [`RELEASE_PUBLIC_KEY`].
pub fn download_verified(r: &Release, dir: &Path) -> anyhow::Result<VerifiedInstaller> {
    let sums = String::from_utf8(curl(&["-fsSL", "--max-time", "60", "--url", &r.sums_url])?)?;
    let sig = String::from_utf8(curl(&["-fsSL", "--max-time", "60", "--url", &r.sig_url])?)?;
    verify_sums(&sums, &sig, RELEASE_PUBLIC_KEY)?;
    let want = expected_hash(&sums, &r.setup_name)
        .ok_or_else(|| anyhow::anyhow!("{} is not listed in SHA256SUMS", r.setup_name))?
        .to_ascii_lowercase();
    let path = dir.join(&r.setup_name);
    download(&r.setup_url, &path)?;
    let mut lock = open_locked(&path)?;
    let got = sha256_of(&mut lock)?;
    if got != want {
        drop(lock);
        let _ = std::fs::remove_file(&path);
        anyhow::bail!("the downloaded installer is damaged (checksum mismatch); try again");
    }
    Ok(VerifiedInstaller { path, _lock: lock })
}

/// Open for reading while denying other writers/deleters (Windows).
fn open_locked(path: &Path) -> anyhow::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(0x0000_0001); // FILE_SHARE_READ only
    }
    Ok(o.open(path)?)
}

impl VerifiedInstaller {
    /// Start the installer with administrator rights (Windows asks the user)
    /// while the file is still locked against changes.
    pub fn launch_elevated(self, args: &str) -> anyhow::Result<()> {
        #[cfg(windows)]
        {
            win::run_as_admin(&self.path, args)?;
            drop(self);
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = args;
            anyhow::bail!("installing updates is only available on Windows")
        }
    }
}

#[cfg(windows)]
mod win {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    pub fn run_as_admin(exe: &Path, args: &str) -> anyhow::Result<()> {
        let file = wide(exe.as_os_str());
        let params = wide(std::ffi::OsStr::new(args));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        // SAFETY: all pointers point at NUL-terminated buffers that outlive the call.
        unsafe { ShellExecuteExW(&mut info) }.map_err(|e| {
            anyhow::anyhow!(
                "Windows did not start the installer (was the permission prompt declined?): {e}"
            )
        })
    }
}

fn curl(args: &[&str]) -> anyhow::Result<Vec<u8>> {
    curl_as("update check failed", args)
}

/// Run curl (HTTPS only); a failure reads "<what>: <curl's message>".
fn curl_as(what: &str, args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let mut cmd = std::process::Command::new("curl");
    // HTTPS only, including redirects (GitHub sends downloads to its CDN).
    cmd.args(["--proto", "=https", "--proto-redir", "=https"]);
    cmd.args(["-A", concat!("Page2Cam/", env!("CARGO_PKG_VERSION"))])
        .args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("could not run curl: {e}"))?;
    anyhow::ensure!(
        out.status.success(),
        "{what}: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(out.stdout)
}

/// A newer release found on GitHub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub page_url: String,
    pub setup_name: String,
    pub setup_url: String,
    pub sums_url: String,
    pub sig_url: String,
}

/// `x.y.z` (optional leading `v`) as numbers; None if malformed.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut it = s.trim().trim_start_matches('v').split('.');
    let v = (
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    );
    it.next().is_none().then_some(v)
}

/// True if `latest` is a newer version than `current`.
pub fn is_newer(current: &str, latest: &str) -> bool {
    matches!((parse_version(current), parse_version(latest)), (Some(c), Some(l)) if l > c)
}

/// Read GitHub's "latest release" JSON. None if it lacks the files we need.
pub fn parse_release(json: &str) -> anyhow::Result<Option<Release>> {
    #[derive(serde::Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }
    #[derive(serde::Deserialize)]
    struct Gh {
        tag_name: String,
        html_url: String,
        assets: Vec<Asset>,
    }
    let gh: Gh = serde_json::from_str(json)?;
    let Some((a, b, c)) = parse_version(&gh.tag_name) else {
        return Ok(None);
    };
    let version = format!("{a}.{b}.{c}");
    // Only files from this project's own releases: never anything the API
    // response points elsewhere, and never text curl could take as an option.
    let download_prefix = format!("https://github.com/{REPO}/releases/download/");
    let find = |name: &str| {
        gh.assets
            .iter()
            .find(|x| x.name == name)
            .map(|x| x.browser_download_url.clone())
            .filter(|u| u.starts_with(&download_prefix))
    };
    if !gh
        .html_url
        .starts_with(&format!("https://github.com/{REPO}/releases/"))
    {
        return Ok(None);
    }
    let setup_name = format!("Page2Cam-Setup-{version}.exe");
    let (Some(setup_url), Some(sums_url), Some(sig_url)) = (
        find(&setup_name),
        find("SHA256SUMS"),
        find("SHA256SUMS.sig"),
    ) else {
        return Ok(None);
    };
    Ok(Some(Release {
        version,
        page_url: gh.html_url,
        setup_name,
        setup_url,
        sums_url,
        sig_url,
    }))
}

/// Check that `sig_b64` is a valid signature of `sums` by `public_key_b64`.
pub fn verify_sums(sums: &str, sig_b64: &str, public_key_b64: &str) -> anyhow::Result<()> {
    use base64::Engine;
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    let b64 = base64::engine::general_purpose::STANDARD;
    let key: [u8; 32] = b64
        .decode(public_key_b64.trim())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("bad public key length"))?;
    let sig: [u8; 64] = b64
        .decode(sig_b64.trim())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("bad signature length"))?;
    VerifyingKey::from_bytes(&key)?
        .verify(sums.as_bytes(), &Signature::from_bytes(&sig))
        .map_err(|_| anyhow::anyhow!("the update's signature is not valid"))
}

/// The hex SHA-256 listed for `file_name` in a `sha256sum`-style list.
pub fn expected_hash<'a>(sums: &'a str, file_name: &str) -> Option<&'a str> {
    sums.lines().find_map(|l| {
        let (hash, name) = l.split_once(char::is_whitespace)?;
        (name.trim_start().trim_start_matches('*') == file_name).then_some(hash)
    })
}

/// Hex SHA-256 of a file.
pub fn file_sha256(path: &Path) -> anyhow::Result<String> {
    sha256_of(&mut std::fs::File::open(path)?)
}

/// Hex SHA-256 of everything readable from `f`.
fn sha256_of(f: &mut std::fs::File) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};

    const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.1.0", "v0.2.0"));
        assert!(is_newer("0.9.1", "0.10.0"));
        assert!(!is_newer("0.2.0", "v0.2.0"));
        assert!(!is_newer("0.2.0", "0.1.9"));
        assert!(!is_newer("0.2.0", "nightly"));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), None);
    }

    const RELEASE_JSON: &str = r#"{
      "tag_name": "v0.2.0",
      "html_url": "https://github.com/prasadvittaldev/Page2Cam/releases/tag/v0.2.0",
      "assets": [
        {"name": "Page2Cam-windows-x64.zip", "browser_download_url": "https://github.com/prasadvittaldev/Page2Cam/releases/download/v0.2.0/Page2Cam-windows-x64.zip"},
        {"name": "Page2Cam-Setup-0.2.0.exe", "browser_download_url": "https://x/setup"},
        {"name": "SHA256SUMS", "browser_download_url": "https://github.com/prasadvittaldev/Page2Cam/releases/download/v0.2.0/SHA256SUMS"},
        {"name": "SHA256SUMS.sig", "browser_download_url": "https://github.com/prasadvittaldev/Page2Cam/releases/download/v0.2.0/SHA256SUMS.sig"}
      ]
    }"#;

    #[test]
    fn finds_installer_and_checksums() {
        let json = RELEASE_JSON.replace(
            "https://x/setup",
            "https://github.com/prasadvittaldev/Page2Cam/releases/download/v0.2.0/Page2Cam-Setup-0.2.0.exe",
        );
        let r = parse_release(&json).unwrap().unwrap();
        assert_eq!(r.version, "0.2.0");
        assert_eq!(r.setup_name, "Page2Cam-Setup-0.2.0.exe");
        assert!(r.setup_url.ends_with("/v0.2.0/Page2Cam-Setup-0.2.0.exe"));
        assert!(r.sums_url.ends_with("/v0.2.0/SHA256SUMS"));
        assert!(r.sig_url.ends_with("/v0.2.0/SHA256SUMS.sig"));
        assert!(r.page_url.ends_with("/v0.2.0"));
    }

    #[test]
    fn hostile_urls_are_rejected() {
        // Only files from this project's own GitHub releases are accepted.
        for bad in [
            "-K/host/share/cfg",
            "http://github.com/prasadvittaldev/Page2Cam/releases/download/v0.2.0/x",
            "file:///C:/x.exe",
            "https://evil.example/Page2Cam-Setup-0.2.0.exe",
            "https://github.com/someone/else/releases/download/v0.2.0/x",
        ] {
            let json = RELEASE_JSON.replace("https://x/setup", bad);
            assert_eq!(parse_release(&json).unwrap(), None, "{bad}");
        }
        let json = RELEASE_JSON.replace(
            "https://github.com/prasadvittaldev/Page2Cam/releases/tag/v0.2.0",
            "javascript:alert(1)",
        );
        assert_eq!(parse_release(&json).unwrap(), None);
    }

    #[test]
    fn release_without_installer_is_ignored() {
        let json = r#"{"tag_name":"v0.1.0","html_url":"u","assets":[{"name":"Page2Cam-windows-x64.zip","browser_download_url":"z"}]}"#;
        assert_eq!(parse_release(json).unwrap(), None);
    }

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    #[test]
    fn signature_checks() {
        let sums = "abc  Page2Cam-Setup-0.2.0.exe\n";
        let sig = B64.encode(key().sign(sums.as_bytes()).to_bytes());
        let public = B64.encode(key().verifying_key().to_bytes());
        assert!(verify_sums(sums, &sig, &public).is_ok());
        assert!(verify_sums("abd  Page2Cam-Setup-0.2.0.exe\n", &sig, &public).is_err());
        let other = B64.encode(
            SigningKey::from_bytes(&[8u8; 32])
                .verifying_key()
                .to_bytes(),
        );
        assert!(verify_sums(sums, &sig, &other).is_err());
        assert!(verify_sums(sums, "not base64!", &public).is_err());
    }

    #[test]
    fn hash_lookup_and_file_hash() {
        let sums = "11aa  Page2Cam-windows-x64.zip\n22bb *Page2Cam-Setup-0.2.0.exe\n";
        assert_eq!(
            expected_hash(sums, "Page2Cam-Setup-0.2.0.exe"),
            Some("22bb")
        );
        assert_eq!(expected_hash(sums, "missing.exe"), None);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            file_sha256(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
