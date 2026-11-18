# Live streaming to YouTube (RTMP) and A/V sync for Page2Cam's ffmpeg child process

Scope: the encoder and ingest settings, how frames and audio are fed to ffmpeg, A/V sync, recovering from network drops, and alternatives to an ffmpeg child process. Researched 2026-11-18.

What the code does now (read from `crates/p2c-stream/src/args.rs`, `controller.rs`, `crates/p2c-audio/src/mixer.rs`, `resample.rs`):
- **Video input:** `-f rawvideo -pix_fmt bgra -s WxH -framerate 30 -i pipe:0`, with `-probesize 32 -analyzeduration 0 -thread_queue_size 512`.
- **Audio input:** `-f f32le -ar 48000 -ac 2 -i tcp://127.0.0.1:<port>`, with the same probe and queue flags.
- **Encode:** `libx264 -preset veryfast -tune zerolatency -pix_fmt yuv420p -b:v K -maxrate K -bufsize 2K -g 2*fps -keyint_min 2*fps -sc_threshold 0`, then `aac -b:a 128k -ar 48000`, out through `-f flv rtmp://...`.
- **Video writer:** sends one frame per 1/fps against an absolute deadline. If it falls more than 1 s behind, it restarts its clock (`start = now; n = 0`).
- **Audio writer:** sends 10 ms (480 frames) per tick against an absolute deadline. It never resets its clock, so after a stall it writes in a burst to catch up.
- **Mixer:** fills gaps with silence. The rings are `AudioRing::new(500)` in `page2cam/src/main.rs` and drop the oldest data on overflow. The mic delay keeps a target backlog and skips ahead when the backlog grows 20 ms past the target. The mic's `Resampler` uses a fixed nominal ratio and does not correct drift.
- **Bundled ffmpeg:** `dist/Page2Cam-windows-x64/ffmpeg.exe` reports itself as `n8.1.3-35-gf1acb0a266-20261117`. A string scan of the binary finds h264_nvenc, h264_qsv, h264_amf, h264_mf, h264_vulkan, hevc_nvenc, hevc_mf, av1_nvenc, av1_amf, av1_qsv and libx264, plus the `fifo` muxer options (`attempt_recovery`, `recover_any_error`) and `rtmp_enhanced_codecs` (each found with `grep -c -a`). A string being present suggests the encoder is compiled in; it does not prove it works at runtime.

Local measurements on this dev box (Ryzen 5 8600G with 12 threads, GeForce RTX 5060 Ti, Radeon 760M iGPU; Linux ffmpeg 7.0.2):
- **Method:** 300 frames (10 s) of `testsrc2` at 1080p30, encoded from a file to `-f null`. The CPU time includes reading the file. The test pattern is easier to encode than real web content.
- **Read only, BGRA (2.5 GB file), no encode:** 0.98 s user + 0.68 s sys.
- **BGRA to yuv420p conversion only (swscale inside ffmpeg):** 3.32 s user + 0.87 s sys. The conversion alone costs about 0.25 of a core for 1080p30.
- **libx264 veryfast + zerolatency, 4500k CBR, BGRA input (the current setup):** 7.15 s user + 0.99 s sys for 10 s of video, about 0.8 of a core on this desktop CPU.
  - superfast: 6.17 + 1.01 s
  - ultrafast: 5.19 + 0.96 s
- **libx264 veryfast with NV12 input:** 4.39 + 0.45 s. That is about 40% less CPU than BGRA input, because ffmpeg skips the swscale step and reads less data.
- **h264_nvenc p5 / tune ll / CBR, BGRA input:** 1.04 + 1.36 s. That is barely above the read-only baseline: NVENC accepts BGRA directly, so the GPU does the color conversion.
- **h264_nvenc, NV12 input:** 0.75 + 0.46 s.
- **h264_vaapi (renderD128, inferred to be the AMD iGPU), BGRA input with `-vf format=nv12,hwupload`:** 3.45 + 0.84 s. The CPU still pays for the swscale BGRA to NV12 step.
- **Like-for-like CPU:** gross (including the 1.66 CPU-s file read), x264 veryfast with BGRA used about 0.81 core-s/s and NVENC with BGRA about 0.24. Net of the read, that is about 0.65 versus 0.07.
- **Interpretation:** on a 4-core or 6-core laptop CPU, x264 veryfast plus the conversion should take a much larger share of the CPU than on this desktop. That is an inference; no laptop was measured.
- **Encoder pixel formats** (from `ffmpeg -h encoder=...`, ffmpeg 7.0.2):
  - h264_nvenc: `yuv420p nv12 p010le yuv444p ... bgr0 bgra rgb0 rgba ... cuda`
  - h264_qsv: `nv12 qsv`
  - h264_vaapi: `vaapi` only (needs `hwupload`)

## Q1: Which hardware encoders can ffmpeg use on Windows 11 and Linux, how should the app pick one, and with what settings? What does YouTube recommend?

### Takeaway
Probe the encoders once at startup in this order:
- **Windows:** h264_nvenc, then h264_qsv, then h264_amf, then h264_mf, then libx264.
- **Linux:** h264_nvenc, then h264_vaapi (or h264_qsv), then libx264.

For every encoder use CBR, a 2 s GOP (`-g 60` at 30 fps), and CABAC with the High profile. Measured here, NVENC takes video encoding almost entirely off the CPU: about 0.65 core down to about 0.07, net of file reading. The current 4500 kbps is below YouTube's "Minimum Bitrate Setting" for H.264 1080p30, which is 5 Mbps ("Recommended" 14 Mbps). The table's column names were confirmed with a verbatim re-fetch.

### Cited Findings

**YouTube's recommended live encoder settings** ([YouTube Help: Choose live encoder settings, bitrates, and resolutions](https://support.google.com/youtube/answer/2853702?hl=en), fetched 2026-11; the page shows no date)
- **Protocol and codecs:** RTMP/RTMPS, with RTMPS recommended for encryption. Accepted codecs are H.264, H.265/HEVC and AV1. HLS is the fallback when an encoder can't send HDR over RTMP.
- **Keyframes:** every 2 s recommended, never more than 4 s. Use 2 B-frames and 1 reference frame, progressive, CABAC, Rec. 709 8-bit for SDR, square pixels, CBR, up to 60 fps.
- **Bitrate for 1080p30:** H.264 minimum 5 Mbps, recommended 14 Mbps. AV1/HEVC minimum 4 Mbps, recommended 10 Mbps.
- **Bitrate for 720p30:** H.264 minimum 3 Mbps, recommended 8 Mbps. AV1/HEVC minimum 2 Mbps, recommended 6 Mbps.
- **Audio:** AAC or MP3, 128 kbps stereo. The page lists "44.1 KHz for stereo audio, 48 KHz for 5.1 surround sound". Page2Cam sends 48 kHz stereo, which is not what the page lists for stereo.

**HEVC and AV1 over RTMP (Enhanced RTMP)**
- The German version of the YouTube page lists H.264, HEVC and AV1 for RTMP/RTMPS — [YouTube Help (de)](https://support.google.com/youtube/answer/2853702?hl=de)
- YouTube added HEVC/AV1 ingest through Veovera's Enhanced RTMP spec as a beta in 2023. OBS 29.1 shipped it and tested NVENC, QSV and AMF AV1. These are 2023 sources (search snippets; the pages were not fetched) — [OBS PR #8522](https://github.com/obsproject/obs-studio/pull/8522); [Tom's Hardware](https://www.tomshardware.com/news/av1-live-streaming-is-finally-coming-to-youtube); [WMSPanel blog](https://blog.wmspanel.com/2023/05/enhanced-rtmp-hevc-av1.html?m=1)
- FFmpeg can publish Enhanced RTMP/FLV from version 6.1 (Flussonic says 6.1.1 or later) — [Alibaba Cloud docs](https://help.aliyun.com/en/live/use-cases/enhanced-rtmp); [Flussonic](https://flussonic.com/doc/live/restream-to-youtube/)
- Locally, `ffmpeg -h protocol=rtmp` (7.0.2) lists `-rtmp_enhanced_codecs <string>: Specify the codec(s) to use in an enhanced rtmp live stream` (local tool output).
- One OBS forum thread reports buffering when streaming AV1 to YouTube over RTMPS. It is an anecdote with no date checked (search snippet) — [OBS forum](https://obsproject.com/forum/threads/av1-streaming-on-youtube-rtmps-always-leads-to-buffering-stream.169397/)

**NVENC**
- NVIDIA's recommended settings for low-latency use (game streaming, video conferencing): ultra-low or low-latency tuning, CBR, multipass quarter or full ("evaluate and decide"), a very small VBV (one frame), unidirectional B-frames, infinite GOP, AQ. The separate "game-casting" row recommends look-ahead — [NVENC Programming Guide, SDK 13.0](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/index.html)
- (Search snippets) Community guides for streaming to YouTube or Twitch with NVENC: CBR, keyframe 2, preset P5 or P6, look-ahead and psycho-visual tuning on, High profile, 2 B-frames. Presets P5 to P7 are said to cost little GPU. These are secondary sources and opinions differ — [OBS forum thread](https://obsproject.com/forum/threads/nvenc-streaming-preset-and-multipass-mode-what-settings-are-correct-for-streaming.163979); [NVIDIA broadcasting guide (older, OBS 23 era)](https://www.nvidia.com/en-gb/geforce/news/broadcasting-guide/print/)
- ffmpeg's h264_nvenc options (local `-h encoder=h264_nvenc`): `-preset p1..p7` (default p4), `-tune hq|ll|ull|lossless`, `-rc cbr`, `-multipass 0..2`, `-rc-lookahead`, `-spatial-aq`, `-zerolatency`, `-forced-idr`, `-no-scenecut`.

**AMD AMF** ([GPUOpen AMF wiki: Recommended FFmpeg Encoder Settings](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/wiki/Recommended-FFmpeg-Encoder-Settings), edited 2025-03-27)
- **Live streaming profile:** `-c:v h264_amf -preset quality -rc vbr_peak -b:v 4M -maxrate 8M -bufsize 8M -vbaq true -preencode true -g 120 -high_motion_quality_boost_enable true -bf 2`
- **Low-latency (conference/webcam) profile:** `-rc cbr`, a small bufsize, no B-frames, `-intra_refresh_mb`. YouTube asks for CBR, so the cbr variant with `-g 60` fits better.

**Media Foundation (h264_mf)**
- Options are `-rate_control` (including `cbr`), `-quality 0..100`, `-scenario`, and `-hw_encoding 1`, which you must set to force hardware encoding — [FFmpeg docs mirror](https://git.terata.org/eessppeelllloo/ffmpeg/commit/4e556b0c0c5b152832ca059d88c654787fca585d)
- One user reports there is no profile option — [Shotcut forum](https://forum.shotcut.org/t/how-to-set-profile-to-high-when-encoding-with-h264-mf-media-foundation/43418)

**Other**
- QSV takes `nv12` or `qsv` surfaces. VAAPI needs `-vaapi_device /dev/dri/renderDxxx -vf format=nv12,hwupload`. Verified locally: VAAPI ran on renderD128; renderD129 (inferred to be the NVIDIA node) failed with "Input/output error". This shows that **device selection must be probed**, not assumed (local test).
- The official ffmpeg wiki page on hardware acceleration (trac.ffmpeg.org/wiki/HWAccelIntro) was behind an Anubis bot wall and could not be fetched.

### Inferences
- **Probe order and method** (for `find_ffmpeg`'s companion): run a tiny test encode per candidate encoder:
  `ffmpeg -hide_banner -f lavfi -i color=black:s=1920x1080:r=30 -frames:v 3 -c:v <enc> [enc opts] -f null -`
  The first encoder that exits with code 0 wins. Cache the result in the config and show it on the admin page ("encoder: h264_nvenc"). A non-zero exit carries a stderr reason, which fits the "self-explaining failures" rule.
  - Order: NVENC, QSV, AMF, MF (Windows only, last because it has the fewest controls), VAAPI (Linux, try each `/dev/dri/renderD*`), then libx264.
  - Being compiled in proves nothing: NVENC fails without an NVIDIA driver, and AMF fails without the AMD driver. Probing costs about 0.5 to 2 s per candidate; do it in the background at startup.
- **Settings:**
  - **NVENC:** `-c:v h264_nvenc -preset p5 -tune hq -rc cbr -b:v K -maxrate K -bufsize 2K -g 60 -bf 2 -profile:v high -spatial-aq 1`. Add `-rc-lookahead 8` if the GPU keeps up.
  - **QSV:** `-c:v h264_qsv -preset medium -b:v K -maxrate K -bufsize 2K -g 60 -bf 2 -look_ahead 0`. QSV CBR is selected when `b:v == maxrate`. That is standard ffmpeg-qsv behavior I did not verify against a source this session.
  - **AMF:** `-c:v h264_amf -usage transcoding -quality quality -rc cbr -b:v K -bufsize 2K -g 60 -bf 2 -vbaq true`
  - **h264_mf:** `-hw_encoding 1 -rate_control cbr -b:v K -g 60`
  - **VAAPI:** `-rc_mode CBR -b:v K -maxrate K -bufsize 2K -g 60`
  - **Every encoder:** set a fixed GOP. Hardware encoders do not honor `-keyint_min` and `-sc_threshold` the way x264 does. NVENC needs `-no-scenecut 1` only when look-ahead is on, and `-forced-idr 1` keeps keyframes IDR.
- **Drop `-tune zerolatency` for x264.** YouTube's own latency is seconds, so x264's ~0.5–1 s of look-ahead and frame threading costs nothing that matters. zerolatency disables B-frames and look-ahead, and YouTube recommends 2 B-frames. The expected gain is better quality at the same bitrate, plausibly 10–20%, but that figure is my estimate and was not measured. Caveat: encoder delay shifts video PTS consistently, so it does not cause A/V desync.
- **Bitrate:** 4500 kbps 1080p30 is below YouTube's 5 Mbps H.264 minimum. YouTube may show "bitrate too low" warnings. Offer 720p30 at 4–6 Mbps as the safe default for laptops on weak uplinks, and 1080p30 at 6–9 Mbps where the uplink allows. Mostly-static web pages compress far better than games, so 1080p at 6 Mbps is likely to look good.
- **HEVC/AV1 via `rtmp_enhanced_codecs`:** worth it only with a hardware HEVC/AV1 encoder, for roughly 30% lower bitrate at the same quality (consistent with YouTube's lower minimums for AV1/HEVC). This is a low priority because of the reported beta status, buffering reports, and test burden.
- **RTMPS:** YouTube recommends it. ffmpeg's `rtmps://` needs TLS support in the build; the BtbN GPL builds include it. That is an inference I did not verify. The URL is typically `rtmps://a.rtmps.youtube.com:443/live2`.

### Gaps
- I found no reliable published benchmark of x264 veryfast 1080p30 CPU use on a laptop. The figures above are this desktop's numbers on a synthetic pattern.
- YouTube's English page shows no "last updated" date, and I could not confirm whether Enhanced RTMP HEVC/AV1 is still "beta" in late 2026.
- AMF's accepted input pixel formats (whether it takes BGRA) and h264_mf's runtime behavior on Windows 11 could not be tested from Linux.
- The exact QSV CBR semantics in ffmpeg 8.1 are not confirmed by a fetched source.
- None of the QSV, AMF, MF or fifo command lines in this note were run against the bundled n8.1 build. Only NVENC, VAAPI and x264 were run, on Linux ffmpeg 7.0.2.
- Using `rtmps://` needs TLS in the bundled build; this is not verified.

## Q2: How should frames and audio be fed to ffmpeg, and how should the app recover from network drops?

### Takeaway
Sending NV12 (3.1 MB per frame, about 93 MB/s) instead of BGRA (8.3 MB per frame, about 249 MB/s) is the certain bandwidth win, for every encoder except NVENC, which takes BGRA itself. Do the conversion in Rust, reusing `p2c_shm::convert::fill_nv12`. The ffmpeg side gets cheaper (measured), but the conversion moves into Page2Cam, so the **net CPU change is unmeasured**. Keep stdin pipes; shared memory is not worth it at this data rate.

Two risks in the input path:
- Raw 8 MB frames queue up inside ffmpeg while the network stalls. In a local test with output blocked, ffmpeg's RSS reached about 690 MB after 20 s.
- The video writer's "restart the clock" path loses frames that audio does not lose, so A/V drifts after each stall.

For drops, RTMP has no `-reconnect` option. Use the `fifo` muxer with `attempt_recovery`, or (better for this app) restart ffmpeg from the controller.

### Cited Findings
- **rawvideo demuxer options:** `framerate` (default 25), `pixel_format`, `video_size` (required), `stride` — [FFmpeg Formats doc](https://ffmpeg.org/ffmpeg-formats.html)
- **Windows pipe history:** a 2012 thread reports high per-write overhead on Windows pipes and slow raw-frame piping at 1280x544 and up. Reading in 64 KB chunks fixed a reader-side slowdown. This material is old — [ffmpeg-user 2012](https://ffmpeg.org/pipermail/ffmpeg-user/2012-August/008904.html)
- **Windows binary mode:** a broken-pipe error was fixed by setting stdin to binary mode. This applies to C programs; Rust's `ChildStdin` is binary already — [SO archive](https://kiwix.lema.org/stackoverflow.com_en_all_2023-11/questions/36420233/ffmpeg-av-interleaved-write-frame-broken-pipe-under-windows)
- **`-use_wallclock_as_timestamps` reports** (search snippets; they concern RTSP, HLS, piped raw H.264 and live inputs, not rawvideo pipes):
  - One user said it fixed desync but made video stutter — [ffmpeg-user 2022-07](https://ffmpeg.org/pipermail/ffmpeg-user/2022-July/055207.html)
  - A Trac ticket says wallclock is not applied to piped raw H.264 — [trac #11268](https://trac.ffmpeg.org/ticket/11268)
  - Users still saw drift over a day — [ffmpeg-user 2025-06](https://ffmpeg.org/pipermail/ffmpeg-user/2025-June/059638.html)
  - Adding it per input did not fix their problem — [ffmpeg-user 2025-01](https://ffmpeg.org/pipermail/ffmpeg-user/2025-January/059008.html)
- **fifo muxer options** (local `ffmpeg -h muxer=fifo`, 7.0.2; also present as strings in the bundled n8.1 exe): `attempt_recovery`, `max_recovery_attempts`, `recovery_wait_time` (default 5 s), `recover_any_error`, `drop_pkts_on_overflow`, `queue_size` (default 60 packets), `restart_with_keyframe`, `fifo_format`, `format_opts`.
- **RTMP protocol options** (local `-h protocol=rtmp`): `rtmp_buffer` (default 3000 ms), `tcp_nodelay`, `rtmp_live`, `rtmp_enhanced_codecs`. There is no reconnect option. The `reconnect*` strings in the ffmpeg binary belong to the HTTP protocol (local tool output).
- **Measured, ffmpeg side only:** NV12 input cut the total x264 CPU time from 8.1 s to 4.8 s per 10 s of 1080p30, and NVENC's from 2.4 s to 1.2 s (local benchmark above). This excludes the cost of producing NV12 in the app. It also includes less file-read work, because the NV12 file is smaller.
- **Measured, input buffering:** raw BGRA 1080p30 was piped into ffmpeg 7.0.2 (`-thread_queue_size 512`, libx264 ultrafast, FLV output to a named pipe nobody read). After 20 s, the receiving ffmpeg had an RSS of about 690 MB and the upstream writer had blocked. It is not known whether this keeps growing or caps (local test, one run).

### Inferences
- **NV12 input (high payoff, low effort).**
  - **Change:** convert in the stream writer thread (or the ticker) and pass `-pix_fmt nv12`.
  - **Savings:** BGRA uses 249 MB/s of pipe bandwidth (CLAUDE.md quotes about 250 MB/s); NV12 needs 93 MB/s. It also removes about 0.25 core of swscale work from ffmpeg (measured).
  - **Cost:** Page2Cam's own conversion. A scalar Rust BGRA to NV12 at 1080p might take several ms per frame on one core (an estimate). Check `fill_nv12` with a benchmark, and consider `yuvutils-rs` or SIMD if it is slow.
  - **NVENC exception:** keep BGRA, because the GPU converts it for free.
  - **Color:** use BT.709 limited range for HD and tag it (`-colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv`). YouTube asks for Rec. 709. `fill_nv12` must use the same matrix; check it. The camera DLL may use BT.601.
- **Lower `-thread_queue_size` for video to about 8–16 (unverified benefit).** In theory 512 queued 8 MB packets is about 4 GB. Measured, ffmpeg 7.0.2 held about 690 MB after 20 s of blocked output. ffmpeg 7+ reworked its threading, so how much of that the option controls is unverified. A small queue makes backpressure reach the writer quickly, so the app can detect "encoder or network can't keep up" (write blocked for more than one frame period) and show it on the admin page.
- **Pipes are fine.** Rust's `std::process` pipes are binary. 93–250 MB/s through one big `write_all` per frame is feasible on modern Windows; the 2012 overhead reports were about many small writes. Named pipes or shared memory add complexity without clear gain. Keep the audio on TCP: it avoids a second stdin and works on both OSes.
- **Timestamps:** do not use `-use_wallclock_as_timestamps`. The reports above show stutter and unreliability, and it timestamps when ffmpeg *reads* data, which includes pipe and queue jitter. Keep count-based timestamps (frame n has PTS n/fps; sample k has PTS k/48000) and make both writers follow **one shared clock** (see Q3).
- **Network drops, option A (in ffmpeg):** wrap the output in fifo:
  `-f fifo -fifo_format flv -attempt_recovery 1 -recover_any_error 1 -max_recovery_attempts 0 -recovery_wait_time 2 -drop_pkts_on_overflow 1 -restart_with_keyframe 1 -queue_size 300 rtmp://...`
  - ffmpeg keeps reading inputs and encoding while it reconnects, so the input pipes never block. YouTube sees a gap and a new FLV session.
  - Caveat: per the RTMP spec and common experience, YouTube may treat the reconnection as the same stream when it happens within the stream's reconnect window. That is not verified this session.
- **Network drops, option B (app-level, more transparent):**
  - When `watch_stderr` sees ffmpeg exit, or "frame=" stops advancing for more than 5 s while state is Live, restart ffmpeg with exponential backoff (1, 2, 4… up to 30 s). Show "reconnecting (attempt n)" on the admin page.
  - This reuses the existing controller. Each restart costs about 1–2 s of stream and resets timestamps, which is cleanly in sync.
  - Recommendation: B first, because it is simpler to reason about and already fits the state machine. Then optionally A.
- **Detecting dropped frames and lag:** parse `fps=`, `speed=`, `drop=` and `dup=` from the `-stats_period 1` lines the app already gets.
  - `speed` below about 0.98x for several seconds means the encoder or network can't keep up. Suggest a hardware encoder, lower resolution or lower bitrate in the UI.
  - Also time `write_all` per frame in the writer.
  - `-progress pipe:2` gives key=value output that is easier to parse than `\r` stats lines.

### Gaps
- I found no current benchmark of 1080p raw-frame throughput through Windows anonymous pipes.
- I did not verify YouTube's reconnect-window behavior with ffmpeg's fifo recovery.
- `fill_nv12` speed and its color matrix were not measured or checked, so the net CPU effect of NV12 is unknown.
- I did not test whether `-thread_queue_size` bounds ffmpeg's memory on 8.1 during a stall, nor whether the ~690 MB plateaus.

## Q3: How should A/V sync work in this design?

### Takeaway
ffmpeg timestamps each raw input by counting: rawvideo frames at `-framerate`, f32le samples at 48 kHz. Sync therefore holds only if the app writes exactly fps frames and 48000 samples per wall-clock second over the life of the stream. Two current behaviors break this:
- **Video stall reset:** after a stall the video writer resets its clock (it loses frames) while the audio writer bursts to catch up (it keeps samples). After a stall of S > 1 s, content captured at wall time T gets video PTS ≈ T−S but audio PTS ≈ T. Viewers then hear **audio S late relative to the picture**, permanently and cumulatively over stalls. This is derived from `controller.rs`; it was not observed.
- **No clock-drift correction:** mic and page audio rings are drained on the system clock. A device clock that runs fast or slow by tens of ppm causes periodic drops or skip-aheads (clicks) or silence gaps instead of smooth correction.

Fix both with one master clock and a small adaptive-resampling controller per source, as OBS's async-audio filter does.

### Cited Findings
- **Why drift happens:** each audio device has its own hardware clock. At the same nominal 48 kHz they differ by ppm because of tolerances and temperature. Unsynced clocks, not nominal rate mismatches, cause buffers to overflow (drops, pops) or underflow (gaps) over minutes. The fix is asynchronous sample-rate conversion that estimates the rate difference and adjusts slowly over minutes so it is inaudible (libsamplerate or libswresample) — [norihiro/obs-async-audio-filter](https://github.com/norihiro/obs-async-audio-filter)
- **OBS prior art (search snippets of libobs commit messages and issue threads, not the code itself):**
  - Each source keeps a "last" and a "next expected" audio timestamp. Timestamps that drift past `TS_SMOOTHING_THRESHOLD` cause resyncs and glitches — [OBS commit 7b8793f](https://github.com/obsproject/obs-studio/commit/7b8793f6c3dec005cd537454f8521ec3ce3d1d60)
  - When a source does not deliver audio at a steady pace, buffering grows until it reaches its maximum — [OBS commit da87f08](https://github.com/obsproject/obs-studio/commit/da87f08da45e3ceb2b386bc01ead51d96b2772d7)
  - OBS syncs on relative timestamps, rebased to the first frame — [OBS discussion #11108](https://github.com/obsproject/obs-studio/discussions/11108)
  - Users still report video slowly falling behind audio in OBS with some USB/PipeWire sources, so even mature software struggles here — [OBS forum](https://obsproject.com/forum/threads/video-gradually-falling-behind-audio-where-can-i-start-investigating.187021/post-683699)
- **Wallclock timestamps** are a known, unreliable workaround (Q2 citations).

### Inferences
- **One master clock, the audio sample count.**
  1. **Audio writer:** keep it as it is, writing 480 frames per 10 ms with no reset, so the audio timeline stays locked to wall time. Silence fill makes it always produce data.
  2. **Video writer:** derive frames-owed from the same start instant and the same elapsed time: `due = floor(elapsed * fps)`.
     - **Behind:** if it is behind by k frames, write k duplicates of the latest frame as fast as the pipe accepts, bounded by about 2 s. Do not reset `n`.
     - **Far behind:** if it is more than about 2 s behind, also tell the audio writer to drop the same amount, so both timelines skip together. Alternatively, restart ffmpeg (Q2 option B).
     - **Where the drift comes from today:** the current `start = now; n = 0` path is the only place video drops time relative to audio.
  3. **Shared start time:** start both writers from the same `Instant`, taken after ffmpeg has accepted the audio connection. Until then, hold the video writer: at present it starts before the audio socket is accepted, so its first frames' content is older than audio PTS 0. This is a small, probably sub-second start offset that is fixed for the whole stream; it is unmeasured.
- **Adaptive resampling for drift (medium effort, removes clicks and gaps on long streams).**
  - **Controller:** for each source ring (mic, and page if CEF's audio is not on the system clock), keep the ring fill near a target, e.g. the mic delay plus 20 ms, or 40 ms for the page. Feed the error into a slow PI controller that nudges the resampler ratio by at most ±0.1% (1000 ppm).
  - **Resampler:** the existing linear `Resampler` already takes an arbitrary `step`. Make `step` adjustable. This is a small change.
  - **Fallback:** keep today's "skip ahead / re-prime" logic for large errors (device restart).
  - **Mic delay:** today's skip-ahead logic is then reached only on faults, not every few minutes. At 100 ppm the backlog changes by 4.8 samples/s, so the 960-frame slack is used up in about 200 s, then a skip occurs. The linear resampler's quality is acceptable for speech; for music on the page, a windowed-sinc resampler (`rubato` crate) is better.
- **Measuring sync (low effort, essential).**
  - **Test page:** flashes a white box and plays a 1 kHz beep together every 2 s (Web Audio scheduled against `requestAnimationFrame`).
  - **Measure:** stream to a local RTMP listener (the existing `rtmp_local` test), or record with `-f tee` to a file. Use ffmpeg filters to find the timestamps of both events: `blackframe`/`signalstats` for luma and `silencedetect` or `astats` for the beep. Report the offset over 30+ minutes.
  - **Mic path:** check the same way with a webcam pointed at a clap or slate.
  - The test can become an `--ignored` integration test that fails if |offset| exceeds 45 ms. ITU-R BT.1359's detectability thresholds are commonly cited as about +45 ms audio-leading and -125 ms audio-lagging; I did not fetch that source this session.
- **Mic delay vs webcam latency:** the current approach (delay the mic by the webcam cut-out latency) is right. Make the delay measured rather than configured: time a frame from capture timestamp to composite and feed a running median into `set_mic_delay_ms`. Changing it should glide through the resampler, not jump.
- **Page audio and video come from the same CEF process.** CEF's OSR audio callback and `on_paint` both run on Chromium's clock, which is the system clock, so the page should need little drift correction. That is an inference I did not verify. Its fixed latency differs, though: audio buffering in CEF versus compositor frame latency. Measure it with the flash and beep test and expose a "page audio offset" setting.

### Gaps
- I did not read OBS's `audio-io.c` or `obs-source.c` source directly; the OBS behavior above comes from commit messages, plugin docs and issues.
- I found no source on CEF's audio clock in OSR mode or on its audio callback latency.
- I did not fetch the ITU BT.1359 thresholds; the figures above are from memory.

## Q4: What are the alternatives to an ffmpeg child process in Rust?

### Takeaway
Keep the ffmpeg child process. It already ships as a single static exe with every hardware encoder, it isolates crashes and GPL code from the app, and its failures are already surfaced through stderr. The in-process options (ffmpeg-next/rsmpeg, gstreamer-rs, raw Media Foundation) would save the pipe copy (about 250 MB/s, or less after NV12) but cost significant build, licensing and distribution work on Windows.

### Cited Findings
- **ffmpeg-next 9.0.0** is in "maintenance mode" and supports FFmpeg 3.4 through 8.0. The maintainer may need contributor PRs for new APIs and advises pinning the version — [docs.rs ffmpeg-next 9.0.0](https://docs.rs/crate/ffmpeg-next/9.0.0)
- More current forks exist (ffmpeg-the-third, rffmpeg), per an opinion blog from 2025-10 — [jdiaz97 blog](https://scour.ing/p/https://jdiaz97.github.io/blog/what-julia-has-that-rust-needs)
- **GStreamer on Windows:**
  - It ships separate runtime and development packages, and the app should redistribute the runtime. Shipping the full runtime installer inflates size; pick libraries by hand to trim it — [GStreamer Windows deployment](https://gstreamer.freedesktop.org/documentation/deploying/windows.html); [Installing on Windows](https://gstreamer.freedesktop.org/documentation/installing/on-windows.html)
  - Mixing C runtimes between the app and GStreamer causes problems — [Installing on Windows](https://gstreamer.freedesktop.org/documentation/installing/on-windows.html)
  - A forum report puts the macOS framework at about 1.89 GB and points to `gstreamer-full` for size — [GStreamer discourse](https://discourse.gstreamer.org/t/optimizing-gstreamer-framework-distribution-for-end-user-applications/3223)

### Inferences
- **libav in-process (ffmpeg-next, rsmpeg, ffmpeg-sys):**
  - **Pros:** no pipe copy; exact control of PTS (pass real timestamps instead of relying on counted frames); direct hardware frame upload (D3D11/CUDA) is possible.
  - **Cons:**
    - Linking FFmpeg for Windows from this Linux cross-build box with cargo-xwin is the hardest part.
    - A GPL static FFmpeg (needed for libx264) makes the whole binary GPL. An LGPL dynamic build drops libx264, so it needs hardware encoders or openh264 for software fallback.
    - Crashes in drivers or encoders take the app down.
    - The API churns across FFmpeg major versions.
  - Verdict: not worth it now.
- **gstreamer-rs:** good pipelines (`appsrc ! videoconvert ! nvh264enc/qsvh264enc/mfh264enc/x264enc ! flvmux ! rtmp2sink`) and robust timestamping with the pipeline clock. But Windows distribution means bundling a trimmed runtime of tens to hundreds of MB, with plugin discovery, MSVC CRT matching and x264 licensing to handle. That is too heavy next to a single ffmpeg.exe.
- **Media Foundation directly** (Sink Writer or IMFTransform H.264 encoder through the `windows` crate):
  - **Pros:** zero extra binaries; uses whatever the GPU vendor's MFT provides.
  - **Cons:** Windows only, so Linux still needs ffmpeg. It has no RTMP/FLV muxer, so you would write FLV and RTMP yourself or still pipe H.264 into ffmpeg `-c copy`. Its quality and rate-control knobs are weaker (h264_mf has no profile control, see Q1).
  - Verdict: only as a curiosity.
- **Middle ground, if pipe bandwidth ever matters:** keep ffmpeg as the muxer and RTMP client, but feed it already-encoded H.264 from an in-process encoder (`-f h264 -i pipe:0 -c:v copy`). This keeps ffmpeg's RTMP, AAC and reconnection code. It is not needed at current scale.

### Ranked recommendations (effort vs payoff, across all questions)

Each recommendation is grouped into one of the four questions above.

| # | Change | Effort | Payoff | Section |
|---|---|---|---|---|
| 1 | Fix the video writer's clock reset (duplicate frames to catch up; never let video lose time that audio keeps). Start both writers from one instant, after the audio socket is accepted. | Low | Removes permanent audio-late offset after stalls (derived from code) | Q3 |
| 2 | Bitrate defaults: 720p30 4–6 Mbps or 1080p30 ≥6 Mbps (YouTube's H.264 1080p30 minimum is 5 Mbps); RTMPS URL; drop `-tune zerolatency` for x264 (gets B-frames as YouTube recommends) | Trivial | Fewer YouTube warnings; better quality per bit | Q1 |
| 3 | Hardware encoder probe and fallback chain with per-encoder CBR, 2 s GOP settings; show the chosen encoder in admin | Medium | About 0.65 core down to about 0.07 core CPU, net of file read (NVENC, measured locally); frees the CPU for CEF and webcam cut-out | Q1 |
| 4 | Send NV12 instead of BGRA (except to NVENC) | Low–medium | Pipe bandwidth −63% (249 to 93 MB/s, certain); ffmpeg-side CPU −40% with x264 (measured); net CPU unmeasured, since the conversion moves into Rust | Q2 |
| 5 | Auto-reconnect: restart ffmpeg with backoff on exit or stall (optionally fifo muxer `attempt_recovery`) | Low–medium | Streams survive Wi-Fi blips | Q2 |
| 6 | Parse `speed=`, `drop=`, `dup=` and time writer blocking; warn in admin | Low | Self-explaining failures (user requirement) | Q2 |
| 7 | Lower video `-thread_queue_size` to about 8–16 | Trivial | Possibly bounds RAM and latency during stalls (~690 MB seen after 20 s blocked); unverified on 8.1 | Q2 |
| 8 | Adaptive drift-correcting resampling on mic (and page if needed) | Medium | Removes periodic clicks and gaps on long streams | Q3 |
| 9 | Automated flash and beep sync measurement test | Medium | Turns sync from guesswork into a number | Q3 |
| 10 | HEVC/AV1 via Enhanced RTMP with hardware encoders | Medium | About 30% bitrate saving; YouTube status uncertain | Q1 |
| 11 | In-process libav or GStreamer | High | Marginal for this app | Q4 |

### Gaps
- I found no published size for a trimmed Windows GStreamer bundle that includes `rtmp2sink` and the hardware encoders.
- I did not look up rsmpeg's current FFmpeg 8 support status.
