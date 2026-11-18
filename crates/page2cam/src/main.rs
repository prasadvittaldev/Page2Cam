mod app;
mod config;
mod logging;
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
    CpuProducer, FrameSink, OutputFormat, OutputProducer, OutputSpec, ProducerEvent, Ticker,
};
use p2c_core::AudioRing;
use p2c_core::FrameSlot;
use p2c_stream::StreamController;
use p2c_webcam::{list_cameras, pick_camera, WebcamCapture};
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
}

impl Cli {
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

fn main() -> anyhow::Result<()> {
    // CEF re-launches this executable for its helper processes; handle that first.
    if let Some(code) = p2c_browser::maybe_run_subprocess() {
        std::process::exit(code);
    }
    logging::init();
    if let Err(e) = run() {
        tracing::error!("{e:#}");
        if cfg!(windows) {
            // Keep the console open long enough to read the error when double-clicked.
            eprintln!("\nPage2Cam stopped: {e:#}\nPress Enter to close.");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
        return Err(e);
    }
    Ok(())
}

fn run() -> anyhow::Result<()> {
    let log_file = logging::current_path();
    let cli = Cli::parse();
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
    let started = std::time::Instant::now();
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
    let vcam_status = match p2c_vcam::open_default(cfg.vcam_device.as_deref(), &spec) {
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
    let webcam = match &cli.fake_webcam {
        Some(img) => start_fake_webcam(img),
        None => start_webcam(&cfg, own_device.as_deref()),
    };
    let webcam_status = match &webcam {
        Some((_, _, name)) => format!("ok: {name}"),
        None => "none".to_string(),
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
    let video_start = start_video(&gpu_choice, spec, cli.test_pattern, &video);
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
    let segmenter = webcam_slot
        .as_ref()
        .map(|cam| start_segmenter(&cfg, cam.clone(), matte.clone(), cutout_gpu));
    if let Some(seg) = &segmenter {
        let l = *layout.read();
        seg.set_enabled(l.visible && l.style.is_cutout());
    }
    let (events_tx, events) = mpsc::channel();
    let ticker = Ticker::spawn(
        spec,
        // CPU page frames (CPU mode, and after a fallback).
        Some(PAGE.clone()),
        webcam_slot.clone(),
        segmenter.as_ref().map(|_| matte.clone()),
        layout.clone(),
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
    if cfg.check_updates {
        updater.spawn_checks();
    }

    // Hardware H.264 encoder: probed in the background (a real 1-frame
    // encode per candidate); x264 until it finishes or if none works.
    let encoder_status = Arc::new(Mutex::new("probing…".to_string()));
    let app = Arc::new(App {
        browser: OnceLock::new(),
        layout,
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
                while !quit.load(std::sync::atomic::Ordering::Relaxed) {
                    app.update_mic_delay();
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            })?
    };

    let admin = p2c_admin::serve(
        AdminConfig {
            port: cfg.admin_port,
            width: cfg.width,
            height: cfg.height,
            open_browser: cfg.open_admin && !cli.no_open,
        },
        app.clone(),
        preview,
    )?;
    tracing::info!("admin page: {}", admin.url);
    println!("\n  Page2Cam is running. Control it here:\n  {}\n  (Press Ctrl+C in this window to quit.)\n", admin.url);

    let result = if cli.test_pattern {
        pattern::spawn(PAGE.clone(), cfg.width, cfg.height);
        let (tx, rx) = mpsc::channel();
        *app.quit_main.lock() = Some(tx.clone());
        let exit = app.exit.clone();
        ctrlc::set_handler(move || {
            exit.user_shutdown.store(true, Ordering::SeqCst);
            let _ = tx.send(());
        })?;
        let _ = rx.recv();
        Ok(())
    } else {
        let profile_dir = directories::ProjectDirs::from("", "", "page2cam")
            .context("no home directory")?
            .data_dir()
            .join("cef-profile");
        let browser_cfg = BrowserConfig {
            url: cfg.url.clone(),
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
        tracing::info!(url = %cfg.url, "loading page");
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
                let _ = app2.browser.set(handle.clone());
                if *f {
                    handle.recreate_cpu();
                }
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
    let action = restart::after_browser_exit(
        user_shutdown,
        app.exit.restart.load(Ordering::SeqCst),
        cli.unexpected_exit_count,
        started.elapsed(),
    );
    finish(action, user_shutdown, &cli, started.elapsed(), result)
}

/// Log the exit decision (R28) and start the new process if there is one.
fn finish(
    action: restart::ExitAction,
    user_shutdown: bool,
    cli: &Cli,
    uptime: std::time::Duration,
    result: anyhow::Result<()>,
) -> anyhow::Result<()> {
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
    let current: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let args = restart::args_for(action, &current, std::process::id());
    let exe = std::env::current_exe().context("finding page2cam's own path")?;
    tracing::info!(exe = %exe.display(), ?args, "starting the new Page2Cam process");
    restart::spawn(&exe, &args).context("could not restart Page2Cam; start it again by hand")?;
    Ok(())
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
/// the adapter Chromium really renders on.
#[cfg(windows)]
fn start_video(
    choice: &p2c_d3d::GpuChoice,
    spec: OutputSpec,
    test_pattern: bool,
    video: &Arc<Mutex<String>>,
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
            let status = video.clone();
            p.on_adapter_change(Box::new(move |chosen: &p2c_d3d::AdapterInfo| {
                let s = video::status_gpu_chromium_chose(&chosen.name);
                tracing::warn!("video: {s}");
                *status.lock() = s;
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
    Some(p2c_browser::GpuPage {
        adapter_luid: g.adapter.luid,
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
    match WebcamCapture::start(&cam, slot.clone()) {
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
fn start_fake_webcam(img: &Path) -> Option<(FrameSlot, WebcamSource, String)> {
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
            s2.put(frame.clone());
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
    p2c_segment::Segmenter::spawn(opener, webcam, matte, choice)
}
