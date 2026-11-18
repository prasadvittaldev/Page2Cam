# Fix the glue before buying GPUs

Page2Cam's biggest wins are not new models or a GPU rewrite. They are three code defects, confirmed in the source but not yet observed at runtime, plus a set of low-effort integration fixes. **First, the DirectML session in `crates/p2c-segment/src/runtime.rs` does not set the two options DirectML documents as required** (memory pattern off, sequential execution), so Windows users may silently get the CPU while the log says "DirectML unavailable". **Second, the stream's video writer in `crates/p2c-stream/src/controller.rs` resets its clock after any stall longer than 1 s and the audio writer does not.** That should leave audio permanently late by each stall's length. **Third, the default 4500 kbps at 1080p30 is below YouTube's 5 Mbps H.264 minimum.** After those, the order is: make the cut-out look right by compositing RVM's own foreground and alpha at native aspect; cut RVM's cost by running it at half rate on its own thread; switch RTMP encoding to a probed hardware encoder, which measured at about 0.65 of a CPU core for libx264 versus about 0.07 for NVENC on the dev box; and stop copying roughly 100 MB of frame data per tick. A full GPU compositor pays off only when frames stay on the GPU end to end (CEF shared textures → composite → hardware encoder). Treat it as a later, Windows-first project behind a `Compositor` trait, with today's CPU path kept as the fallback. For distribution, the HKLM-registered camera DLL effectively rules out MSIX. Ship a per-machine Inno Setup (or Linux-buildable NSIS) installer with a small signed-download update checker now, and move to Velopack per-user delta updates only if 300 MB full-installer updates become a problem. Code signing comes first, because Smart App Control blocks unsigned executables. Speed figures are estimates unless the text says "measured" (a local benchmark) or "in the code" (read from the source during this research). None of the Windows paths has run on real hardware.

## Three code defects to fix before any optimization

All three were confirmed by reading the current source. Their runtime effects follow from the code and the documentation, but nobody has observed them.

**DirectML session options are missing.** `runtime.rs::session()` builds the DirectML session as `Session::builder(env)?.with_execution_providers([DirectML…error_on_failure()]).commit_from_file(model)` and sets nothing else. ONNX Runtime's documentation says the DML EP "does not support memory pattern optimizations or parallel execution". Both "must be disabled or an error is returned", and memory pattern is on by default ([ORT DirectML EP](https://onnxruntime.ai/docs/execution-providers/DirectML-ExecutionProvider.html)). Whether ORT 1.24.4 really errors, or just warns and turns the option off, was not verified. If it errors, the `match` arm sends Windows users to the CPU with a misleading warning. The fix is two builder calls that exist in the pinned `ort` 2.0.0-rc.15 source: `.with_memory_pattern(false)?` and `.with_parallel_execution(false)?`. The same function should also log the adapter and EP actually in use, and expose `with_device_id` and `with_performance_preference(HighPerformance)` in config. ORT warns that "the primary display (GPU 0) is often not the most performant one", which matters on laptops with both an iGPU and a dGPU ([ORT DirectML EP](https://onnxruntime.ai/docs/execution-providers/DirectML-ExecutionProvider.html)). Logging placement matters for one more reason. With `api-22` or later enabled, `ort` rc.15's builder silently sets an `AutoDevicePolicy::MaxEfficiency` EP-selection policy (crate source, [pykeio/ort](https://github.com/pykeio/ort)). That is probably harmless when no NPU EP is registered.

**The video-clock reset desynchronizes audio after a stall.** `write_video` paces against `start + period * n`, and when it falls more than 1 s behind it runs `start = now; n = 0` (controller.rs, lines 217–220). `write_audio` keeps its own `start`, never resets it, and bursts to catch up. ffmpeg timestamps raw inputs by counting: frame n gets n/fps and sample k gets k/48000. A stall of S seconds therefore removes S seconds from the video timeline but none from the audio timeline, so viewers should hear **audio S seconds late, permanently, with the offsets adding up across stalls**. There is a second, smaller offset. The video writer starts before ffmpeg has connected to the audio socket, so the first video frames predate audio PTS 0 by a fixed but unmeasured amount.

The fix is a single master clock. Keep the audio writer as it is. Make the video writer compute `due = floor(elapsed * fps)` from a shared `Instant` taken after the audio accept, and catch up by writing duplicates of the latest frame instead of resetting `n`. Beyond about 2 s behind, either have audio drop the same amount or restart ffmpeg. Don't reach for `-use_wallclock_as_timestamps`: users report it introduces stutter and still drifts ([ffmpeg-user 2022-07](https://ffmpeg.org/pipermail/ffmpeg-user/2022-July/055207.html); [ffmpeg-user 2025-06](https://ffmpeg.org/pipermail/ffmpeg-user/2025-June/059638.html)).

**The default bitrate is below YouTube's floor.** `video_kbps: 4500` appears in `crates/page2cam/src/config.rs`, `crates/p2c-stream/src/args.rs` and `packaging/windows/page2cam.toml`. YouTube's live encoder page lists these H.264 bitrates:

| Resolution | Minimum | Recommended |
|---|---|---|
| 1080p30 | 5 Mbps | 14 Mbps |
| 720p30 | 3 Mbps | 8 Mbps |

The same page asks for a 2 s keyframe interval, 2 B-frames, CBR and CABAC, and recommends RTMPS ([YouTube Help](https://support.google.com/youtube/answer/2853702?hl=en)). Mostly static web pages compress well, so a sensible default is 1080p30 at 6–9 Mbps, plus a 720p30 preset at 4–6 Mbps for weak uplinks. In the same change, drop `-tune zerolatency` from libx264. It disables the B-frames and look-ahead that YouTube recommends, and YouTube's multi-second ingest latency absorbs x264's look-ahead delay. A 10–20% gain in quality per bit is plausible but unmeasured. The encoder delay is constant, so it cannot cause desync. A minor point to note: YouTube's page lists 44.1 kHz for stereo audio, while Page2Cam sends 48 kHz stereo. That is unlikely to matter but worth knowing.

The research also turned up smaller defects:

- **Camera colour:** the Windows camera's `fill_nv12` (`p2c-shm`) takes chroma from a single pixel and uses nearest-neighbour scaling. Its colour matrix has not been checked against the BT.709 that YouTube expects.
- **Popups:** `<select>` popups (`PET_POPUP`) are dropped, a correctness gap for interactive pages.

## The cut-out halo comes from integration, not the model

Every shipping background-removal system follows the same recipe:

1. A tiny segmentation network running at about 256×144 to 512×288.
2. **Edge-aware upsampling guided by the full-resolution camera frame.** Google Meet uses a joint bilateral filter ([Google Research 2020](https://research.google/blog/background-features-in-google-meet-powered-by-web-ml/)). Teams uses a deep guided filter that "uses the high-resolution input as a guide" ([Microsoft Research](https://www.microsoft.com/en-us/research/?p=1172105)). RVM's own Deep Guided Filter (DGF) does the same ([RVM paper](https://arxiv.org/abs/2108.11515)).
3. Temporal stabilization.

The code departs from this recipe in four places:

- `engine.rs` squashes every webcam frame, 4:3 included, to 640×360 with `downsample_ratio` 0.4.
- It reads `pha` and discards `fgr`.
- `tensor.rs::write_alpha` then bilinearly upscales that mask onto the **raw** webcam pixels.
- `compose.rs` blends with straight alpha and scales the PIP with `use_alpha(false)`.

The halo comes from that last pair of steps. At an edge pixel the camera saw I = αF + (1−α)B, so `I·α + page·(1−α)` puts the room colour B back in. RVM already predicts the clean foreground F. Its foreground error is about half MODNet's (VM-D646 FG MSE 2.93 vs 6.31), and the authors' reference composite is `fgr * pha + bgr * (1 - pha)` ([RVM paper](https://arxiv.org/abs/2108.11515); [RVM README](https://github.com/PeterL1n/RobustVideoMatting)). **Writing `fgr` into BGR and `pha` into A is the single cheapest quality fix**, because the network already computes `fgr`. If `fgr` looks softer than the camera inside the body, use it only in the edge band (0 < α < ~0.95) and keep camera pixels elsewhere. That hybrid is untested.

The second fix is premultiplied resampling. Bilinear scaling of straight-alpha pixels bleeds the colour of transparent pixels into the edge, which produces the classic dark or coloured fringe. Premultiply once when the PIP is rescaled, scale, then blend `out = Fp + page·(1−α)`. That is also one multiply per channel cheaper than today's blend. As optional polish, add Meet-style light wrap ([Google Research 2020](https://research.google/blog/background-features-in-google-meet-powered-by-web-ml/)): mix a little blurred page colour (k ≈ 0.1–0.3) into the edges, computed over the PIP region only. Don't EMA-smooth RVM's alpha heavily. It is trained with a temporal-coherence loss and stabilizes within about 15 frames, so extra smoothing only adds ghost trails ([RVM paper](https://arxiv.org/abs/2108.11515)).

Fixing the aspect ratio carries a trade-off the research notes disagree on. RVM is fully convolutional and takes its resolution from `downsample_ratio`. Its documentation says to pick a ratio that puts the downsampled frame between 256 and 512 px ([RVM inference docs](https://github.com/PeterL1n/RobustVideoMatting/blob/master/documentation/inference.md)). Feeding 1280×720 at ratio 0.25 keeps the encoder at about 320×180, close to today's 256×144. DGF then delivers alpha and foreground at full webcam resolution, guided by the full-resolution frame, and the bilinear `write_alpha` step goes away. But DGF runs at the input resolution, and 720p is four times the refiner pixels of 640×360. That makes full-resolution DGF a likely CPU cost increase, at odds with the speed goals in the next section. The resolution is to fix the aspect everywhere but choose the refiner size per tier, after a benchmark:

| Tier | Input | Ratio | Encoder size |
|---|---|---|---|
| CPU, 16:9 camera | about 640×360 | 0.4–0.5 | ~256–320 px wide |
| CPU, 4:3 camera | 640×480 | 0.5 | 320×240 |
| GPU | native 720p | 0.25 | 320×180 |

On the GPU tier the extra refiner work is cheap. Also confirm that the shipped `rvm_mobilenetv3_fp32.onnx` has dynamic spatial axes.

The MediaPipe "fast" path needs a full post-processing chain, because it is a segmenter, not a matter. Slack found that compositing MediaPipe's confidence map at half opacity made people look "ghostly", and fixed it with threshold mapping and bilateral filtering ([Slack Engineering](https://slack.engineering/building-background-effects-for-clips/)). The GPL-3.0 OBS background-removal plugin can be ported legally, because the licences match. Its chain is ([obs-backgroundremoval source](https://raw.githubusercontent.com/royshil/obs-backgroundremoval/main/src/background-filter.cpp)):

1. Skip inference when PSNR against the last processed frame exceeds 35 dB.
2. Blend over time with factor 0.85.
3. Keep only contours larger than 5% of the image.
4. Blur, then re-binarize.
5. Offer erode and feather controls.

For Page2Cam, the equivalent chain is:

1. Upsample with a fast guided filter that uses webcam luma as the guide. Its cost is O(N) regardless of radius, and subsampling gives more than a 10× speed-up at s=4 ([He & Sun](https://arxiv.org/pdf/1505.00996)).
2. Apply an EMA that resets per pixel on large changes.
3. Remap with `smoothstep(0.35, 0.65)`.
4. Remove small blobs.
5. Erode 1–2 px before feathering.

Feed MediaPipe's landscape model a letterboxed 16:9 frame, or use the 256×256 general model for 4:3 cameras. The landscape model is "designed for cases where the input always has that shape" ([MediaPipe guide](https://ai.google.dev/mediapipe/solutions/vision/image_segmenter)).

No newer model justifies a switch. **No permissively licensed, laptop-real-time model from 2023 to 2026 clearly beats RVM-MobileNetV3**, whose MAD of 6.08 against MODNet's 9.41 on VideoMatte240K still sets the bar ([RVM paper](https://arxiv.org/abs/2108.11515)). MatAnyone 2 carries the non-commercial NTU S-Lab licence ([arXiv 2512.11782](https://arxiv.org/abs/2512.11782v1)). BiRefNet runs at about 17 fps at 1024² on an RTX 4090 ([Replicate](https://replicate.com/sprited/birefnet/readme)), orders of magnitude too slow for a laptop.

The sensible tier set:

- **Fast:** MediaPipe plus guided upsampling.
- **Quality:** RVM-MobileNetV3 with `fgr`, at the CPU refiner size above.
- **Max (optional):** full-resolution DGF or RVM-ResNet50 on a discrete GPU via DirectML.

## Halving RVM's cost comes from scheduling, not session flags

The research notes report about 31 ms per frame for RVM fp32 at 640×360 on the Ryzen 5 8600G CPU and about 1 ms for MediaPipe landscape. The notes don't give the method, so treat these as indicative. ORT already turns on all graph optimizations by default ([ORT graph optimizations](https://onnxruntime.ai/docs/performance/model-optimizations/graph-optimizations.html)), so no session flag will give a large CPU speed-up.

The biggest cheap lever is temporal decimation. **Run RVM every second frame (15 Hz) on its own thread, publishing to a latest-only `FrameSlot`**, as the rest of the architecture already does. The compositor then never waits and always uses the newest mask. That should roughly halve average CPU (estimate). Two caveats apply:

- **Edge lag on fast motion.** Dilating the reused mask slightly hides it.
- **The recurrent state.** Whether RVM's state behaves well at a 15 Hz cadence is unverified; check it visually.

The notes disagree on skipping static frames. It is a big win for talking-head streams, but with RVM a skipped frame must not advance the recurrent state. One note judges the skip safe only for the stateless MediaPipe path. Ship it there first, and add it to RVM only with a forced refresh every N frames, after a visual check.

Thread tuning comes next, and it is about contention more than speed. ORT defaults to one intra-op thread per physical core with spinning on, and spinning "consumes more CPU cycles, resources, and power" ([ORT threading](https://onnxruntime.ai/docs/performance/tune-performance/threading.html)). On a 6-core part that also runs CEF, x264 and MJPEG decode, try `with_intra_threads(3–4)` with `with_intra_op_spinning(false)`, and measure in the full app rather than in isolation. The OBS plugin reports that "a 2-thread setting works best" ([obs-backgroundremoval](https://github.com/royshil/obs-backgroundremoval)). Two cleanups are free: reuse the 2.7 MB `src` buffer, and feed the r1–r4 output `Value`s straight back in instead of calling `to_vec()` and `clone()`. They are worth well under 1 ms on CPU, because the recurrent states total about 1 MB (estimate).

On DirectML the priorities change. RVM's authors note that the naive loop moves the recurrent state between CPU and GPU every frame when it "could have just stayed on the GPU" ([RVM inference docs](https://github.com/PeterL1n/RobustVideoMatting/blob/master/documentation/inference.md)). `ort` rc.15 exposes the full IoBinding surface (`create_binding`, `bind_output_to_device`, `run_binding`, `AllocationDevice::DIRECTML`). The plan:

1. **Keep state on the GPU.** Ping-pong r1–r4 in DML memory, and bind only `pha` (and `fgr`) to CPU memory. That removes eight transfers per frame, and the GPU syncs that come with them.
2. **Use the official fp16 model.** Switch to `rvm_mobilenetv3_fp16.onnx` ([RVM README](https://github.com/PeterL1n/RobustVideoMatting)), or convert with `keep_io_types=True` so the Rust I/O stays f32 ([ORT float16](https://onnxruntime.ai/docs/performance/model-optimizations/float16.html)).
3. **Re-export with fixed shapes.** Use a fixed resolution and a constant ratio, because DML performs best when "tensor shapes are known at the time a session is created" ([ORT DirectML EP](https://onnxruntime.ai/docs/execution-providers/DirectML-ExecutionProvider.html)).

All of this compiles, but none of it has been checked at runtime. No public benchmark of RVM on DML was found.

The remaining options rank lower:

- **fp16 on CPU:** doesn't help, because ORT's CPU EP lacks most fp16 kernels ([ORT float16](https://onnxruntime.ai/docs/performance/model-optimizations/float16.html)).
- **Static int8 QDQ on the encoder only:** a fair experiment on VNNI CPUs. ORT warns that worse performance on older devices "is not rare" ([ORT quantization](https://onnxruntime.ai/docs/performance/model-optimizations/quantization.html)). NVIDIA's FAQ says ORT-DML does not support 8-bit ([NVIDIA Model Optimizer FAQ](https://nvidia.github.io/Model-Optimizer/_sources/support/2_faqs.rst.txt)), a third-party claim that ORT's own docs neither confirm nor deny.
- **Windows ML:** DirectML is in "sustained engineering" and is labelled "legacy" in Windows ML. Windows ML downloads vendor EPs (QNN, OpenVINO, VitisAI, AMD GPU, TensorRT-RTX), but NPU and vendor EPs need Windows 11 24H2 or later ([Windows ML EPs](https://learn.microsoft.com/en-us/windows/ai/new-windows-ml/supported-execution-providers)). Using it from Rust means bootstrapping the Windows App SDK and confirming that its bundled ORT exposes `api-24`.
- **Windows Studio Effects background mask:** the most interesting zero-inference path is its documented mask mode, with a per-frame `MF_CAPTURE_METADATA_FRAME_BACKGROUND_MASK` ([KS property](https://learn.microsoft.com/en-us/windows-hardware/drivers/stream/ksproperty-cameracontrol-extended-backgroundsegmentation); [MF attribute](https://learn.microsoft.com/en-us/windows/win32/medfound/mf-capture-metadata-frame-background-mask)). It works only on Copilot+ PCs whose camera OEM opted in, needs a Media Foundation capture path instead of nokhwa, and nobody has confirmed that shipping drivers implement it.

## The pipeline moves ~100 MB per tick, and the encoder dominates

### Copies and encoders are the cheap wins

The code does avoidable work on every 33 ms tick:

- `p2c-browser/src/handlers.rs::on_paint` allocates and copies a fresh 8.3 MB buffer with `.to_vec()` on every paint and ignores `_dirty_rects`.
- `compose()` copies the page with `copy_from_slice` even when its `seq` is unchanged, and builds a new `Resizer` for the cut-out styles on every call.
- `StreamSink` and `PreviewSink` each call `frame.clone()`.
- The preview thread polls every 80 ms, and `jpeg-encoder = "0.6"` is built without its `simd` feature.
- On the Windows camera path, the app copies BGRA into shared memory and the DLL copies it out again before a scalar conversion (per the notes' read of `stream.rs`).

That adds up to about 100 MB of memory traffic per frame, roughly 3 GB/s at 30 fps (arithmetic, not measured). It fits within a laptop's bandwidth, but it competes with CEF, the iGPU and ONNX. The fixes are low effort:

1. **Buffer pool:** hand sinks `Arc<Frame>` buffers from a pool of three instead of cloning.
2. **Reuse in `on_paint`:** keep one page buffer, ideally updating only the dirty rects.
3. **Skip unchanged work:** skip compose when the inputs and layout haven't changed.
4. **PIP cache:** cache the scaled, premultiplied PIP keyed on `(cam_seq, matte_seq, w, h, style)`.
5. **Preview:** turn on `jpeg-encoder/simd`, and have the preview thread wait on a notification instead of polling.

Each avoided 8 MB copy is worth roughly 0.3–1 ms of one core (estimate).

For the scalar colour converters, `yuvutils-rs` is pure Rust, so it cross-compiles under cargo-xwin, and it is BSD-3/Apache-2.0. **It converts RGBA→YUV420 in 781 µs with AVX2, versus 847 µs for libyuv, on a 2.66 MP image** ([yuvutils-rs](https://github.com/awxkee/yuvutils-rs)). That works out to about 0.3–0.6 ms per 1080p frame, and it fixes the single-pixel chroma. It provides `bgra_to_yuv_nv12`, but no direct BGRA→YUYV function was found, so the Linux packer needs hand-written SIMD.

**Measured on the dev box, libx264 is the heaviest stage the app controls.** The test encoded 10 s of 1080p30 `testsrc2` with ffmpeg 7.0.2 on the 8600G:

| Encoder and input | CPU use | Notes |
|---|---|---|
| libx264 veryfast, BGRA (current setup) | ~0.81 core gross, ~0.65 net of file reading | |
| libx264 veryfast, NV12 | about 40% less (4.39 + 0.45 s vs 7.15 + 0.99 s) | ffmpeg skips its swscale step, which alone costs ~0.25 core |
| h264_nvenc, BGRA | ~0.07 core net | NVENC accepts BGRA directly |

The test pattern is easier to encode than real pages, and no laptop was measured, so the libx264 share on a 4–6 core laptop will be larger (inference).

The plan that follows:

1. **Probe encoders.** At stream start, run a 3-frame `lavfi` test encode per candidate. On Windows, try NVENC, QSV, AMF, MF, then libx264. On Linux, try NVENC, VAAPI on each `renderD*` node, then libx264. On the dev box, VAAPI worked on renderD128 and failed on renderD129, so the device must be probed, not assumed. The bundled `ffmpeg.exe` (n8.1.3) contains the strings for all of these encoders, which suggests but does not prove they work.
2. **Send NV12.** Use NV12 for every encoder except NVENC: pipe bandwidth drops from 249 to 93 MB/s. The net CPU saving is unmeasured until the Rust-side conversion is benchmarked. Tag the output BT.709 limited range.
3. **Restart on failure.** Restart ffmpeg with backoff when it exits, or when `frame=` stops advancing for 5 s while Live. ffmpeg's RTMP has no reconnect option.
4. **Report health.** Parse `speed=` and `drop=`, and time `write_all`, so the admin page can explain a struggling uplink.
5. **Limit buffering.** A single local test with blocked output pushed ffmpeg's RSS to about 690 MB in 20 s. Lowering the video `-thread_queue_size` from 512 to 8–16 may bound that; this is unverified on ffmpeg 8.1.

For audio drift, give each source ring a slow PI controller that keeps the ring fill near a target by nudging the existing linear `Resampler`'s step by at most ±0.1%, as OBS's async resampling does ([obs-async-audio-filter](https://github.com/norihiro/obs-async-audio-filter)). Today, a 100 ppm clock mismatch on the mic uses up the 960-frame slack in about 200 s and then clicks (arithmetic). Also add a test: a flash-and-beep page, streamed and analysed with ffmpeg's `signalstats` and `silencedetect`, run as an ignored integration test, turns sync from guesswork into a number.

Keep the ffmpeg child process. ffmpeg-next is in "maintenance mode" ([docs.rs](https://docs.rs/crate/ffmpeg-next/9.0.0)), GStreamer's Windows runtime is heavy to redistribute ([GStreamer deployment](https://gstreamer.freedesktop.org/documentation/deploying/windows.html)), and a separate process isolates GPL code and driver crashes from the app.

On CEF, the current design pumps `do_message_loop_work` from the main thread. CEF's API documentation calls `CefDoMessageLoopWork` "not recommended for most users" (relayed through a search summary of [cef_app_win.h docs](https://cef-builds.spotifycdn.com/docs/121.3/cef__app__win_8h.html)). `cef_types.h` also describes `multi_threaded_message_loop` as supported on Windows and Linux and recommends it over an external pump ([cef_types.h](https://raw.githubusercontent.com/chromiumembedded/cef/master/include/internal/cef_types.h)). Switching to it removes the trade-off between pump latency and CPU. The costs are posting every host call to `TID_UI` and stricter shutdown ordering. `external_begin_frame_enabled`, with one `SendExternalBeginFrame` per ticker tick, would lock page frames to output frames. It was broken in Chromium 73+ and later fixed ([CEF PR #222](https://bitbucket.org/chromiumembedded/cef/pull-requests/222/fix-use_external_begin_frame-support-and/diff)), so verify it with the existing render test.

On the Windows camera, Microsoft's Frame Server rules are already met: `PINNAME_VIDEO_CAPTURE`, `MFGetSystemTime` timestamps, and allocator samples ([Frame Server Custom Media Source](https://learn.microsoft.com/en-us/windows-hardware/drivers/stream/frame-server-custom-media-source)). The cheap change is to offer more media types: 640×360, 640×480, 960×540 and YUY2 alongside NV12, with 1280×720 NV12 first in the list. Community reports have Teams negotiating YUY2 at about 360p–480p ([Microsoft Q&A](https://learn.microsoft.com/en-us/answers/questions/4444093/bad-quality-with-the-anker-c200-2k-camera?forum=msteams-all&page=2)). This might help with the unexplained case where Teams remote participants see nothing, which even VCamSample's author cannot explain ([VCamSample](https://github.com/smourier/VCamSample)), but that is speculation.

The producer also needs recovery. It should reopen the shared-memory mapping after a Frame Server restart and call `Start()` on the camera again after resume. VCamSample issue #4 showed that a DLL refcount bug crashes the shared Frame Server process ([VCamSample #4](https://github.com/smourier/VCamSample/issues/4)), so never call `Release()` by hand.

On Linux, keep YUYV through v4l2loopback with `exclusive_caps=1`. Write a frame immediately after `S_FMT`, and set `keep_format`, `sustain_framerate` and a `timeout` image ([v4l2loopback](https://github.com/umlaeute/v4l2loopback)). PipeWire cameras remain unreliable for browsers, because the portal filters software nodes ([Arch BBS](https://bbs.archlinux.org/viewtopic.php?id=314930)).

### The GPU pays only when frames never leave it

The page arrives through CPU `OnPaint`, and every sink (shared memory, v4l2loopback, the ffmpeg pipe, JPEG) consumes CPU memory. A GPU compositor today would therefore add an 8 MB upload and a 3–8 MB readback per frame. Bandwidth is not the problem; at 249 MB/s it is a small fraction of what unified memory provides. **Synchronization is.** wgpu's `map_async` resolves only after `poll()`, so Bevy and rerun accept "always a frame of latency" for readback ([Bevy gpu_readback](https://dev-docs.bevyengine.org/src/gpu_readback/gpu_readback.rs.html); [re_renderer](https://docs.rs/crate/re_renderer/0.32.1/source/src/texture_readback.rs)).

Each step below keeps the CPU path as its fallback:

1. Hardware encoding through ffmpeg flags. It needs no GPU API code.
2. Measure on target laptops.
3. Only if CPU use is still high, a wgpu compositor fed by CEF shared textures.
4. That compositor's NV12 output fed to a hardware encoder in-process.
5. Segmentation tensors shared on the GPU.
6. GPU virtual-camera samples, last.

The building blocks exist. The pinned `cef` 156.6.0 crate has an `accelerated_osr` feature with `osr_texture_import/{d3d11,dmabuf,vulkan}.rs` (checked in the local registry). It imports CEF's shared texture into a `wgpu::Texture`, and must be called "within the originating `on_accelerated_paint` callback", on the same physical GPU as CEF ([cef osr_texture_import](https://docs.rs/crate/cef/latest/source/src/osr_texture_import/mod.rs)).

**Start on Windows, and defer Linux until the CEF pin reaches M158.** CEF #4313, reported on CEF 156, describes `OnAcceleratedPaint` frames that "can be read before they're drawn on NVIDIA". The fix landed for M158 on 2026-11-10, and no backport to 156 is confirmed ([CEF #4313](https://github.com/chromiumembedded/cef/issues/4313)).

wgpu has no D3D11 backend ([wgpu changelog](https://github.com/SecondHalfGames/wgpu/blob/trunk/CHANGELOG.md)), so use `windows-rs` D3D11 only for the video processor and Media Foundation interop. Skip GPU samples for the virtual camera. VCamSample lists Chrome, Edge and Teams as "D3D-less" clients ([VCamSample](https://github.com/smourier/VCamSample)), and Frame Server runs the DLL in another process, which would require shared NT handles and fences.

The fallback mechanics:

1. **One interface:** `CpuCompositor` (today's code, optimized) and `GpuCompositor` sit behind a `Compositor` trait owned by the ticker.
2. **Startup check:** reject software adapters (WARP, llvmpipe), match CEF's adapter by LUID, and composite one known frame on both paths. Fall back to CPU if the GPU result is wrong or slower.
3. **Device loss:** on `DXGI_ERROR_DEVICE_REMOVED` or wgpu's device-lost callback ([Microsoft device-lost guidance](https://learn.microsoft.com/en-us/windows/uwp/gaming/handling-device-lost-scenarios)), swap to CPU on the next tick and retry the GPU with backoff.
4. **Visible reason:** show why in the admin status, as CLAUDE.md requires.

One catch: `OnPaint` and shared-texture mode are mutually exclusive, so the CPU fallback means re-creating the browser or keeping a CPU readback of the GPU frame. The same probe, self-test and fallback pattern already covers the encoder and DirectML, so a single "GPU available" check at startup can drive all three, with each failing independently.

## Ship Inno Setup now, Velopack deltas later

The camera DLL drives the installer design. VCamSample states that the media source can be registered only in **HKLM**, from a location readable by both Frame Server (LocalService) and Frame Server Monitor (LocalSystem), and that registering from a user-profile path "won't work" ([VCamSample](https://github.com/smourier/VCamSample)).

That narrows the options:

- **MSIX:** effectively ruled out. It virtualizes COM registration and blocks or redirects HKLM writes ([Microsoft Tech Community](https://techcommunity.microsoft.com/discussions/msix-discussions/msix-write-hklm-in-the-bubble/4351407/replies/4351982)). This is an inference, since no source tested an MSIX virtual camera.
- **WiX/MSI:** needs a Windows build host ([Tauri](https://tauri.app/distribute/windows-installer/)), and WiX v6+ is under the Open Source Maintenance Fee ([FireGiant](https://docs.firegiant.com/wix/releasenotes/)).

**Phase 1 is a per-machine Inno Setup installer.** Its file flags map directly onto the camera's needs: `regserver` registers the DLL, `restartreplace` handles the DLL being locked by Frame Server, and `uninsrestartdelete` covers uninstall ([Inno Setup [Files]](https://jrsoftware.org/ishelp/topic_filessection.htm)). It is free for non-commercial use ([jrsoftware](https://jrsoftware.org/isorder.php)). If everything must build on Linux, NSIS is the alternative, since Tauri documents makensis plus cargo-xwin cross-builds ([Tauri](https://tauri.app/distribute/windows-installer/)). Running Inno's compiler under Wine is unverified, so a GitHub Actions `windows-latest` job is the safe build route.

The installer reimplements `packaging/windows/install-camera.cmd`:

1. Stop FrameServerMonitor and FrameServer in `PrepareToInstall`.
2. Copy and register the DLL.
3. Create `%ProgramData%\Page2Cam` with LocalService and Users Modify ACLs.
4. Install vc_redist silently, only if it is missing.

The app needs four matching changes:

- Stop shipping `page2cam.toml` next to the exe, because Program Files isn't user-writable. Fall back to `<config_dir>/page2cam/config.toml`.
- Move logs to `%LocalAppData%\Page2Cam\logs`.
- Create an `AppMutex` so the installer can close the running app.
- Make `windows_cam.rs` report a missing or outdated camera component.

The updater is a small Rust module that:

1. Queries GitHub's `releases/latest` at most once a day.
2. Downloads the setup exe.
3. Verifies its SHA-256 against an ed25519-signed checksum file, using a public key compiled into the binary (`self_update`'s zipsign `signatures` feature is the sourced option; [self_update](https://docs.rs/self_update/latest/self_update/)).
4. Launches the installer elevated with `/VERYSILENT` and exits.

One relaunch problem is still open. Anything the elevated installer starts runs elevated, and `skipifsilent` entries never run under `/VERYSILENT`. So the app should spawn a non-elevated waiter before it exits, which restarts the app once the installer finishes.

**Phase 2 is Velopack**, adopted only if a full ~300 MB download plus a UAC prompt on every update proves annoying. Velopack can pack Windows releases on any OS, generates per-file Zstandard deltas against the previous GitHub release, and installs per-user to `%LocalAppData%` with "no elevation required" ([Velopack deltas](https://docs.velopack.io/packaging/deltas); [installer](https://docs.velopack.io/packaging/installer); [cross-compiling](https://docs.velopack.io/packaging/cross-compiling)). CEF, ffmpeg, ONNX Runtime and the models rarely change, so code-only updates should be a few MB (inferred, not measured).

In this design the camera DLL becomes a separately versioned component. The app compares a `CAMERA_COMPONENT_VERSION` with the installed DLL, and only when the DLL is out of date does it relaunch `page2cam.exe --install-camera` via `runas` to repeat the cmd script's steps in Rust. Two points are unverified: the ordering of Velopack's `VelopackApp::build().run()` against CEF's `maybe_run_subprocess()`, since both inspect argv, and whether Velopack's 30-second uninstall hook can raise a UAC prompt ([Velopack hooks](https://docs.velopack.io/integrating/hooks)).

**Signing comes first.** On Windows 11, Smart App Control "will block execution of unsigned files unless the file has a positive reputation". EV certificates "no longer bypass SmartScreen", and reputation transfers only within a single publisher identity ([Microsoft Learn](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation)). There are two realistic routes:

- **Azure Artifact Signing:** starts at $9.99/month, but individual developers must be in the US or Canada ([quickstart](https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart)).
- **SignPath Foundation:** free, but it requires CI builds, issues the certificate in SignPath's name, and forbids proprietary components ([SignPath terms](https://signpath.org/terms)). Bundling `vc_redist` and `DirectML.dll` needs clarifying with SignPath first.

jsign 7.x can drive either one from Linux ([jsign](https://github.com/ebourg/jsign)). Once the installer exists, add winget, which applies Inno and NSIS silent switches automatically ([winget manifest docs](https://learn.microsoft.com/en-us/windows/package-manager/package/manifest)).

## Ranked work list by effort and payoff

The Evidence column uses these labels:

- **In code:** read from the source during this research.
- **Measured:** a local benchmark on the dev box.
- **Sourced:** documented in the cited sources.
- Everything else is an estimate.

| # | Change (where) | Effort | Payoff | Evidence |
|---|---|---|---|---|
| 1 | DML `with_memory_pattern(false)`, `with_parallel_execution(false)`; log adapter/EP; device id in config (`p2c-segment/runtime.rs`) | Very low | May decide between GPU and a silent CPU fallback | In code; ORT runtime behaviour unverified |
| 2 | One shared A/V clock, no video reset, video starts after audio accept (`p2c-stream/controller.rs`) | Low | Removes permanent audio-late offset after stalls | In code; effect derived, not observed |
| 3 | Bitrate ≥6 Mbps at 1080p30 or a 720p preset; drop `zerolatency`; RTMPS (`config.rs`, `args.rs`, `page2cam.toml`) | Trivial | Clears YouTube's 5 Mbps floor; better quality per bit | YouTube minimum sourced; quality gain estimated |
| 4 | Composite RVM `fgr`+`pha`, native aspect, premultiplied scaling (`engine.rs`, `tensor.rs`, `compose.rs`) | Low–medium | Removes halo and 4:3 distortion | Mechanism sourced; visual gain unmeasured |
| 5 | Async RVM at 15 Hz; 3–4 threads, no spinning; static skip on MediaPipe (`p2c-segment`) | Low | ~2× lower average RVM CPU | Estimate |
| 6 | Hardware encoder probe with libx264 fallback (`p2c-stream`) | Medium | 0.65 → 0.07 core with NVENC | Measured (desktop, synthetic) |
| 7 | NV12 into ffmpeg except NVENC; BT.709 tags | Low–medium | −63% pipe bandwidth; −40% ffmpeg-side CPU | Measured ffmpeg side; net unmeasured |
| 8 | Buffer pool, PIP cache, `jpeg-encoder/simd`, event-driven preview (`handlers.rs`, `compose.rs`, `preview.rs`) | Low | Several ms/frame, much less memory traffic | Copies in code; savings estimated |
| 9 | Restart ffmpeg with backoff; parse `speed`/`drop`; smaller `thread_queue_size` | Low–medium | Streams survive Wi-Fi blips; self-explaining failures | ~690 MB stall RSS measured once |
| 10 | Per-machine Inno installer + signed update checker + code signing | Medium | Installable, updatable, not blocked by Smart App Control | Sourced; untested on hardware |
| 11 | MediaPipe post-processing chain | Medium | Usable "fast" mode | Sourced (OBS, Slack) |
| 12 | yuvutils-rs for NV12/YUYV/webcam conversion (`p2c-shm`, `p2c-vcam`, `p2c-webcam`) | Medium | ~0.3–0.6 ms per conversion; correct chroma | Benchmark sourced; extrapolated |
| 13 | More vcam media types; Frame Server restart/resume handling (`p2c-vcam-win-source`, `windows_cam.rs`) | Low–medium | Compatibility, possibly Teams | Speculative for Teams |
| 14 | DML fp16 + IoBinding ping-pong; static RVM re-export; full-res DGF on GPU tier | Medium | Largest GPU segmentation win | Runtime-unverified |
| 15 | Drift-correcting resampler; flash-and-beep sync test | Medium | No clicks on long streams; sync as a number | Mechanism sourced |
| 16 | CEF `multi_threaded_message_loop`; external begin frame | Medium | Smoother cadence, lower idle CPU | No benchmark found |
| 17 | Velopack per-user deltas + elevated camera component | Medium–high | Few-MB updates without UAC | Delta size unmeasured |
| 18 | wgpu compositor + CEF `accelerated_osr` behind a `Compositor` trait (Windows first) | High | Large only end to end on GPU | Linux blocked by CEF #4313 until M158 |
| 19 | Studio Effects mask, Windows ML NPU EPs, int8 CPU RVM | High | Hardware-specific | Mostly unverified |

## Conclusion

Most of Page2Cam's performance and quality debt is in its glue code, not in its architecture or its models. The latest-only slots, the absolute-deadline ticker and the ffmpeg child process are sound. The defects are a writer that resets a clock, a session builder that skips two documented options, and a matte path that discards the foreground RVM already computes. The rule that falls out for "GPU when available" is: **offload whole stages that already have a GPU-native consumer** (the encoder first, then the segmentation tensors on DML), and don't move single CPU stages onto the GPU between CPU neighbours. Each offload should carry its own probe, self-test and fallback, so that a bad driver on the user's PC degrades one stage visibly instead of breaking the app.

Two cheap harnesses would turn most of the estimates above into data, and they should come before items 4 to 8, whose order is otherwise a well-informed guess:

- **Per-stage timing:** `Instant` timing in the ticker and sinks, logged as p50/p99, plus a criterion sweep over RVM resolution, ratio, refiner size and thread count.
- **Sync test:** the flash-and-beep test against the local RTMP listener.

On distribution, the binding constraint is Windows trust, not tooling. Smart App Control can block an unsigned Page2Cam outright, so decide on signing (Artifact Signing versus SignPath, which depends on where the developer lives and how SignPath treats the bundled Microsoft binaries) before writing any installer code.
