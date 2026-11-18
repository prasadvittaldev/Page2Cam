//! Update checks (GitHub Releases) and self-install through the installer.
use std::sync::Arc;
use std::time::Duration;

use p2c_admin::UpdateInfo;
use p2c_update::Release;
use parking_lot::Mutex;

/// Looks up the newest release (the real one is [`p2c_update::fetch_latest`]).
pub type Fetch = Box<dyn Fn() -> anyhow::Result<Option<Release>> + Send + Sync>;

const FIRST_CHECK: Duration = Duration::from_secs(15);
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

pub struct Updater {
    current: String,
    /// Installed with the installer (so it can update itself).
    installed: bool,
    fetch: Fetch,
    info: Mutex<UpdateInfo>,
    release: Mutex<Option<Release>>,
}

impl Updater {
    pub fn new(current: &str, installed: bool, fetch: Fetch) -> Updater {
        Updater {
            current: current.into(),
            installed,
            fetch,
            info: Mutex::new(UpdateInfo::default()),
            release: Mutex::new(None),
        }
    }

    pub fn info(&self) -> UpdateInfo {
        self.info.lock().clone()
    }

    /// Ask GitHub once. Failures are logged, not shown (no nagging offline).
    pub fn check_once(&self) {
        let found = match (self.fetch)() {
            Ok(r) => r.filter(|r| p2c_update::is_newer(&self.current, &r.version)),
            Err(e) => {
                tracing::debug!("update check failed: {e:#}");
                return;
            }
        };
        let mut info = self.info.lock();
        if info.state == "installing" {
            return;
        }
        *info = match &found {
            Some(r) => {
                if info.version != r.version {
                    tracing::info!(version = %r.version, "a newer Page2Cam is available");
                }
                UpdateInfo {
                    state: "available".into(),
                    version: r.version.clone(),
                    page_url: r.page_url.clone(),
                    can_install: self.installed,
                }
            }
            None => UpdateInfo::default(),
        };
        *self.release.lock() = found;
    }

    /// Check now and then every few hours, on a background thread.
    pub fn spawn_checks(self: &Arc<Self>) {
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("p2c-update".into())
            .spawn(move || {
                std::thread::sleep(FIRST_CHECK);
                loop {
                    me.check_once();
                    std::thread::sleep(CHECK_EVERY);
                }
            });
    }

    /// Download and verify the newer installer, then start it elevated.
    /// On success the caller should close Page2Cam; the installer restarts it.
    pub fn install(&self) -> anyhow::Result<()> {
        let release = self
            .release
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no update is available"))?;
        anyhow::ensure!(
            self.installed,
            "this copy of Page2Cam was not installed with the installer; download the new version from GitHub: {}",
            release.page_url
        );
        self.set_state("installing");
        let result = (|| {
            let dir = std::env::temp_dir().join("page2cam-update");
            std::fs::create_dir_all(&dir)?;
            tracing::info!(version = %release.version, "downloading the update");
            let setup = p2c_update::download_verified(&release, &dir)?;
            tracing::info!(installer = %setup.display(), "update verified; starting the installer");
            run_installer_elevated(&setup)
        })();
        if let Err(e) = &result {
            tracing::warn!("update failed: {e:#}");
            self.set_state(&format!("error: {e:#}"));
        }
        result
    }

    fn set_state(&self, state: &str) {
        self.info.lock().state = state.into();
    }
}

/// Start the installer silently with administrator rights (Windows asks the
/// user once). `/RELAUNCH` makes it start Page2Cam again when done.
fn run_installer_elevated(setup: &std::path::Path) -> anyhow::Result<()> {
    anyhow::ensure!(cfg!(windows), "self-update is only available on Windows");
    let path = setup.display().to_string().replace('\'', "''");
    let script =
        format!("Start-Process -FilePath '{path}' -ArgumentList '/S','/RELAUNCH' -Verb RunAs");
    let mut cmd = std::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output()?;
    anyhow::ensure!(
        out.status.success(),
        "Windows did not start the installer (was the permission prompt declined?)"
    );
    Ok(())
}

/// True when this exe was put in place by the installer (it writes
/// Uninstall.exe next to it). Portable zip copies update by hand.
pub fn is_installed() -> bool {
    cfg!(windows)
        && std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join("Uninstall.exe").is_file()))
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(v: &str) -> Release {
        Release {
            version: v.into(),
            page_url: format!("https://github.com/x/releases/tag/v{v}"),
            setup_name: format!("Page2Cam-Setup-{v}.exe"),
            setup_url: "u".into(),
            sums_url: "s".into(),
            sig_url: "g".into(),
        }
    }

    fn updater(installed: bool, latest: Option<&'static str>) -> Updater {
        Updater::new(
            "0.2.0",
            installed,
            Box::new(move || Ok(latest.map(release))),
        )
    }

    #[test]
    fn newer_release_is_offered() {
        let u = updater(true, Some("0.3.0"));
        u.check_once();
        let i = u.info();
        assert_eq!(
            (i.state.as_str(), i.version.as_str(), i.can_install),
            ("available", "0.3.0", true)
        );
        assert!(i.page_url.ends_with("v0.3.0"));
    }

    #[test]
    fn same_or_older_release_is_quiet() {
        for v in [Some("0.2.0"), Some("0.1.0"), None] {
            let u = updater(true, v);
            u.check_once();
            assert_eq!(u.info(), UpdateInfo::default(), "{v:?}");
        }
    }

    #[test]
    fn failed_check_is_quiet() {
        let u = Updater::new("0.2.0", true, Box::new(|| anyhow::bail!("offline")));
        u.check_once();
        assert_eq!(u.info(), UpdateInfo::default());
    }

    #[test]
    fn portable_copy_points_to_github() {
        let u = updater(false, Some("0.3.0"));
        u.check_once();
        assert!(!u.info().can_install);
        let e = u.install().unwrap_err().to_string();
        assert!(e.contains("GitHub"), "{e}");
    }

    #[test]
    fn install_without_an_update_fails() {
        let u = updater(true, None);
        u.check_once();
        assert!(u.install().is_err());
    }
}
