//! Agent control, platform-free parts: page names, the discovery file,
//! registering Page2Cam with desktop agents, the activity list and which
//! agent calls get a balloon.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};

use p2c_mcp::BackendCall;
use serde_json::{json, Map, Value};

/// Default port of the tray's agent server.
pub const AGENT_PORT: u16 = 8766;
/// The MCP server's name in agent configs.
pub const SERVER_KEY: &str = "page2cam";

// ---------------------------------------------------------------- pages

const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// A safe file name (without `.html`): `[A-Za-z0-9 _-]{1,64}`, anything else
/// becomes `_`, Windows reserved names get a `_` in front, empty → `page`.
pub fn sanitize_page_name(name: &str) -> String {
    let name = name.strip_suffix(".html").unwrap_or(name);
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    s = s.trim().to_string();
    if s.is_empty() {
        s = "page".into();
    }
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(&s)) {
        s.insert(0, '_');
    }
    s.chars()
        .take(64)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// `<dir>/<sanitised name>.html` (always inside `dir`).
pub fn page_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.html", sanitize_page_name(name)))
}

/// The pages in `dir` (names without `.html`), sorted.
pub fn list_pages(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.strip_suffix(".html").map(String::from)
        })
        .collect();
    v.sort();
    v
}

/// Write `html` as page `name` in `dir` (created if needed); returns its path.
pub fn write_page(dir: &Path, name: &str, html: &str) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let p = page_path(dir, name);
    write_atomic(&p, html.as_bytes())?;
    Ok(p)
}

/// `file:///C:/…` URL for a local path.
pub fn file_url(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file:///");
    for c in s.trim_start_matches('/').chars() {
        match c {
            ' ' => out.push_str("%20"),
            '#' => out.push_str("%23"),
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3F"),
            c => out.push(c),
        }
    }
    out
}

// ------------------------------------------------------------ discovery

/// Write `path` through a temporary file and a rename (readers never see
/// half a file).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("p2c-tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// The discovery file's text.
pub fn discovery_json(url: &str, token: &str) -> String {
    serde_json::to_string_pretty(&json!({ "url": url, "token": token })).unwrap_or_default()
}

/// `(url, token)` from a discovery file's text.
pub fn parse_discovery(text: &str) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(text).ok()?;
    Some((
        v["url"].as_str()?.to_string(),
        v["token"].as_str()?.to_string(),
    ))
}

pub fn write_discovery(path: &Path, url: &str, token: &str) -> io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    write_atomic(path, discovery_json(url, token).as_bytes())
}

pub fn remove_discovery(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// `%LOCALAPPDATA%\page2cam\agent.json` (other systems: the local data dir).
pub fn discovery_path() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("page2cam").join("agent.json"))
    } else {
        directories::ProjectDirs::from("", "", "page2cam")
            .map(|d| d.data_local_dir().join("agent.json"))
    }
}

// --------------------------------------------------------- registration

/// Desktop agents whose config files we edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentApp {
    ClaudeDesktop,
    Codex,
    Cursor,
    VsCode,
}

impl AgentApp {
    pub fn label(self) -> &'static str {
        match self {
            AgentApp::ClaudeDesktop => "Claude Desktop",
            AgentApp::Codex => "Codex",
            AgentApp::Cursor => "Cursor",
            AgentApp::VsCode => "VS Code",
        }
    }
}

/// Why a config file was left alone.
#[derive(Debug, PartialEq, Eq)]
pub enum RegError {
    /// Not JSON/TOML we understand: never touched.
    Unparseable(String),
    /// The servers key isn't an object/table.
    Shape(String),
}

impl std::fmt::Display for RegError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegError::Unparseable(e) => {
                write!(f, "could not read the config file ({e}); left it unchanged")
            }
            RegError::Shape(k) => write!(
                f,
                "{k} in the config file is not a list of servers; left it unchanged"
            ),
        }
    }
}

fn parse_obj(text: &str) -> Result<Map<String, Value>, RegError> {
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(RegError::Unparseable("not a JSON object".into())),
        Err(e) => Err(RegError::Unparseable(e.to_string())),
    }
}

/// `text` (a JSON config) with our server under `key` (`mcpServers` or
/// `servers`), replacing an older one; everything else kept in order.
pub fn register_json(text: &str, key: &str, exe: &str, vscode: bool) -> Result<String, RegError> {
    let mut root = parse_obj(text)?;
    let servers = root
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(servers) = servers else {
        return Err(RegError::Shape(key.into()));
    };
    let mut entry = Map::new();
    if vscode {
        entry.insert("type".into(), "stdio".into());
    }
    entry.insert("command".into(), exe.into());
    entry.insert("args".into(), json!(["--mcp"]));
    servers.insert(SERVER_KEY.into(), Value::Object(entry));
    Ok(serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_default() + "\n")
}

/// `text` without our server under `key`; `Ok(None)` when there was none.
pub fn unregister_json(text: &str, key: &str) -> Result<Option<String>, RegError> {
    let mut root = parse_obj(text)?;
    let Some(Value::Object(servers)) = root.get_mut(key) else {
        return Ok(None);
    };
    if servers.shift_remove(SERVER_KEY).is_none() {
        return Ok(None);
    }
    Ok(Some(
        serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_default() + "\n",
    ))
}

/// Codex `config.toml` with `[mcp_servers.page2cam]` (comments kept).
pub fn register_codex_toml(text: &str, exe: &str) -> Result<String, RegError> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| RegError::Unparseable(e.to_string()))?;
    let servers = doc.entry("mcp_servers").or_insert_with(|| {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        toml_edit::Item::Table(t)
    });
    let Some(servers) = servers.as_table_mut() else {
        return Err(RegError::Shape("mcp_servers".into()));
    };
    let mut t = toml_edit::Table::new();
    t.insert("command", toml_edit::value(exe));
    let mut args = toml_edit::Array::new();
    args.push("--mcp");
    t.insert("args", toml_edit::value(args));
    servers.insert(SERVER_KEY, toml_edit::Item::Table(t));
    Ok(doc.to_string())
}

/// Codex `config.toml` without our table; `Ok(None)` when there was none.
pub fn unregister_codex_toml(text: &str) -> Result<Option<String>, RegError> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| RegError::Unparseable(e.to_string()))?;
    let Some(servers) = doc.get_mut("mcp_servers").and_then(|s| s.as_table_mut()) else {
        return Ok(None);
    };
    if servers.remove(SERVER_KEY).is_none() {
        return Ok(None);
    }
    Ok(Some(doc.to_string()))
}

/// Where the agents keep their configs.
#[derive(Clone, Debug, Default)]
pub struct RegEnv {
    /// `%APPDATA%` (Roaming).
    pub appdata: Option<PathBuf>,
    /// The user's home folder.
    pub home: Option<PathBuf>,
    /// `%LOCALAPPDATA%` (packaged apps keep their files under `Packages`).
    pub localappdata: Option<PathBuf>,
}

impl RegEnv {
    pub fn from_env() -> RegEnv {
        RegEnv {
            appdata: std::env::var_os("APPDATA").map(PathBuf::from),
            home: directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf()),
            localappdata: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        }
    }
}

/// One agent config file to edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub app: AgentApp,
    pub path: PathBuf,
    /// Read instead while `path` doesn't exist yet: the packaged Claude
    /// Desktop sees the real `%APPDATA%` file until it has its own copy, so
    /// its copy starts from that file (the user's other servers stay).
    pub seed: Option<PathBuf>,
    /// Older config files of the same agent that may hold an entry of ours
    /// (written before its newer layout was known): cleaned when switching
    /// off.
    pub stale: Vec<PathBuf>,
}

/// The first subfolder of `dir` whose name starts with `prefix` (any case),
/// by name, keeping only those `keep` accepts.
fn first_dir_with_prefix(
    dir: &Path,
    prefix: &str,
    keep: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.to_ascii_lowercase().starts_with(prefix))
        })
        .map(|e| e.path())
        .filter(|p| p.is_dir() && keep(p))
        .collect();
    v.sort();
    v.into_iter().next()
}

/// Claude Desktop 6+'s data folder: `%LOCALAPPDATA%\Claude-Data\claude_<id>`
/// (its config file lives there, next to its other data).
fn claude_data_dir(localappdata: &Path) -> Option<PathBuf> {
    first_dir_with_prefix(&localappdata.join("Claude-Data"), "claude", |_| true)
}

/// The packaged (MSIX) Claude Desktop's config folder:
/// `%LOCALAPPDATA%\Packages\Claude_<publisher id>\LocalCache\Roaming\Claude`
/// (its writes to `%APPDATA%` are redirected there).
fn packaged_claude_dir(localappdata: &Path) -> Option<PathBuf> {
    let pkg = first_dir_with_prefix(&localappdata.join("Packages"), "claude_", |p| {
        p.join("LocalCache").join("Roaming").is_dir()
    })?;
    Some(pkg.join("LocalCache").join("Roaming").join("Claude"))
}

/// Claude Desktop's config file, newest layout first: the `Claude-Data`
/// folder (6+), the packaged app's redirected `%APPDATA%`, then the real
/// `%APPDATA%\Claude`. Older files seed a new one and are cleaned when off.
fn claude_desktop_target(env: &RegEnv) -> Option<Target> {
    const FILE: &str = "claude_desktop_config.json";
    let local = env.localappdata.as_deref();
    let data = local.and_then(claude_data_dir).map(|d| d.join(FILE));
    let packaged = local.and_then(packaged_claude_dir).map(|d| d.join(FILE));
    let real_dir = env.appdata.as_ref().map(|a| a.join("Claude"));
    let real = real_dir
        .as_ref()
        .filter(|d| d.is_dir())
        .map(|d| d.join(FILE));
    let mut layouts: Vec<PathBuf> = [data, packaged, real].into_iter().flatten().collect();
    if layouts.is_empty() {
        return None;
    }
    let path = layouts.remove(0);
    Some(Target {
        app: AgentApp::ClaudeDesktop,
        path,
        seed: layouts.iter().find(|p| p.is_file()).cloned(),
        stale: layouts,
    })
}

/// Each installed agent's config file (only when its folder exists).
pub fn registration_targets(env: &RegEnv) -> Vec<Target> {
    let mut v: Vec<Target> = claude_desktop_target(env).into_iter().collect();
    let mut add = |app, dir: Option<PathBuf>, file: &str| {
        if let Some(d) = dir.filter(|d| d.is_dir()) {
            v.push(Target {
                app,
                path: d.join(file),
                seed: None,
                stale: Vec::new(),
            });
        }
    };
    add(
        AgentApp::Codex,
        env.home.as_ref().map(|h| h.join(".codex")),
        "config.toml",
    );
    add(
        AgentApp::Cursor,
        env.home.as_ref().map(|h| h.join(".cursor")),
        "mcp.json",
    );
    add(
        AgentApp::VsCode,
        env.appdata.as_ref().map(|a| a.join("Code").join("User")),
        "mcp.json",
    );
    v
}

/// Add (`on`) or remove our entry in one agent's config file.
pub fn apply_one(app: AgentApp, path: &Path, on: bool, exe: &str) -> Result<(), String> {
    apply_target(
        &Target {
            app,
            path: path.to_path_buf(),
            seed: None,
            stale: Vec::new(),
        },
        on,
        exe,
    )
}

/// [`apply_one`] for a [`Target`]: a missing file starts from its seed.
pub fn apply_target(t: &Target, on: bool, exe: &str) -> Result<(), String> {
    let (app, path) = (t.app, t.path.as_path());
    if !on {
        // Best effort: the agent no longer reads these files.
        for s in t.stale.iter().filter(|s| s.is_file()) {
            if let Err(e) = apply_one(app, s, false, exe) {
                tracing::warn!(error = %e, "could not clean an older agent config");
            }
        }
    }
    let read = |p: &Path| match std::fs::read_to_string(p) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("could not read {}: {e}", p.display())),
    };
    let text = match read(path)? {
        Some(t) => t,
        // Nothing of ours to remove: the agent reads the seed, untouched.
        None if !on => return Ok(()),
        None => match &t.seed {
            Some(s) => read(s)?.unwrap_or_default(),
            None => String::new(),
        },
    };
    let new = match (app, on) {
        (AgentApp::Codex, true) => register_codex_toml(&text, exe).map(Some),
        (AgentApp::Codex, false) => unregister_codex_toml(&text),
        (AgentApp::VsCode, true) => register_json(&text, "servers", exe, true).map(Some),
        (AgentApp::VsCode, false) => unregister_json(&text, "servers"),
        (_, true) => register_json(&text, "mcpServers", exe, false).map(Some),
        (_, false) => unregister_json(&text, "mcpServers"),
    }
    .map_err(|e| e.to_string())?;
    match new {
        Some(new) if new != text || !path.exists() => path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| write_atomic(path, new.as_bytes()))
            .map_err(|e| format!("could not write {}: {e}", path.display())),
        _ => Ok(()),
    }
}

/// Run `exe args` without a window, at most `limit`; success = exit 0.
/// Its output is read on threads while it runs (a full pipe would stall it).
pub fn run_cli(exe: &Path, args: &[&str], limit: std::time::Duration) -> Result<(), String> {
    use std::io::Read;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not run {}: {e}", exe.display()))?;
    let drain = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut keep = Vec::new();
            if let Some(mut r) = r {
                let mut buf = [0u8; 8192];
                while let Ok(n) = r.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    // Keep the start for the error message; drop the rest.
                    if keep.len() < 4096 {
                        keep.extend_from_slice(&buf[..n.min(4096 - keep.len())]);
                    }
                }
            }
            String::from_utf8_lossy(&keep).into_owned()
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let until = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if std::time::Instant::now() < until => {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} did not finish in {} s",
                    exe.display(),
                    limit.as_secs()
                ));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let _ = out.join();
    let err = err.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        let err: String = err.trim().chars().take(200).collect();
        Err(format!("{} failed ({status}): {err}", exe.display()))
    }
}

/// What running the `claude` CLI gave: `None` when it is not installed.
pub type CliResult = Option<Result<(), String>>;

/// One registration outcome for the Agents panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegResult {
    pub app: String,
    pub result: Result<(), String>,
}

/// Register with (or unregister from) every installed agent. `claude_cli`
/// runs `claude mcp …` with the given args and returns `None` when the
/// `claude` command isn't installed.
pub fn apply_registration(
    on: bool,
    exe: &str,
    env: &RegEnv,
    claude_cli: &dyn Fn(&[&str]) -> CliResult,
) -> Vec<RegResult> {
    let mut out: Vec<RegResult> = registration_targets(env)
        .into_iter()
        .map(|t| RegResult {
            app: t.app.label().into(),
            result: apply_target(&t, on, exe),
        })
        .collect();
    // Claude Code rewrites ~/.claude.json while it runs: only its own CLI.
    let remove = claude_cli(&["mcp", "remove", "--scope", "user", SERVER_KEY]);
    let r = if on {
        claude_cli(&[
            "mcp", "add", "--scope", "user", SERVER_KEY, "--", exe, "--mcp",
        ])
    } else {
        // "not found" is fine when switching off.
        remove.map(|_| Ok(()))
    };
    if let Some(result) = r {
        out.push(RegResult {
            app: "Claude Code".into(),
            result,
        });
    }
    out
}

/// The copy-paste snippets for other clients (no token: it's in agent.json).
pub fn snippets(exe: &str, url: &str) -> Value {
    json!({
        "stdio": serde_json::to_string_pretty(&json!({ "mcpServers": { SERVER_KEY: { "command": exe, "args": ["--mcp"] } } })).unwrap_or_default(),
        "url": serde_json::to_string_pretty(&json!({ "mcpServers": { SERVER_KEY: { "url": url,
            "headers": { "Authorization": "Bearer <token from %LOCALAPPDATA%\\page2cam\\agent.json>" } } } })).unwrap_or_default(),
    })
}

// ------------------------------------------------------------- activity

/// One agent call that changed something.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityEntry {
    /// Seconds since the Unix epoch.
    pub time: u64,
    pub agent: String,
    pub action: String,
    pub result: String,
}

/// The last [`Activity::CAP`] entries.
#[derive(Debug, Default)]
pub struct Activity {
    entries: VecDeque<ActivityEntry>,
}

impl Activity {
    pub const CAP: usize = 200;

    pub fn push(&mut self, e: ActivityEntry) {
        if self.entries.len() == Self::CAP {
            self.entries.pop_front();
        }
        self.entries.push_back(e);
    }

    /// Newest first.
    pub fn list(&self) -> Vec<ActivityEntry> {
        self.entries.iter().rev().cloned().collect()
    }

    pub fn to_json(&self) -> Value {
        Value::Array(
            self.list()
                .into_iter()
                .map(|e| json!({ "time": e.time, "agent": e.agent, "action": e.action, "result": e.result }))
                .collect(),
        )
    }
}

/// The balloon for an agent's call, if it's one the user should hear about
/// (public or leaving the PC). `running` = Page2Cam was running.
pub fn balloon_for(agent: &str, call: &BackendCall, running: bool) -> Option<String> {
    use BackendCall as C;
    // The name is the agent's own choice: marked and short, so a balloon
    // can't pass for a message from Page2Cam or Windows.
    let short: String = agent.chars().take(40).collect();
    let agent = format!("Agent “{short}”");
    Some(match call {
        C::Start if !running => format!("{agent} started Page2Cam"),
        C::Stop if running => format!("{agent} stopped Page2Cam"),
        C::ForceRestart => format!("{agent} restarted Page2Cam"),
        C::StartStream => format!("{agent} started your stream"),
        C::StopStream => format!("{agent} stopped your stream"),
        C::InstallUpdate => format!("{agent} is installing a Page2Cam update"),
        C::SetGpu(_) if running => format!("{agent} changed the GPU; Page2Cam restarts"),
        C::OpenUrl(_) | C::ShowPage { .. } | C::Navigate(_) if running => {
            format!("{agent} changed the page")
        }
        C::SetBackground(_) | C::UploadBackgroundImage { .. } if running => {
            format!("{agent} changed your background")
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_names_cannot_escape_folder() {
        let dir = Path::new("C:/Users/u/Documents/Page2Cam/pages");
        for (name, want) in [
            ("..\\..\\x", "______x"),
            ("../x", "___x"),
            ("CON", "_CON"),
            ("con", "_con"),
            ("a/b", "a_b"),
            ("", "page"),
            ("   ", "page"),
            ("agenda.html", "agenda"),
            ("Café ☕", "Caf_ _"),
            ("my agenda-2", "my agenda-2"),
        ] {
            let s = sanitize_page_name(name);
            assert_eq!(s, want, "{name:?}");
            let p = page_path(dir, name);
            assert_eq!(p.parent(), Some(dir), "{name:?}");
            assert!(p.to_string_lossy().ends_with(".html"));
        }
        let long = sanitize_page_name(&"x".repeat(200));
        assert_eq!(long.len(), 64);
        for n in ["a\u{0}b", "a:b", "a*b?", "\u{202e}gpj.html"] {
            let s = sanitize_page_name(n);
            assert!(
                s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || " _-".contains(c)),
                "{n:?} -> {s}"
            );
        }
    }

    #[test]
    fn pages_write_and_list() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("pages");
        let p = write_page(&dir, "agenda", "<p>1</p>").unwrap();
        write_page(&dir, "agenda", "<p>2</p>").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "<p>2</p>");
        write_page(&dir, "b", "x").unwrap();
        assert_eq!(
            list_pages(&dir),
            vec!["agenda".to_string(), "b".to_string()]
        );
        assert!(list_pages(&d.path().join("missing")).is_empty());
        assert_eq!(
            file_url(Path::new("C:\\a b\\p#1.html")),
            "file:///C:/a%20b/p%231.html"
        );
    }

    #[test]
    fn discovery_roundtrip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("agent.json");
        write_discovery(&p, "http://127.0.0.1:8766/mcp", "tok").unwrap();
        let t = std::fs::read_to_string(&p).unwrap();
        assert_eq!(
            parse_discovery(&t),
            Some(("http://127.0.0.1:8766/mcp".into(), "tok".into()))
        );
        remove_discovery(&p).unwrap();
        assert!(!p.exists());
        remove_discovery(&p).unwrap();
        assert_eq!(parse_discovery("{"), None);
    }

    const EXE: &str = "C:\\Program Files\\Page2Cam\\page2cam.exe";

    #[test]
    fn registration_preserves_other_entries() {
        let text = r#"{
  "globalShortcut": "Ctrl+Space",
  "mcpServers": {
    "zeta": { "command": "z" },
    "page2cam": { "command": "old.exe" },
    "alpha": { "command": "a", "args": ["1"] }
  },
  "aaa": 1
}"#;
        let out = register_json(text, "mcpServers", EXE, false).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["globalShortcut"], "Ctrl+Space");
        assert_eq!(v["aaa"], 1);
        assert_eq!(v["mcpServers"]["zeta"]["command"], "z");
        assert_eq!(v["mcpServers"]["alpha"]["args"], json!(["1"]));
        assert_eq!(
            v["mcpServers"]["page2cam"],
            json!({ "command": EXE, "args": ["--mcp"] })
        );
        // Key order kept (globalShortcut before mcpServers before aaa; zeta first).
        let g = out.find("globalShortcut").unwrap();
        let m = out.find("mcpServers").unwrap();
        let a = out.find("\"aaa\"").unwrap();
        assert!(g < m && m < a, "{out}");
        assert!(
            out.find("zeta").unwrap() < out.find("alpha").unwrap(),
            "{out}"
        );

        // Empty / missing file → just ours.
        let v: Value =
            serde_json::from_str(&register_json("", "mcpServers", EXE, false).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["page2cam"]["command"], EXE);
        // VS Code: `servers` with type stdio.
        let v: Value =
            serde_json::from_str(&register_json("{\"inputs\": []}", "servers", EXE, true).unwrap())
                .unwrap();
        assert_eq!(v["servers"]["page2cam"]["type"], "stdio");
        assert_eq!(v["inputs"], json!([]));

        let codex = "# my codex settings\nmodel = \"gpt-5\" # pinned\n\n[mcp_servers.other]\ncommand = \"o\"\n";
        let out = register_codex_toml(codex, EXE).unwrap();
        assert!(
            out.contains("# my codex settings") && out.contains("# pinned"),
            "{out}"
        );
        assert!(out.contains("[mcp_servers.other]"), "{out}");
        assert!(out.contains("[mcp_servers.page2cam]"), "{out}");
        let doc: toml::Table = out.parse().unwrap();
        assert_eq!(
            doc["mcp_servers"]["page2cam"]["command"].as_str(),
            Some(EXE)
        );
        assert_eq!(
            doc["mcp_servers"]["page2cam"]["args"][0].as_str(),
            Some("--mcp")
        );
        // Again: replaced, not doubled.
        let again = register_codex_toml(&out, "D:\\new.exe").unwrap();
        assert_eq!(
            again.matches("[mcp_servers.page2cam]").count(),
            1,
            "{again}"
        );
        let fresh = register_codex_toml("", EXE).unwrap();
        assert!(fresh.contains("[mcp_servers.page2cam]"), "{fresh}");
    }

    #[test]
    fn registration_skips_unparseable() {
        assert!(matches!(
            register_json("{ nope", "mcpServers", EXE, false),
            Err(RegError::Unparseable(_))
        ));
        assert!(matches!(
            register_json("[1]", "mcpServers", EXE, false),
            Err(RegError::Unparseable(_))
        ));
        assert!(matches!(
            register_json("{\"mcpServers\": 3}", "mcpServers", EXE, false),
            Err(RegError::Shape(_))
        ));
        assert!(register_codex_toml("model = ", EXE).is_err());
        assert!(register_codex_toml("mcp_servers = 3", EXE).is_err());
        // Through the file: untouched, error reported.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("claude_desktop_config.json");
        std::fs::write(&p, "{ // comment\n}").unwrap();
        assert!(apply_one(AgentApp::ClaudeDesktop, &p, true, EXE).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ // comment\n}");
    }

    #[test]
    fn unregister_removes_only_ours() {
        let text = register_json(
            "{\"mcpServers\": {\"x\": {\"command\": \"x\"}}, \"k\": 1}",
            "mcpServers",
            EXE,
            false,
        )
        .unwrap();
        let out = unregister_json(&text, "mcpServers").unwrap().unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v,
            json!({ "mcpServers": { "x": { "command": "x" } }, "k": 1 })
        );
        assert_eq!(unregister_json(&out, "mcpServers").unwrap(), None);
        assert_eq!(unregister_json("{}", "mcpServers").unwrap(), None);
        let codex =
            register_codex_toml("# c\n[mcp_servers.other]\ncommand = \"o\"\n", EXE).unwrap();
        let out = unregister_codex_toml(&codex).unwrap().unwrap();
        assert!(
            out.contains("# c") && out.contains("[mcp_servers.other]") && !out.contains("page2cam"),
            "{out}"
        );
        assert_eq!(unregister_codex_toml(&out).unwrap(), None);
    }

    /// The packaged (MSIX) Claude Desktop reads its own copy under
    /// `%LOCALAPPDATA%\Packages\Claude_*\LocalCache\Roaming\Claude`, and the
    /// real `%APPDATA%\Claude` file only until that copy exists.
    #[test]
    fn packaged_claude_desktop_gets_its_own_copy_seeded_from_the_real_one() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("Roaming/Claude/claude_desktop_config.json");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        let other = r#"{"mcpServers":{"other":{"command":"x"}}}"#;
        std::fs::write(&real, other).unwrap();
        let pkg = d.path().join("Local/Packages/Claude_abc123");
        std::fs::create_dir_all(pkg.join("LocalCache/Roaming")).unwrap();
        std::fs::create_dir_all(d.path().join("Local/Packages/ClaudeNot/x")).unwrap();
        let env = RegEnv {
            appdata: Some(d.path().join("Roaming")),
            home: Some(d.path().join("home")),
            localappdata: Some(d.path().join("Local")),
        };
        let private = pkg.join("LocalCache/Roaming/Claude/claude_desktop_config.json");
        let t = registration_targets(&env);
        assert_eq!(t.len(), 1, "{t:?}");
        assert_eq!(t[0].app, AgentApp::ClaudeDesktop);
        assert_eq!(t[0].path, private);
        let r = apply_registration(true, EXE, &env, &|_: &[&str]| None);
        assert!(r.iter().all(|x| x.result.is_ok()), "{r:?}");
        let text = std::fs::read_to_string(&private).unwrap();
        assert!(
            text.contains("page2cam") && text.contains("other"),
            "{text}"
        );
        assert_eq!(std::fs::read_to_string(&real).unwrap(), other);
        apply_registration(false, EXE, &env, &|_: &[&str]| None);
        let text = std::fs::read_to_string(&private).unwrap();
        assert!(
            !text.contains("page2cam") && text.contains("other"),
            "{text}"
        );
    }

    /// Claude Desktop 6+ keeps its config in
    /// `%LOCALAPPDATA%\Claude-Data\claude_<id>`; older copies of ours are
    /// cleaned up when switching off.
    #[test]
    fn claude_data_folder_wins_and_old_copies_are_cleaned_when_off() {
        let d = tempfile::tempdir().unwrap();
        let data = d.path().join("Local/Claude-Data/claude_abc123");
        std::fs::create_dir_all(&data).unwrap();
        let new = data.join("claude_desktop_config.json");
        let prefs = r#"{"coworkUserFilesPath":"x","preferences":{"a":1}}"#;
        std::fs::write(&new, prefs).unwrap();
        std::fs::write(d.path().join("Local/Claude-Data/README.txt"), "r").unwrap();
        let pkg = d
            .path()
            .join("Local/Packages/Claude_abc123/LocalCache/Roaming/Claude");
        std::fs::create_dir_all(&pkg).unwrap();
        let old = pkg.join("claude_desktop_config.json");
        let env = RegEnv {
            appdata: Some(d.path().join("Roaming")),
            home: Some(d.path().join("home")),
            localappdata: Some(d.path().join("Local")),
        };
        // An older copy of ours from before this layout was known.
        apply_one(AgentApp::ClaudeDesktop, &old, true, EXE).unwrap();
        let t = registration_targets(&env);
        assert_eq!(t.len(), 1, "{t:?}");
        assert_eq!(t[0].path, new);
        apply_registration(true, EXE, &env, &|_: &[&str]| None);
        let text = std::fs::read_to_string(&new).unwrap();
        assert!(
            text.contains("page2cam") && text.contains("coworkUserFilesPath"),
            "{text}"
        );
        apply_registration(false, EXE, &env, &|_: &[&str]| None);
        let text = std::fs::read_to_string(&new).unwrap();
        assert!(
            !text.contains("page2cam") && text.contains("preferences"),
            "{text}"
        );
        let text = std::fs::read_to_string(&old).unwrap();
        assert!(!text.contains("page2cam"), "old copy left: {text}");
    }

    #[test]
    fn registration_targets_only_existing_dirs_and_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let env = RegEnv {
            appdata: Some(d.path().join("Roaming")),
            home: Some(d.path().join("home")),
            localappdata: Some(d.path().join("Local")),
        };
        assert!(registration_targets(&env).is_empty());
        std::fs::create_dir_all(d.path().join("Roaming/Claude")).unwrap();
        std::fs::create_dir_all(d.path().join("home/.codex")).unwrap();
        let t = registration_targets(&env);
        assert_eq!(
            t.iter().map(|x| x.app).collect::<Vec<_>>(),
            vec![AgentApp::ClaudeDesktop, AgentApp::Codex]
        );
        let calls = std::cell::RefCell::new(Vec::<String>::new());
        let r = apply_registration(true, EXE, &env, &|a: &[&str]| {
            calls.borrow_mut().push(a.join(" "));
            Some(Ok(()))
        });
        assert!(r.iter().all(|x| x.result.is_ok()), "{r:?}");
        assert_eq!(r.len(), 3);
        let claude =
            std::fs::read_to_string(d.path().join("Roaming/Claude/claude_desktop_config.json"))
                .unwrap();
        assert!(claude.contains("page2cam"));
        assert!(calls
            .borrow()
            .iter()
            .any(|c| c.starts_with("mcp add --scope user page2cam -- ")));
        let r = apply_registration(false, EXE, &env, &|_: &[&str]| None);
        assert_eq!(r.len(), 2, "no claude CLI → no Claude Code row");
        let claude =
            std::fs::read_to_string(d.path().join("Roaming/Claude/claude_desktop_config.json"))
                .unwrap();
        assert!(!claude.contains("page2cam"), "{claude}");
        let codex = std::fs::read_to_string(d.path().join("home/.codex/config.toml")).unwrap();
        assert!(!codex.contains("page2cam"), "{codex}");
    }

    #[cfg(windows)]
    #[test]
    fn a_chatty_cli_cannot_stall_registration() {
        // 1 MB on stderr and stdout fills any pipe; reading only after exit
        // would wait for the full limit.
        let ps = std::path::PathBuf::from(std::env::var("SystemRoot").unwrap())
            .join("System32\\WindowsPowerShell\\v1.0\\powershell.exe");
        let t = std::time::Instant::now();
        let r = run_cli(
            &ps,
            &["-NoProfile", "-Command", "[Console]::Error.Write('e' * 1000000); [Console]::Out.Write('o' * 1000000); exit 3"],
            std::time::Duration::from_secs(20),
        );
        let e = r.unwrap_err();
        assert!(e.contains("failed") && e.contains("eee"), "{e}");
        assert!(
            t.elapsed() < std::time::Duration::from_secs(15),
            "{:?}",
            t.elapsed()
        );
    }

    #[test]
    fn activity_keeps_last_200() {
        let mut a = Activity::default();
        for i in 0..250 {
            a.push(ActivityEntry {
                time: i,
                agent: "A".into(),
                action: format!("a{i}"),
                result: "ok".into(),
            });
        }
        let l = a.list();
        assert_eq!(l.len(), 200);
        assert_eq!(l[0].action, "a249");
        assert_eq!(l[199].action, "a50");
        assert_eq!(a.to_json()[0]["time"], 249);
    }

    #[test]
    fn balloon_names_are_marked_and_short() {
        // An agent can't make a balloon read like Page2Cam or Windows itself.
        let b = balloon_for(
            "Page2Cam: your camera is hacked, call 555",
            &BackendCall::StartStream,
            true,
        )
        .unwrap();
        assert!(
            b.starts_with("Agent “Page2Cam: your camera is hacked"),
            "{b}"
        );
        let long = balloon_for(&"x".repeat(100), &BackendCall::StopStream, true).unwrap();
        assert_eq!(
            long,
            format!("Agent “{}” stopped your stream", "x".repeat(40))
        );
    }

    #[test]
    fn balloon_table() {
        use BackendCall as C;
        let a = "Claude";
        assert_eq!(
            balloon_for(a, &C::StartStream, true).as_deref(),
            Some("Agent “Claude” started your stream")
        );
        assert_eq!(
            balloon_for(a, &C::StopStream, true).as_deref(),
            Some("Agent “Claude” stopped your stream")
        );
        assert_eq!(
            balloon_for(a, &C::Start, false).as_deref(),
            Some("Agent “Claude” started Page2Cam")
        );
        assert_eq!(balloon_for(a, &C::Start, true), None);
        assert_eq!(
            balloon_for(a, &C::Stop, true).as_deref(),
            Some("Agent “Claude” stopped Page2Cam")
        );
        assert_eq!(
            balloon_for(a, &C::ForceRestart, false).as_deref(),
            Some("Agent “Claude” restarted Page2Cam")
        );
        assert_eq!(
            balloon_for(a, &C::OpenUrl("x".into()), true).as_deref(),
            Some("Agent “Claude” changed the page")
        );
        assert_eq!(balloon_for(a, &C::OpenUrl("x".into()), false), None);
        assert_eq!(
            balloon_for(a, &C::SetBackground(json!({ "full": "youtube" })), true).as_deref(),
            Some("Agent “Claude” changed your background")
        );
        assert_eq!(balloon_for(a, &C::SetBackground(json!({})), false), None);
        for c in [
            C::Status,
            C::GetOverlay,
            C::GetCrop,
            C::Snapshot,
            C::ListPages,
            C::GetBackground,
            C::ListGpus,
        ] {
            assert_eq!(balloon_for(a, &c, true), None, "{c:?}");
        }
        assert_eq!(
            balloon_for(a, &C::SetOverlay(json!({ "x": 1 })), true),
            None
        );
    }
}
