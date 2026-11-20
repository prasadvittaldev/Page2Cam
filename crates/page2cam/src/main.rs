#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod app;
mod background;
mod config;
mod engine;
mod health;
mod logging;
mod mcp_bridge;
mod pattern;
mod restart;
mod update;
mod video;

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, OnceLock};

use anyhow::Context;
use clap::Parser;
use p2c_admin::{AdminConfig, PreviewSink};
use p2c_audio::{MixSettings, Mixer};
use p2c_browser::BrowserConfig;
use p2c_compositor::{
    CpuProducer, FrameSink, OutputFormat, OutputProducer, OutputSpec, OverlayStyle, ProducerEvent,
    Ticker,
};
use p2c_core::AudioRing;
use p2c_core::{FrameSlot, Nv12Gate, Nv12Slot};
use p2c_stream::StreamController;
use p2c_webcam::{list_cameras, pick_camera, WebcamCapture, WebcamStatus};
use parking_lot::{Mutex, RwLock};

use crate::app::App;
use crate::config::Config;

#[derive(Parser, Debug)]
#[command(
    name = "page2cam",
    version,
    about = "Show a web page as a virtual webcam or live stream"
)]
struct Cli {
    /// Config file (default: <config dir>/page2cam/config.toml)
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    url: Option<String>,
    #[arg(long)]
    width: Option<u32>,
    #[arg(long)]
    height: Option<u32>,
    #[arg(long)]
    fps: Option<u32>,
    /// Virtual camera device, e.g. /dev/video5 (default: auto-detect)
    #[arg(long)]
    device: Option<PathBuf>,
    /// Webcam overlay: top-left | top-right | bottom-left | bottom-right | off
    #[arg(long)]
    pip: Option<String>,
    /// Webcam overlay width in percent of the output width (5..=50)
    #[arg(long)]
    pip_size: Option<u32>,
    /// Use the webcam whose name contains this text
    #[arg(long)]
    webcam: Option<String>,
    /// Show a moving test pattern instead of a web page
    #[arg(long)]
    test_pattern: bool,
    /// Don't open the admin page in a browser at startup
    #[arg(long)]
    no_open: bool,
    /// Admin page port (default 8765)
    #[arg(long)]
    admin_port: Option<u16>,
    /// Use a still image (PNG/JPEG) as the webcam, for testing without a camera
    #[arg(long, value_name = "IMAGE")]
    fake_webcam: Option<PathBuf>,
    /// Relaunch: wait for this (old) Page2Cam process to exit first.
    #[arg(long, hide = true, value_name = "PID")]
    wait_for_pid: Option<u32>,
    /// Relaunch after Chromium closed unexpectedly: how often in a row.
    #[arg(long, hide = true, default_value_t = 0)]
    unexpected_exit_count: u32,
    /// `gpu` setting for this run only ("cpu" after repeated GPU crashes);
    /// the saved setting is unchanged.
    #[arg(long, hide = true, value_name = "GPU")]
    gpu_override: Option<String>,
    /// Run as the tray's video engine (admin token from P2C_ADMIN_TOKEN).
    #[arg(long, hide = true, conflicts_with = "standalone")]
    engine: bool,
    /// Engine: the tray owns the virtual camera; only write frames to it.
    #[arg(long, hide = true, requires = "engine")]
    camera_from_tray: bool,
    /// Engine: exit when this (tray) process is gone.
    #[arg(long, hide = true, requires = "engine", value_name = "PID")]
    parent_pid: Option<u32>,
    /// Run on its own (own camera, console output), without the tray.
    #[arg(long)]
    standalone: bool,
}

impl Cli {
    fn role(&self) -> engine::Role {
        if self.engine {
            engine::Role::Engine
        } else {
            engine::Role::Standalone
        }
    }

    fn camera_mode(&self) -> p2c_vcam::CameraMode {
        if self.camera_from_tray {
            p2c_vcam::CameraMode::FromTray
        } else {
            p2c_vcam::CameraMode::Own
        }
    }

    fn apply(&self, mut c: Config) -> Config {
        if let Some(v) = &self.url {
            c.url = v.clone();
        }
        if let Some(v) = self.width {
            c.width = v;
        }
        if let Some(v) = self.height {
            c.height = v;
        }
        if let Some(v) = self.fps {
            c.fps = v;
        }
        if let Some(v) = &self.device {
            c.vcam_device = Some(v.clone());
        }
        if let Some(v) = &self.pip {
            c.pip = v.clone();
        }
        if let Some(v) = self.pip_size {
            c.pip_size = v;
        }
        if let Some(v) = &self.webcam {
            c.webcam = Some(v.clone());
        }
        if let Some(v) = self.admin_port {
            c.admin_port = v;
        }
        c
    }
}

fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    // Started by a desktop agent: the stdio MCP bridge (stdout is JSON-RPC
    // only; it uses the pipes the agent gave, never a console). Checked
    // first, before anything else runs: the marker lock below must exist as
    // early as possible, or a sweep starting at the same moment could stop
    // the bridge while it starts.
    if is_mcp_args(&args) {
        // Marks this process as a bridge: engine starts and the tray's
        // leftover sweeps spare it (they would cut the agent off).
        // (Linux reads the command line instead; no lock files left behind.)
        let marker =
            cfg!(windows).then(|| p2c_proc::try_lock(&p2c_proc::mcp_lock_name(std::process::id())));
        let code = mcp_bridge::run();
        drop(marker);
        std::process::exit(code);
    }
    // CEF re-launches this executable for its helper processes; handle that
    // next (they never attach a console or print; `--mcp` is never theirs).
    if let Some(code) = p2c_browser::maybe_run_subprocess() {
        std::process::exit(code);
    }
    // Windows without a role flag: the tray (no console; its own log file).
    if is_tray_args(&args, cfg!(windows)) {
        run_tray(&args);
    }
    let engine = is_engine_args(&args);
    // GUI subsystem: started from a console, standalone still prints there
    // (before any output). Double-clicked: no console, the log file only.
    let console = !engine && attach_parent_console();
    // Standalone next to a tray would fight it for the camera (and stop it
    // as a leftover): refuse, always with a message box. Before logging,
    // which would truncate the log of the engine the tray runs.
    if !engine && cfg!(windows) && p2c_tray::tray_running() {
        let msg = "Page2Cam is already running in the tray. Use the tray icon to \
                   start or stop it, or quit the tray before using --standalone.";
        eprintln!("\n{msg}");
        show_error(msg);
        std::process::exit(engine::EXIT_CRASH);
    }
    logging::init(!engine);
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            tracing::error!("{e:#}");
            if !engine {
                eprintln!("\nPage2Cam stopped: {e:#}");
                if !console {
                    show_error(&format!("Page2Cam stopped: {e:#}"));
                }
            }
            std::process::exit(engine::EXIT_CRASH);
        }
    }
}

/// `--engine` among the program's arguments (before clap parses them, to
/// decide about the console and logging).
fn is_engine_args(args: &[std::ffi::OsString]) -> bool {
    args.iter().skip(1).any(|a| a == "--engine")
}

/// `--mcp` as the only argument: the MCP bridge role.
fn is_mcp_args(args: &[std::ffi::OsString]) -> bool {
    p2c_proc::is_mcp_args(args)
}

/// The tray role: Windows and neither `--engine` nor `--standalone`.
fn is_tray_args(args: &[std::ffi::OsString], windows: bool) -> bool {
    windows
        && !args
            .iter()
            .skip(1)
            .any(|a| a == "--engine" || a == "--standalone")
}

/// Run the Windows tray and exit. Errors go to a message box (the GUI
/// subsystem has no console).
#[cfg(windows)]
fn run_tray(args: &[std::ffi::OsString]) -> ! {
    // A second launch only signals the running tray: leave its log alone
    // (opening it truncates).
    if !p2c_tray::tray_running() {
        logging::init_named(false, "page2cam-tray.log");
    }
    if args.len() > 1 {
        tracing::warn!(
            ?args,
            "the tray ignores these arguments; use --standalone to run Page2Cam without the tray"
        );
    }
    match p2c_tray::run() {
        Ok(()) => std::process::exit(engine::EXIT_OK),
        Err(e) => {
            tracing::error!("tray: {e:#}");
            show_error(&format!("Page2Cam stopped: {e:#}"));
            std::process::exit(engine::EXIT_CRASH);
        }
    }
}

#[cfg(not(windows))]
fn run_tray(_args: &[std::ffi::OsString]) -> ! {
    unreachable!("the tray exists on Windows only")
}

/// Attach to the console of the process that started us, if it has one.
#[cfg(windows)]
fn attach_parent_console() -> bool {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: no pointers; fails harmlessly without a parent console.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok()
}

#[cfg(not(windows))]
fn attach_parent_console() -> bool {
    true
}

/// A start-up error when there is no console to show it in (double-click).
#[cfg(windows)]
fn show_error(text: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    // SAFETY: valid NUL-terminated strings for the call's duration.
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from("Page2Cam"),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
fn show_error(_text: &str) {}

/// Runs Page2Cam; returns the process exit code.
fn run() -> anyhow::Result<i32> {
    let log_file = logging::current_path();
    let cli = Cli::parse();
    let role = cli.role();
    // Engine: the tray that started us is never stopped as a leftover.
    let keep: Vec<u32> = cli.parent_pid.into_iter().collect();
    // Engine: the tray's admin token, from the environment only. Removed
    // from ours so Chromium's helpers and ffmpeg don't inherit it.
    let admin_token = engine::admin_token(role, std::env::var(engine::TOKEN_ENV).ok());
    std::env::remove_var(engine::TOKEN_ENV);
    // Same for the tray endpoint's address and token (read once, now).
    let _ = p2c_admin::TrayLink::global();
    // Engine nobody can reach: stop before touching anything (no
    // stop_others, camera or Chromium).
    let admin_token = match admin_token {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("{e}");
            return Ok(engine::EXIT_CONFIG);
        }
    };
    if role == engine::Role::Engine {
        tracing::info!(
            parent = ?cli.parent_pid,
            camera_from_tray = cli.camera_from_tray,
            "running as the tray's engine"
        );
    }
    // Opened right away (a handle on Windows), so a pid reused later can't
    // pass for the tray. Already gone: exit at once, before stop_others
    // (which could otherwise stop a tray or Page2Cam started since).
    let parent = match cli.parent_pid {
        Some(pid) => match engine::Parent::open(pid) {
            Some(p) if p.alive() => Some(p),
            _ => {
                tracing::warn!(pid, "the tray is already gone; not starting");
                return Ok(engine::EXIT_OK);
            }
        },
        None => None,
    };
    // A relaunch: let the old process release the camera, port and CEF
    // profile before opening any of them.
    if let Some(pid) = cli.wait_for_pid {
        tracing::info!(pid, "waiting for the previous Page2Cam to close");
        if !restart::wait_for_pid(pid, restart::WAIT_FOR_OLD) {
            tracing::warn!(
                pid,
                "previous Page2Cam still running after 10 s; starting anyway"
            );
        }
    }
    // Leftover instances (or their orphaned Chromium helpers) would hold the
    // camera, admin port and CEF profile: stop them before opening any.
    // In engine mode the tray (`keep`; also an ancestor) is spared.
    p2c_proc::stop_others(std::process::id(), cli.wait_for_pid, &keep);
    // Only one engine at a time (two launches milliseconds apart). Held until
    // `run` returns; a relaunch's new process waits for our exit anyway.
    let _engine_lock = match p2c_proc::take_engine_lock(std::process::id(), cli.wait_for_pid, &keep)
    {
        Ok(l) => l,
        // Standalone: an error from `run`, so a double-click shows why.
        Err(e) if role == engine::Role::Standalone => {
            return Err(e.context("Page2Cam is already starting; this one exits"));
        }
        Err(e) => {
            tracing::error!("{e:#}; this one exits");
            return Ok(engine::EXIT_CRASH);
        }
    };
    let started = std::time::Instant::now();
    // CEF can kill the whole process (e.g. "GPU process isn't usable.
    // Goodbye."), and then the relaunch at the end of `run` never happens:
    // let Windows restart us after a crash instead (R38). Undone in `finish`.
    // Not in engine mode: the tray restarts the engine.
    if role.register_crash_restart() {
        let wer_args = restart::wer_restart_args(&current_args(), cli.unexpected_exit_count);
        match restart::register_crash_restart(&wer_args) {
            Ok(()) if cfg!(windows) => {
                tracing::info!(args = ?wer_args, "Windows restarts Page2Cam if it crashes after 60 s or more")
            }
            Ok(()) => {}
            Err(e) => tracing::warn!("no automatic restart after a crash: {e:#}"),
        }
    }
    let cfg = cli.apply(Config::load(cli.config.as_deref())?);
    cfg.validate()?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "Page2Cam starting {}x{}@{}",
        cfg.width,
        cfg.height,
        cfg.fps
    );
    let spec = OutputSpec {
        width: cfg.width,
        height: cfg.height,
        fps: cfg.fps,
        // Windows feeds the camera and the stream NV12 (BT.709, converted here);
        // Linux keeps BGRA (v4l2 YUYV sink, BGRA stream input).
        format: if cfg!(windows) {
            OutputFormat::Nv12
        } else {
            OutputFormat::Bgra
        },
    };

    // Outputs. The virtual camera is optional: preview and streaming still work without it.
    let mut sinks: Vec<Box<dyn FrameSink>> = Vec::new();
    let mut own_device = None;
    let mut vcam_live = p2c_vcam::CameraStatus::default();
    let vcam_status =
        match p2c_vcam::open_default_mode(cfg.vcam_device.as_deref(), &spec, cli.camera_mode()) {
            Ok(cam) => {
                tracing::info!("virtual camera: {}", cam.description);
                own_device = cam.own_device;
                vcam_live = cam.status;
                sinks.push(cam.sink);
                format!("ok: {}", cam.description)
            }
            Err(e) => {
                tracing::warn!("virtual camera unavailable: {e:#}");
                format!("error: {e:#}")
            }
        };
    let preview = FrameSlot::new();
    sinks.push(Box::new(PreviewSink::new(
        preview.clone(),
        (cfg.fps / 12).max(1),
    )));

    let page_ring = AudioRing::new(500);
    let mic_ring = AudioRing::new(500);
    let mixer = Mixer::new(
        page_ring.clone(),
        mic_ring.clone(),
        MixSettings {
            mic_on: false,
            ..MixSettings::default()
        },
    );
    let stream = StreamController::new(mixer.clone());
    sinks.push(Box::new(stream.sink()));

    let layout = Arc::new(RwLock::new(cfg.pip_layout()?));
    // Saved backgrounds. The user's page (opened at start-up unless a
    // generated background is chosen): `--url` if given, else the saved
    // `last_page_url`, else `url`.
    let last_page_url =
        background::initial_page_url(cli.url.is_some(), &cfg.last_page_url, &cfg.url);
    let (bg_state, bg_warnings) = background::BgState::from_config(&cfg, last_page_url);
    for w in &bg_warnings {
        tracing::warn!("background setting: {w}");
    }
    if bg_state.full != background::FullBg::Page && layout.read().style == OverlayStyle::Box {
        // A full-frame background needs a cut-out style (hand-edited settings).
        layout.write().style = OverlayStyle::CutoutFull;
    }
    let backdrop = Arc::new(RwLock::new(bg_state.backdrop()));
    let crop: p2c_core::SharedCrop = Arc::new(RwLock::new(cfg.crop()));
    // NV12 originals of the webcam frames (stays empty for fake/test input).
    let nv12_slot = Nv12Slot::new();
    let nv12_gate = Nv12Gate::new();
    let webcam = match &cli.fake_webcam {
        Some(img) => start_fake_webcam(img, crop.clone()),
        None => start_webcam(
            &cfg,
            own_device.as_deref(),
            nv12_slot.clone(),
            nv12_gate.clone(),
            crop.clone(),
        ),
    };
    let webcam_status = match &webcam {
        Some((_, WebcamSource::Real(cap), _)) => cap.status(),
        Some((_, WebcamSource::Fake(..), name)) => WebcamStatus::new(&format!("ok: {name}")),
        None => WebcamStatus::new("none"),
    };
    let webcam_slot = webcam.as_ref().map(|(slot, _, _)| slot.clone());

    // GPU: resolved once from the `gpu` setting against a fresh adapter list
    // (adapter LUIDs are never stored: a stale one crashes Chromium).
    let gpu_saved = app::normalize_gpu(&cfg.gpu);
    let gpu_running = match &cli.gpu_override {
        Some(o) => {
            let o = app::normalize_gpu(o);
            tracing::warn!(
                gpu = %o,
                saved = %gpu_saved,
                "GPU setting overridden for this run (saved setting unchanged)"
            );
            o
        }
        None => gpu_saved.clone(),
    };
    let adapters = p2c_d3d::enumerate();
    let (gpu_choice, gpu_note) =
        p2c_d3d::choose(&adapters, &p2c_d3d::GpuSetting::parse(&gpu_running));
    if let Some(n) = &gpu_note {
        tracing::info!("{n}");
    }
    // CPU compatibility mode keeps the 0.3.0 behaviour: the cut-out still uses
    // the high-performance GPU if there is a hardware adapter.
    let cutout_adapter = match &gpu_choice {
        p2c_d3d::GpuChoice::Gpu(a) => Some(a.clone()),
        p2c_d3d::GpuChoice::Cpu { .. } => {
            match p2c_d3d::choose(&adapters, &p2c_d3d::GpuSetting::Auto).0 {
                p2c_d3d::GpuChoice::Gpu(a) => Some(a),
                p2c_d3d::GpuChoice::Cpu { .. } => None,
            }
        }
    };
    let cutout_gpu = cutout_adapter.as_ref().map(|a| a.index);

    // Video: the GPU compositor on the chosen adapter (Chromium hands it
    // shared textures), else the CPU compositor. Decided before CEF starts.
    let video = Arc::new(Mutex::new(String::new()));
    // The browser, for the video producer to ask for a repaint (set once it exists).
    let browser_cell: Arc<OnceLock<p2c_browser::BrowserHandle>> = Arc::default();
    let video_start = start_video(&gpu_choice, spec, cli.test_pattern, &video, &browser_cell);
    // R28 forced the CPU for this run: say why (the admin offers to try the
    // GPU again).
    let gpu_override = cli.gpu_override.is_some();
    *video.lock() = if gpu_override && video_start.gpu.is_none() {
        video::status_gpu_override()
    } else {
        video_start.status.clone()
    };
    tracing::info!(
        "video: {}; cut-out GPU: {}",
        video.lock(),
        cutout_adapter
            .as_ref()
            .map_or("none (CPU)", |a| a.name.as_str())
    );

    // Cut-out: segmentation runs on its own thread and publishes the person
    // mask; the ticker combines it with the newest webcam frame.
    let matte = p2c_core::MaskSlot::new();
    let segmenter = webcam_slot.as_ref().map(|cam| {
        start_segmenter(
            &cfg,
            cam.clone(),
            nv12_slot.clone(),
            nv12_gate.clone(),
            matte.clone(),
            cutout_gpu,
        )
    });
    if let Some(seg) = &segmenter {
        let l = *layout.read();
        seg.set_enabled(l.visible && backdrop.read().needs_mask(&l));
    }
    let (events_tx, events) = mpsc::channel();
    let ticker = Ticker::spawn(
        spec,
        // CPU page frames (CPU mode, and after a fallback).
        Some(PAGE.clone()),
        webcam_slot.clone(),
        segmenter.as_ref().map(|_| matte.clone()),
        layout.clone(),
        backdrop.clone(),
        video_start.producer,
        sinks,
        events_tx,
    );
    let fallback = ticker.fallback_trigger();
    let gpu_video = video_start.gpu;

    let updater = Arc::new(update::Updater::new(
        env!("CARGO_PKG_VERSION"),
        update::is_installed(),
        Box::new(p2c_update::fetch_latest),
    ));
    // Engine mode: the tray checks for updates.
    if role.check_updates(cfg.check_updates) {
        updater.spawn_checks();
    }

    // Hardware H.264 encoder: probed in the background (a real 1-frame
    // encode per candidate); x264 until it finishes or if none works.
    let encoder_status = Arc::new(Mutex::new("probing…".to_string()));
    let app = Arc::new(App {
        browser: OnceLock::new(),
        crop,
        layout,
        backdrop,
        bg: Mutex::new(bg_state),
        apply_lock: Mutex::new(()),
        bg_base: OnceLock::new(),
        data_dir: directories::ProjectDirs::from("", "", "page2cam")
            .map(|d| d.data_dir().to_path_buf()),
        thumbs: background::ThumbCache::new(Box::new(|url| p2c_update::fetch_small(url, 8))),
        stream,
        mixer,
        mic_ring,
        mic: Mutex::new(None),
        mic_status: Mutex::new("off".into()),
        webcam_slot,
        webcam_status,
        vcam_status,
        vcam_live,
        stream_pix_fmt: match spec.format {
            OutputFormat::Nv12 => "nv12",
            OutputFormat::Bgra => "bgra",
        },
        stream_url: Mutex::new(cfg.rtmp_url.clone()),
        width: cfg.width,
        height: cfg.height,
        fps: cfg.fps,
        video_kbps: cfg.video_kbps,
        fallback_url: Mutex::new(cfg.url.clone()),
        log_file: log_file
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        segmenter,
        settings_path: config::save_path(cli.config.as_deref()),
        mic_delay_ms: Mutex::new(0),
        latencies: Mutex::new(Default::default()),
        pipeline_ms: ticker.pipeline_latency(),
        updater: updater.clone(),
        video: video.clone(),
        encoder: encoder_status.clone(),
        gpu_saved: Mutex::new(gpu_saved),
        gpu_running,
        gpu_override,
        cutout_gpu: cutout_adapter.as_ref().map(|a| a.name.clone()),
        exit: Arc::default(),
        quit_main: Mutex::new(None),
        ticker_stats: ticker.stats(),
        health_window: Mutex::new(Default::default()),
        output_frame: preview.clone(),
    });

    // Applies to the next stream start; a live stream is not restarted.
    // Hardware encoding only in GPU mode: CPU (compatibility) is x264 as in
    // 0.3.0. The probe uses the stream's own encoder options.
    {
        let vendor = match &gpu_choice {
            p2c_d3d::GpuChoice::Gpu(a) => Some(a.vendor_id),
            p2c_d3d::GpuChoice::Cpu { .. } => None,
        };
        let (fps, kbps) = (cfg.fps, cfg.video_kbps);
        let (app, status) = (app.clone(), encoder_status.clone());
        let spawned = std::thread::Builder::new()
            .name("p2c-encoder-probe".into())
            .spawn(move || {
                let found = p2c_stream::find_ffmpeg().and_then(|ff| {
                    p2c_stream::candidates(vendor)
                        .into_iter()
                        .find(|&e| p2c_stream::probe(&ff, e, fps, kbps))
                });
                let e = found.unwrap_or(p2c_stream::Encoder::X264);
                tracing::info!("stream encoder: {e}");
                app.stream.set_encoder(e);
                *status.lock() = e.to_string();
            });
        if spawned.is_err() {
            *encoder_status.lock() = p2c_stream::Encoder::X264.to_string();
        }
    }

    // The ticker switched to the CPU compositor: say so, and make Chromium
    // paint on the CPU (once the browser exists; see the `run` callback).
    // `fell_back` is held while it is set and the browser read here, and
    // while the browser is set and the flag read in the `run` callback, so
    // exactly one of the two sees the other and switches the browser.
    let fell_back = Arc::new(Mutex::new(false));
    let events_thread = {
        let (app, fell_back) = (app.clone(), fell_back.clone());
        std::thread::Builder::new()
            .name("p2c-video-events".into())
            .spawn(move || {
                // Ends when the ticker stops (its sender is dropped).
                for ev in events {
                    let ProducerEvent::Failed(reason) = ev;
                    let s = video::status_fallback(&reason);
                    tracing::warn!("video: {s}");
                    *app.video.lock() = s;
                    let mut f = fell_back.lock();
                    *f = true;
                    if let Some(b) = app.browser.get() {
                        b.recreate_cpu();
                    }
                }
            })?
    };

    // Keep the mic in step with the cut-out's processing delay.
    let quit = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let delay_thread = {
        let (app, quit) = (app.clone(), quit.clone());
        std::thread::Builder::new()
            .name("p2c-mic-delay".into())
            .spawn(move || {
                // Also the health samples, every 4th round (~1 s).
                let mut round = 0u32;
                while !quit.load(std::sync::atomic::Ordering::Relaxed) {
                    app.update_mic_delay();
                    // History moves (Back/Forward, the page itself) must not leave one
                    // of our background pages up.
                    app.keep_page_off_background();
                    if round.is_multiple_of(4) {
                        app.sample_health();
                    }
                    round = round.wrapping_add(1);
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            })?
    };

    // Engine: shut down normally once the tray is gone (checked every 1 s).
    // (If it died since the check at start, the first check here sees it.)
    if let Some(p) = parent {
        let (a, pid) = (app.clone(), p.pid());
        engine::spawn_parent_watchdog(
            p,
            quit.clone(),
            Box::new(move || a.request_shutdown(std::time::Duration::ZERO)),
        )?;
        tracing::info!(pid, "watching the tray process");
    }

    // Engine: exactly the tray's port and token (exit 76 if the port is taken).
    let admin = p2c_admin::serve(
        AdminConfig {
            port: cfg.admin_port,
            width: cfg.width,
            height: cfg.height,
            open_browser: role.open_browser(cfg.open_admin, cli.no_open),
            token: admin_token,
            strict_port: role.strict_port(),
        },
        app.clone(),
        preview,
    );
    let admin = match admin {
        Ok(a) => Ok(a),
        // Standalone: as before (no fallback port found is fatal at once).
        Err(e) if role == engine::Role::Standalone => return Err(e),
        Err(e) => Err(e),
    };
    let port_failed = admin
        .as_ref()
        .err()
        .is_some_and(|e| e.downcast_ref::<p2c_admin::PortUnavailable>().is_some());
    match &admin {
        Ok(admin) if role.print_admin_url() => {
            tracing::info!("admin page: {}", admin.url);
            println!("\n  Page2Cam is running. Control it here:\n  {}\n  (Press Ctrl+C in this window to quit.)\n", admin.url);
        }
        // Engine: the tray holds the token; keep it out of the log.
        Ok(admin) => tracing::info!("admin page on 127.0.0.1:{}", admin.addr.port()),
        Err(e) => tracing::error!(port_failed, "admin page: {e:#}"),
    }
    // Background pages are served by the admin server: a saved YouTube,
    // picture or colour background starts now that its URL is known.
    let start_url = match &admin {
        Ok(a) => app.start_background(a.bg_url.clone()),
        Err(_) => None,
    }
    .unwrap_or_else(|| app.bg.lock().last_page_url.clone());

    let result = if let Err(e) = admin {
        Err(e)
    } else if cli.test_pattern {
        pattern::spawn(PAGE.clone(), cfg.width, cfg.height);
        let (tx, rx) = mpsc::channel();
        let already = {
            let mut q = app.quit_main.lock();
            *q = Some(tx.clone());
            app.exit.user_shutdown.load(Ordering::SeqCst)
        };
        let exit = app.exit.clone();
        ctrlc::set_handler(move || {
            exit.user_shutdown.store(true, Ordering::SeqCst);
            let _ = tx.send(());
        })?;
        if !already {
            let _ = rx.recv();
        }
        Ok(())
    } else {
        let profile_dir = directories::ProjectDirs::from("", "", "page2cam")
            .context("no home directory")?
            .data_dir()
            .join("cef-profile");
        let browser_cfg = BrowserConfig {
            url: start_url.clone(),
            width: cfg.width,
            height: cfg.height,
            fps: cfg.fps,
            profile_dir,
            audio: Some(page_ring),
            // Built from a LUID enumerated again just now (R7).
            gpu_page: gpu_page(gpu_video.as_ref(), &fallback),
        };
        let watch = browser_cfg
            .gpu_page
            .is_some()
            .then(|| gpu_video.as_ref().map(|g| g.stats.clone()))
            .flatten();
        tracing::info!(url = %start_url, "loading page");
        let app2 = app.clone();
        let (quit2, fell_back2, fallback2) = (quit.clone(), fell_back.clone(), fallback.clone());
        // CEF owns the main thread until shutdown; Ctrl-C asks it to close.
        p2c_browser::run(browser_cfg, PAGE.clone(), move |handle| {
            let (h, exit) = (handle.clone(), app2.exit.clone());
            if let Err(e) = ctrlc::set_handler(move || {
                exit.user_shutdown.store(true, Ordering::SeqCst);
                h.shutdown()
            }) {
                tracing::error!(error = %e, "could not install Ctrl-C handler");
            }
            {
                // A fallback before the browser existed: switch it now (see
                // `fell_back`).
                let f = fell_back2.lock();
                // `quit_main` held: a shutdown requested before now (quit
                // route, tray gone) is seen here, a later one sees the browser.
                let q = app2.quit_main.lock();
                let _ = app2.browser.set(handle.clone());
                let _ = browser_cell.set(handle.clone());
                if *f {
                    handle.recreate_cpu();
                }
                if app2.exit.user_shutdown.load(Ordering::SeqCst) {
                    tracing::info!("shutdown requested during start-up; closing");
                    handle.shutdown();
                }
                drop(q);
            }
            if let Some(stats) = watch {
                spawn_watchdog(stats, fallback2, fell_back2, quit2);
            }
        })
    };
    tracing::info!("shutting down");
    quit.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = delay_thread.join();
    if let Some(seg) = &app.segmenter {
        seg.request_stop();
    }
    app.stream.stop();
    if let Some(m) = app.mic.lock().take() {
        m.stop();
    }
    ticker.stop();
    let _ = events_thread.join();
    stop_webcam(webcam);
    let user_shutdown = app.exit.user_shutdown.load(Ordering::SeqCst);
    let restart_requested = app.exit.restart.load(Ordering::SeqCst);
    if !role.self_relaunch() {
        // Engine: no relaunch of our own; the exit code tells the tray.
        let code = if port_failed {
            engine::EXIT_PORT
        } else {
            engine::exit_code(restart_requested, user_shutdown, result.is_ok())
        };
        if let Err(e) = &result {
            tracing::error!("engine stopped: {e:#}");
        }
        tracing::info!(
            code,
            user_shutdown,
            restart_requested,
            uptime_s = started.elapsed().as_secs(),
            "engine exits"
        );
        return Ok(code);
    }
    let action = restart::after_browser_exit(
        user_shutdown,
        restart_requested,
        cli.unexpected_exit_count,
        started.elapsed(),
    );
    finish(action, user_shutdown, &cli, started.elapsed(), result).map(|()| engine::EXIT_OK)
}

/// Log the exit decision (R28) and start the new process if there is one.
fn finish(
    action: restart::ExitAction,
    user_shutdown: bool,
    cli: &Cli,
    uptime: std::time::Duration,
    result: anyhow::Result<()>,
) -> anyhow::Result<()> {
    // An orderly exit: no crash restart by Windows (a relaunch below is ours).
    restart::unregister_crash_restart();
    match action {
        restart::ExitAction::Exit if user_shutdown => {
            tracing::info!("closed by the operator (or for an update); not restarting");
            return result;
        }
        restart::ExitAction::Exit => {
            tracing::error!(
                previous_unexpected_exits = cli.unexpected_exit_count,
                uptime_s = uptime.as_secs(),
                "not restarting: the page engine closed unexpectedly again, even in CPU mode"
            );
            return result;
        }
        restart::ExitAction::Relaunch {
            unexpected_count: 0,
            ..
        } => tracing::info!("restarting Page2Cam as requested"),
        restart::ExitAction::Relaunch {
            unexpected_count,
            gpu_cpu,
        } => {
            if let Err(e) = &result {
                tracing::error!("page engine stopped: {e:#}");
            }
            if gpu_cpu {
                tracing::warn!(
                    unexpected_exits = unexpected_count,
                    uptime_s = uptime.as_secs(),
                    "the page engine closed unexpectedly twice within 60 s of start \
                     (likely a GPU driver reset or update); restarting once in CPU mode \
                     (--gpu-override cpu; the saved GPU setting is unchanged)"
                );
            } else {
                tracing::warn!(
                    unexpected_exits = unexpected_count,
                    uptime_s = uptime.as_secs(),
                    "the page engine closed unexpectedly; restarting Page2Cam \
                     (GPUs are enumerated again)"
                );
            }
        }
    }
    let args = restart::args_for(action, &current_args(), std::process::id());
    let exe = std::env::current_exe().context("finding page2cam's own path")?;
    tracing::info!(exe = %exe.display(), ?args, "starting the new Page2Cam process");
    restart::spawn(&exe, &args).context("could not restart Page2Cam; start it again by hand")?;
    Ok(())
}

/// This process's arguments (without the program name).
fn current_args() -> Vec<String> {
    std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// The video producer chosen at start, its status text, and (GPU) the parts
/// the browser needs.
struct VideoStart {
    producer: Box<dyn OutputProducer>,
    status: String,
    gpu: Option<GpuVideo>,
}

/// The GPU path's browser side: shared textures go through `holder` to the
/// current device; `stats` feeds the watchdog.
#[cfg(windows)]
struct GpuVideo {
    adapter: p2c_d3d::AdapterInfo,
    holder: p2c_d3d::PageHolder,
    stats: Arc<video::TextureStats>,
}

/// Never built off Windows (no GPU path there).
#[allow(dead_code)]
#[cfg(not(windows))]
struct GpuVideo {
    stats: Arc<video::TextureStats>,
}

/// GPU mode: device, page input and GPU compositor on the chosen adapter.
/// Any failure here starts in CPU mode for this run (CEF has not started, so
/// Chromium simply gets no adapter switch).
/// `video` is the status text; it is updated if the producer later moves to
/// the adapter Chromium really renders on, and then `browser` is asked to
/// repaint (a static page would not send another frame by itself).
#[cfg(windows)]
fn start_video(
    choice: &p2c_d3d::GpuChoice,
    spec: OutputSpec,
    test_pattern: bool,
    video: &Arc<Mutex<String>>,
    browser: &Arc<OnceLock<p2c_browser::BrowserHandle>>,
) -> VideoStart {
    let cpu = |status: String| VideoStart {
        producer: Box::new(CpuProducer::new(spec)),
        status,
        gpu: None,
    };
    let a = match choice {
        p2c_d3d::GpuChoice::Cpu { reason } => {
            tracing::info!(%reason, "video on the CPU");
            return cpu(video::STATUS_COMPAT.into());
        }
        // The test pattern paints into the CPU page slot.
        p2c_d3d::GpuChoice::Gpu(_) if test_pattern => return cpu(video::STATUS_TEST_PATTERN.into()),
        p2c_d3d::GpuChoice::Gpu(a) => a,
    };
    let holder = p2c_d3d::PageHolder::default();
    match p2c_d3d::RecoveringProducer::new(a.luid, spec, holder.clone()) {
        Ok(mut p) => {
            // Chromium ignored the adapter switch: the producer moved to its
            // adapter (Chromium is left as it is; DirectML stays).
            let (status, browser) = (video.clone(), browser.clone());
            p.on_adapter_change(Box::new(move |chosen: &p2c_d3d::AdapterInfo| {
                let s = video::status_gpu_chromium_chose(&chosen.name);
                tracing::warn!("video: {s}");
                *status.lock() = s;
                if let Some(b) = browser.get() {
                    b.repaint();
                }
            }));
            VideoStart {
                producer: Box::new(p),
                status: video::status_gpu(&a.name),
                gpu: Some(GpuVideo {
                    adapter: a.clone(),
                    holder,
                    stats: Arc::default(),
                }),
            }
        }
        Err(e) => {
            let reason = format!("GPU {} unusable: {e:#}", a.name);
            tracing::warn!("{reason}");
            cpu(video::status_fallback(&reason))
        }
    }
}

#[cfg(not(windows))]
fn start_video(
    _choice: &p2c_d3d::GpuChoice,
    spec: OutputSpec,
    _test_pattern: bool,
    _video: &Arc<Mutex<String>>,
    _browser: &Arc<OnceLock<p2c_browser::BrowserHandle>>,
) -> VideoStart {
    VideoStart {
        producer: Box::new(CpuProducer::new(spec)),
        status: video::STATUS_COMPAT.into(),
        gpu: None,
    }
}

/// Chromium's GPU settings, or `None` (CPU paints). The LUID is checked
/// against a fresh enumeration right before CEF starts: a LUID Chromium can't
/// find crashes it, so a vanished adapter means CPU mode instead.
#[cfg(windows)]
fn gpu_page(
    gpu: Option<&GpuVideo>,
    fallback: &p2c_compositor::FallbackTrigger,
) -> Option<p2c_browser::GpuPage> {
    let g = gpu?;
    if !p2c_d3d::enumerate()
        .iter()
        .any(|a| a.luid == g.adapter.luid && !a.software)
    {
        fallback.fall_back(format!("GPU {} is no longer present", g.adapter.name));
        return None;
    }
    let (holder, stats) = (g.holder.clone(), g.stats.clone());
    #[allow(unused_mut)]
    let mut adapter_luid = g.adapter.luid;
    // Debug builds: `P2C_FAKE_BAD_LUID=1` hands Chromium a LUID no adapter
    // has, to see what CEF does with one (R38).
    #[cfg(debug_assertions)]
    if std::env::var_os("P2C_FAKE_BAD_LUID").is_some_and(|v| v == "1") {
        adapter_luid = 0x7ffe_0000_0000_beef;
        tracing::warn!(
            luid = format_args!("{adapter_luid:#x}"),
            "P2C_FAKE_BAD_LUID: giving Chromium a LUID no adapter has"
        );
    }
    Some(p2c_browser::GpuPage {
        adapter_luid,
        on_texture: Arc::new(
            move |handle, _w, _h| match holder.on_shared_texture(handle) {
                Ok(()) => {
                    stats.record_ok();
                    true
                }
                Err(e) => {
                    stats.record_err(format!("{e:#}"));
                    false
                }
            },
        ),
    })
}

#[cfg(not(windows))]
fn gpu_page(
    _gpu: Option<&GpuVideo>,
    _fallback: &p2c_compositor::FallbackTrigger,
) -> Option<p2c_browser::GpuPage> {
    None
}

/// Watches Chromium's shared textures from browser creation on: none within
/// 5 s, or 10 failed opens in a row → CPU fallback (the ticker switches and
/// reports it; the events thread recreates the browser with CPU paints).
fn spawn_watchdog(
    stats: Arc<video::TextureStats>,
    fallback: p2c_compositor::FallbackTrigger,
    fell_back: Arc<Mutex<bool>>,
    quit: Arc<std::sync::atomic::AtomicBool>,
) {
    let start = std::time::Instant::now();
    let spawned = std::thread::Builder::new()
        .name("p2c-gpu-watchdog".into())
        .spawn(move || {
            use std::sync::atomic::Ordering::SeqCst;
            while !quit.load(SeqCst) && !*fell_back.lock() {
                if let Some(reason) = stats.verdict(start.elapsed()) {
                    fallback.fall_back(reason);
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "could not start the GPU watchdog");
    }
}

/// Composited page frames. A static so the browser and ticker share one slot.
static PAGE: std::sync::LazyLock<FrameSlot> = std::sync::LazyLock::new(FrameSlot::new);

/// Start capturing the overlay webcam, never picking our own output device.
/// Any failure just means no PIP.
fn start_webcam(
    cfg: &Config,
    own_device: Option<&Path>,
    nv12: Nv12Slot,
    gate: Nv12Gate,
    crop: p2c_core::SharedCrop,
) -> Option<(FrameSlot, WebcamSource, String)> {
    if !cfg.pip_layout().ok()?.visible {
        return None;
    }
    let cams = list_cameras()
        .map_err(|e| tracing::warn!(error = %e, "could not list webcams; running without PIP"))
        .ok()?;
    let Some(cam) = pick_camera(&cams, cfg.webcam.as_deref(), own_device) else {
        tracing::warn!("no webcam found; running without PIP");
        return None;
    };
    let slot = FrameSlot::new();
    match WebcamCapture::start(&cam, own_device, slot.clone(), nv12, gate, crop) {
        Ok(cap) => {
            tracing::info!(webcam = %cam.name, "webcam on");
            Some((slot, WebcamSource::Real(cap), cam.name.clone()))
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "webcam failed; running without PIP");
            None
        }
    }
}

/// A running webcam source.
enum WebcamSource {
    Real(WebcamCapture),
    /// `--fake-webcam`: a still image re-published at 30 fps.
    Fake(
        Arc<std::sync::atomic::AtomicBool>,
        std::thread::JoinHandle<()>,
    ),
}

fn stop_webcam(webcam: Option<(FrameSlot, WebcamSource, String)>) {
    match webcam.map(|(_, src, _)| src) {
        Some(WebcamSource::Real(cap)) => cap.stop(),
        Some(WebcamSource::Fake(stop, h)) => {
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = h.join();
        }
        None => {}
    }
}

/// `--fake-webcam`: decode `img` once and publish it like a 30 fps camera.
fn start_fake_webcam(
    img: &Path,
    crop: p2c_core::SharedCrop,
) -> Option<(FrameSlot, WebcamSource, String)> {
    let rgba = match image::open(img) {
        Ok(i) => i.to_rgba8(),
        Err(e) => {
            tracing::warn!(image = %img.display(), "fake webcam image unreadable: {e}");
            return None;
        }
    };
    let (w, h) = rgba.dimensions();
    let mut data = rgba.into_raw();
    for px in data.as_chunks_mut::<4>().0 {
        px.swap(0, 2); // RGBA -> BGRA
    }
    let frame = p2c_core::Frame::from_bgra(w, h, data).ok()?;
    let slot = FrameSlot::new();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (s2, stop2) = (slot.clone(), stop.clone());
    let handle = std::thread::spawn(move || {
        while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
            let c = *crop.read();
            s2.put(p2c_core::apply_crop(frame.clone(), &c));
            std::thread::sleep(std::time::Duration::from_millis(33));
        }
    });
    let name = format!("fake: {}", img.display());
    tracing::info!(webcam = %name, "webcam on");
    Some((slot, WebcamSource::Fake(stop, handle), name))
}

/// Load ONNX Runtime and start the cut-out thread (disabled until a cut-out
/// style is chosen). If the runtime can't load, the thread reports why.
fn start_segmenter(
    cfg: &Config,
    webcam: FrameSlot,
    nv12: Nv12Slot,
    gate: Nv12Gate,
    matte: p2c_core::MaskSlot,
    gpu: Option<u32>,
) -> p2c_segment::Segmenter {
    let runtime_dir = p2c_segment::runtime_dir();
    let models = p2c_segment::models_dir();
    let loaded = p2c_segment::runtime::init(&runtime_dir).map_err(|e| format!("{e:#}"));
    match &loaded {
        Ok(()) => tracing::info!(dir = %runtime_dir.display(), "ONNX Runtime loaded"),
        Err(e) => tracing::warn!("cut-out unavailable: {e}"),
    }
    let choice = p2c_segment::EngineChoice::parse(&cfg.cutout_engine).unwrap_or_default();
    let opener: p2c_segment::Opener = Box::new(move |c| match &loaded {
        Ok(()) => p2c_segment::open_engine(&models, c, gpu),
        Err(e) => anyhow::bail!("{e}"),
    });
    p2c_segment::Segmenter::spawn_nv12(opener, webcam, nv12, gate, matte, choice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_args_parse() {
        let c = Cli::try_parse_from([
            "page2cam",
            "--engine",
            "--camera-from-tray",
            "--admin-port",
            "8794",
            "--parent-pid",
            "4321",
        ])
        .unwrap();
        assert!(c.engine && c.camera_from_tray && !c.standalone);
        assert_eq!(c.admin_port, Some(8794));
        assert_eq!(c.parent_pid, Some(4321));
        assert_eq!(c.role(), engine::Role::Engine);
        assert_eq!(c.camera_mode(), p2c_vcam::CameraMode::FromTray);

        // No role flag: the Windows tray (dispatched in `main` before
        // parsing, see `is_tray_args`); everywhere else standalone.
        let c = Cli::try_parse_from(["page2cam", "--no-open"]).unwrap();
        assert_eq!(c.role(), engine::Role::Standalone);
        assert_eq!(c.camera_mode(), p2c_vcam::CameraMode::Own);
        let c = Cli::try_parse_from(["page2cam", "--standalone"]).unwrap();
        assert_eq!(c.role(), engine::Role::Standalone);

        // Engine without the tray's camera: owns it.
        let c = Cli::try_parse_from(["page2cam", "--engine"]).unwrap();
        assert_eq!(c.camera_mode(), p2c_vcam::CameraMode::Own);

        // Contradictions and engine-only flags without --engine are refused.
        assert!(Cli::try_parse_from(["page2cam", "--engine", "--standalone"]).is_err());
        assert!(Cli::try_parse_from(["page2cam", "--camera-from-tray"]).is_err());
        assert!(Cli::try_parse_from(["page2cam", "--parent-pid", "1"]).is_err());
        // The token never comes from the command line.
        assert!(Cli::try_parse_from(["page2cam", "--engine", "--token", "x"]).is_err());
    }

    #[test]
    fn engine_flag_seen_before_parsing() {
        let a = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert!(is_engine_args(&a(&["page2cam", "--engine", "--fps", "30"])));
        assert!(!is_engine_args(&a(&["page2cam", "--standalone"])));
        assert!(!is_engine_args(&a(&["page2cam"])));
        // Only the program's own args count, not a value that looks like it.
        assert!(!is_engine_args(&a(&["--engine"])));
    }

    #[test]
    fn tray_is_the_default_on_windows_only() {
        let a = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert!(is_tray_args(&a(&["page2cam"]), true));
        // Other flags without a role still mean the tray (ignored there).
        assert!(is_tray_args(&a(&["page2cam", "--no-open"]), true));
        assert!(!is_tray_args(&a(&["page2cam", "--standalone"]), true));
        // The MCP bridge: exactly `--mcp`.
        assert!(is_mcp_args(&a(&["page2cam", "--mcp"])));
        assert!(!is_mcp_args(&a(&["page2cam"])));
        assert!(!is_mcp_args(&a(&["page2cam", "--engine", "--mcp"])));
        assert!(!is_tray_args(
            &a(&["page2cam", "--engine", "--parent-pid", "1"]),
            true
        ));
        // Linux (and any non-Windows build): never the tray.
        assert!(!is_tray_args(&a(&["page2cam"]), false));
    }
}
