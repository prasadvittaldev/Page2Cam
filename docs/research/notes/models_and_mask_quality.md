# Person segmentation / video matting models and mask quality for Page2Cam

Scope: which models and which mask post-processing give the best-looking real-time webcam cut-out over a live page, under GPL-3.0 distribution, Windows 11 laptop (CPU or DirectML), >=15-30 fps at 720p.

Context from the code (read 2026-11-18, not a web source): `crates/p2c-segment/src/engine.rs` feeds RVM a 640x360 resize of the webcam frame with `downsample_ratio = 0.4` (so the encoder runs at 256x144 and RVM's own Deep Guided Filter upsamples to 640x360), reads only `pha`, drops `fgr`. `tensor.rs::write_alpha` then bilinearly upsamples the 640x360 (or MediaPipe 256x144) mask into the webcam frame's alpha byte, leaving the raw BGR untouched; `p2c-compositor/src/compose.rs::blend` does straight-alpha `cam*a + page*(1-a)`. A 4:3 webcam is squashed to 640x360.

## 1. Which models (2023-2026 and incumbents) beat or complement RVM / MediaPipe?

### Takeaway
No permissively licensed, real-time, laptop-class model published 2023-2026 clearly beats RVM-MobileNetV3 for webcam video matting; the newer, higher-quality video matters (MatAnyone/MatAnyone 2, SAM2-based) are either non-commercial licensed or far too heavy. The best "quality" option remains RVM, used better (full-resolution DGF, native aspect, use `fgr`); the best "fast" options are MediaPipe selfie (landscape/general) or PP-HumanSegV2-Lite, both segmenters that need post-processing to look good.

### Cited Findings

**Robust Video Matting (RVM), 2021 (older, still the reference real-time video matter)**
- Licence GPL-3.0 ("Code is re-released under GPL-3.0 license"); official ONNX exports `rvm_mobilenetv3_fp32/fp16.onnx` and `rvm_resnet50_fp32/fp16.onnx`, opset 12, "Tested on ONNX Runtime with CPU and CUDA backends" — [RVM README](https://github.com/PeterL1n/RobustVideoMatting)
- Recommended `downsample_ratio`: 0.25 for HD, 0.125 for 4K; CoreML exports list 1280x720 at s=0.375 and 1920x1080 at s=0.25 — [RVM README](https://github.com/PeterL1n/RobustVideoMatting)
- Architecture: MobileNetV3-Large + LR-ASPP encoder, ConvGRU recurrent decoder, and a Deep Guided Filter (DGF) module: "the input frame [is downsampled] by a factor s before passing through the encoder-decoder network. Then the low-resolution alpha, foreground, final hidden features, as well as the high-resolution input frame are provided to the DGF module to produce high-resolution alpha and foreground." Outputs are 1-ch alpha, 3-ch foreground, 1-ch segmentation — [RVM paper, arXiv 2108.11515](https://arxiv.org/abs/2108.11515)
- Trained with an explicit temporal-coherence loss on both alpha and foreground (d alpha/dt and dF/dt vs ground truth) — [RVM paper](https://arxiv.org/abs/2108.11515)
- Quality, VideoMatte240K 512x288 (Table 1): RVM MAD 6.08, MSE 1.47, Grad 0.88, Conn 0.41, dtSSD 1.36; MODNet MAD 9.41, MSE 4.30, Grad 1.89, Conn 0.81, dtSSD 2.23; BGMv2 MAD 25.19; FBA (with DeepLabV3 trimap) MAD 8.36. Foreground MSE: "ours is behind BGMv2 but outperforms FBA and MODNet" — [RVM paper](https://arxiv.org/abs/2108.11515)
- Quality at 1920x1080 with s=0.25 (Table 2): RVM SAD 6.57 / dtSSD 1.90 vs MODNet+Fast Guided Filter SAD 11.13 / dtSSD 3.08 — [RVM paper](https://arxiv.org/abs/2108.11515)
- Size/speed (Tables 3-4, GTX 1080 Ti FP32 tensor throughput): RVM 3.749 M params / 14.5 MB; MODNet 6.487 M / 25.0 MB. 512x288 s=1: RVM 131.9 fps (4.57 GMACs), MODNet 104.9 fps. 1920x1080 s=0.25: RVM 104.2 fps (4.15 GMACs) vs MODNet+FGF 100.3. 4K s=0.125: RVM 76.5 fps — [RVM paper](https://arxiv.org/abs/2108.11515). README table (FP16): RTX 3090 HD 172 fps, RTX 2060 Super HD 134 fps — [RVM README](https://github.com/PeterL1n/RobustVideoMatting). Note: GMACs at HD (4.15) is lower than at 512x288 s=1 (4.57) because the encoder runs at s*resolution; DGF's cost is mostly resize/elementwise ops not counted in GMACs ("GMACs is only a rough approximation") — [RVM paper](https://arxiv.org/abs/2108.11515)
- Temporal: RVM's MAD "drops significantly in the first 15 frames then the metric stays stable. MODNet, even with its neighbor frame smoothing trick, has large fluctuations" — [RVM paper](https://arxiv.org/abs/2108.11515)
- OpenVINO model zoo lists RVM-MobileNetV3 at 9.3892 GFlops, 3.7363 MParams (at its reference input) — [OpenVINO OMZ](https://docs.openvino.ai/2024/omz_models_model_robust_video_matting_mobilenetv3.html)

**MODNet (AAAI 2022; older)**
- Apache-2.0 for code, models and demos; ONNX conversion code in repo; webcam demos; no released fps numbers — [ZHKKKe/MODNet](https://github.com/ZHKKKe/MODNet)
- OpenVINO "modnet-webcam-portrait-matting": MobileNetV2 backbone, 512x512 input, 31.16 GFlops, 6.46 MParams — [OpenVINO OMZ README](https://github.com/openvinotoolkit/open_model_zoo/blob/master/models/public/modnet-webcam-portrait-matting/README.md)
- Per-frame (not temporal); does not predict foreground; worse than RVM on every metric above — [RVM paper](https://arxiv.org/abs/2108.11515)

**MediaPipe Selfie Segmentation (Google; current "Fast")**
- Landscape model 144x256 float16, general 256x256; landscape is "designed for cases where the input always has that shape, such as video calls" — [MediaPipe image segmenter guide](https://ai.google.dev/mediapipe/solutions/vision/image_segmenter)
- Model card: average IoU ~96.08% landscape, 96.57% general; ~1.15% range across skin-tone groups — [MediaPipe Selfie model card (mirror)](https://dcmpx.remotevs.com/com/googleapis/storage/SL/mediapipe-assets/Model%20Card%20MediaPipe%20Selfie%20Segmentation.pdf) (third-party mirror of Google's storage bucket)
- Licence: Qualcomm AI Hub lists the source model as Apache-2.0 — [Qualcomm AI Hub](https://aihub.qualcomm.com/models/mediapipe_selfie); I did not see the statement on Google's own card.
- It is a binary segmenter (soft confidence, not a matte): Slack found half-opacity compositing made people look "ghostly" and used threshold mapping + bilateral filtering instead — [Slack Engineering, 2022](https://slack.engineering/building-background-effects-for-clips/)

**MediaPipe Selfie Multiclass 256x256**
- 6 classes (background, hair, body-skin, face-skin, clothes, others); output 256x256x6 — [Multiclass model card (mirror)](https://dcmpx.remotevs.com/com/googleapis/storage/SL/mediapipe-assets/Model%20Card%20Multiclass%20Segmentation.pdf)
- Latency on Pixel 6: ~217.76 ms CPU / ~71.24 ms GPU (column pairing slightly ambiguous) — [MediaPipe image segmenter guide](https://developers.google.com/edge/mediapipe/solutions/vision/image_segmenter); ~16 MB — [pub.dev package docs](https://pub.dev/documentation/face_detection_tflite/latest/face_detection_tflite_native/SelfieSegmentation-class.html)
- No explicit licence found for this model.

**Google Meet segmentation models**
- 2020: modified MobileNetV3-small encoder, 193K params, 400 KB fp16; input 256x144 or 160x96 chosen by hardware; ~93.6% IoU for the larger model; ~70 fps end-to-end at 720p on 2018 MacBook Pro, 33 fps on Acer Chromebook 11 — [Google Research blog, Oct 2020](https://research.google/blog/background-features-in-google-meet-powered-by-web-ml/) (older)
- 2022 HD segmenter: EfficientNet-Lite backbone (no squeeze-excite, inefficient on WebGL), decoder of 1x1-conv MLP, NAS-tuned; input 512x288 (vs 256x144); IoU 96.9% vs 94.0% for the CPU segmenter; 4.3 ms WebGL vs 8.7 ms Wasm SIMD on 2018 MacBook Pro; "crisper boundaries around hair and fingers" — [Google Research blog, Aug 2022](https://research.google/blog/high-definition-segmentation-in-google-meet/). The 512x288 model itself is not openly published as far as found.
- OBS plugin ships a "MediaPipe Meet Segmentation" model via PINTO model zoo #082 — [royshil/obs-backgroundremoval](https://github.com/royshil/obs-backgroundremoval)

**PP-HumanSeg v2 (PaddleSeg)**
- PP-HumanSegV2-Lite (portrait): 256x144 input, mIoU 96.63% on PP-HumanSeg-14K, 15.86 ms on Snapdragon 855 single thread (PaddleLite), 5.4 MB; V1-Lite 398x224, 93.60%, 29.68 ms — [PP-HumanSeg README](https://github.com/PaddlePaddle/PaddleSeg/blob/release/2.9/contrib/PP-HumanSeg/README.md)
- Used as an option in the OBS plugin — [royshil/obs-backgroundremoval](https://github.com/royshil/obs-backgroundremoval)

**SINet** — offered in the OBS plugin (from anilsathyan7/Portrait-Segmentation) — [royshil/obs-backgroundremoval](https://github.com/royshil/obs-backgroundremoval). No current numbers found (2019-2020 era model).

**BiRefNet / BiRefNet_lite (MIT)**
- MIT licence, inherited by variants and ONNX re-hosts — [BiRefNet-lite ONNX on HF](https://huggingface.co/CoderViking/birefnet-lite-onnx); [Replicate readme](https://replicate.com/sprited/birefnet/readme)
- Full BiRefNet ~17 fps at 1024x1024 on an RTX 4090 FP16, 3.45 GB VRAM (project changelog, as reported) — [Replicate readme](https://replicate.com/sprited/birefnet/readme); BiRefNet_lite = Swin-Tiny backbone, 44.4 M params — [PromptLayer listing](https://www.promptlayer.com/models/birefnetlite); a WebGPU fp16 export of lite reportedly ~700 ms/frame at 1024 on a 4090 — [runes/birefnet-lite-webgpu](https://huggingface.co/runes/birefnet-lite-webgpu)
- BiRefNet_dynamic(-matting) trained at 256x256..2304x2304, no need to resize to 1024 — [BiRefNet_dynamic-matting](https://huggingface.co/ZhengPeng7/BiRefNet_dynamic-matting)
- Per-image, no temporal state.

**MatAnyone / MatAnyone 2 (2025)**
- MatAnyone 2 (arXiv 2512.11782, CVPR 2026) — state-of-the-art claims on synthetic and real benchmarks; licensed under **NTU S-Lab License 1.0**, non-commercial redistribution; Beeble runs it under a separate licence — [arXiv 2512.11782](https://arxiv.org/abs/2512.11782v1); [MatAnyone HF demo](https://peiqingyang-matanyone.hf.space/); [Beeble](https://beeble.ai/models/matanyone)

**SAM-based / generative matting (2025-2026)**
- SAM2Matting (arXiv 2606.27339) and VideoMaMa ("Mask-Guided Video Matting via Generative Prior", arXiv 2601.14255) exist — [SAM2Matting](https://arxiv.org/pdf/2606.27339); [VideoMaMa](https://arxiv.org/pdf/2601.14255). I did not obtain speed/licence; SAM2 / diffusion backbones are offline-class.
- DART (arXiv 2402.15820, 2024) "Depth-Enhanced Accurate and Real-Time Background Matting" needs a background/depth — [arXiv 2402.15820](https://arxiv.org/pdf/2402.15820)

**NVIDIA Maxine AI Green Screen (proprietary, NVIDIA RTX only)**
- 4 modes: quality / performance x chairs-as-foreground/background; Mode 0 (quality) is default; output 8-bit alpha at input resolution with confidence-based soft values; stateful to keep results consistent across frames; latency ~1.7 ms RTX 3080, 1.02 ms RTX 4090 at 720p; input >=512x288 and "aspect ratio... must be 16:9 for best results"; works best with one person — [NVIDIA Maxine AIGS docs](https://docs.nvidia.com/maxine/vfx/latest/Filters/AIGreenScreen.html); [Performance reference](https://docs.nvidia.com/maxine/vfx/WindowsVFXSDK/PerformanceReference.html)

### Inferences
- For "Quality", stay on RVM-MobileNetV3 (GPL-3.0 matches the project; temporal; only model in the field that is real-time, permissive-enough, and outputs a foreground estimate). Its weaknesses in Page2Cam are integration choices (see sections 2-4), not the model.
- RVM-ResNet50 (also GPL-3.0, same ONNX set) is a possible "Max" tier on discrete GPUs via DirectML; the README's HD fps table doesn't separate backbones, so its laptop cost must be measured.
- For "Fast", MediaPipe landscape is fine at 16:9 input; PP-HumanSegV2-Lite is an alternative with similar size/IoU claims (different datasets, so not directly comparable). Neither gives a matte; both need the refinement pipeline in section 2 (guided upsampling + temporal smoothing + edge remap) to look acceptable.
- MODNet: Apache-2.0 but strictly worse than RVM and no foreground; no reason to add it. BiRefNet(-lite): best per-image matte quality among MIT models, but ~1-2 orders too slow for a laptop at 30 fps and flickers without temporal state; at most a "still/background-snapshot" tool. MatAnyone: licence incompatible.

### Gaps
- No independent CPU / DirectML ms-per-frame numbers on laptop hardware found for RVM, MediaPipe, PP-HumanSegV2 or MODNet; must be benchmarked locally.
- Licence of PP-HumanSeg weights not visible in the README fetched (PaddleSeg as a project is believed to be Apache-2.0, unverified here). Licence of the MediaPipe multiclass model not found.
- No 2023-2026 open, real-time, permissive "RVM successor" with code and numbers was found (searched; only papers without code/speed, or non-commercial ones).
- VideoMatte240K MAD for MediaPipe / PP-HumanSeg not available (they report IoU on their own sets).

## 2. Mask post-processing: upsampling, temporal smoothing, cleanup, thresholds

### Takeaway
Everyone who ships this upsamples the low-res mask with an edge-aware filter guided by the full-resolution frame (Meet: joint bilateral; Teams and RVM: deep guided filter; Slack: bilateral), not plain bilinear. For segmenter models, add temporal smoothing (EMA ~0.85 in the OBS plugin), a soft threshold remap, small-blob removal and slight erosion. For RVM, the simplest big win is to let RVM's own DGF upsample to the webcam resolution.

### Cited Findings
- Meet: "In the refinement stage, we apply a joint bilateral filter to smooth the low resolution mask"; refinement is skipped on low-end devices — [Google Research blog 2020](https://research.google/blog/background-features-in-google-meet-powered-by-web-ml/)
- Teams (Microsoft Research poster, June 2022): NAS-found lightweight backbones under ms-per-frame budgets; a "mapping model designed to update model parameters dynamically" for lighting, "significantly reducing temporal flickering"; a Deep Guided Filtering module "processes a low-resolution initial mask and uses the high-resolution input as a guide" to recover hair strands in real time — [Microsoft Research, Person Segmentation in Teams](https://www.microsoft.com/en-us/research/?p=1172105)
- RVM chose filter-based upsampling (DGF) "because it is faster and well supported by all inference frameworks"; DGF and Fast Guided Filter "incur very minor differences in performance" — [RVM paper](https://arxiv.org/abs/2108.11515)
- Guided filter: O(N) in pixels, independent of radius; Fast Guided Filter subsamples by s for O(N/s^2) and >10x speed-up at s=4 — [He & Sun, Fast Guided Filter, 2015](https://arxiv.org/pdf/1505.00996); [He et al., Guided Image Filtering, TPAMI](https://people.csail.mit.edu/kaiming/publications/pami12guidedfilter.pdf) (older)
- Deep/trainable guided filter as differentiable joint upsampling, 10-100x faster than alternatives on FiveK — [Wu et al., CVPR 2018, "Fast End-to-End Trainable Guided Filter"](https://arxiv.org/abs/1803.05619v2) (older)
- Slack Clips (MediaPipe landscape): upsample, then edge-preserving bilateral filter reduces pixelation/noise "including haloing around hair and face"; threshold mapping to near-binary edges for replacement (blur is forgiving, replacement is not); background-replacement smoothing weighted by an inverted parabola centred at 0.5 (most blur where confidence is lowest); edge-detection fusion with the video was tried and not shipped — [Slack Engineering, June 2022](https://slack.engineering/building-background-effects-for-clips/)
- OBS Background Removal plugin (GPL-3.0) pipeline and defaults, from `src/background-filter.cpp` — [source](https://raw.githubusercontent.com/royshil/obs-backgroundremoval/main/src/background-filter.cpp):
  - skip inference when PSNR to last processed frame > 35 dB (`enable_image_similarity` on), reuse previous mask; optional `mask_every_x_frames` (default 1)
  - threshold at 0.5 (default on)
  - temporal smoothing `cv::addWeighted(current, prev)` with `temporal_smooth_factor` default 0.85 (EMA)
  - contour filter: keep only external contours with area > 5% of the image, filled (removes stray blobs)
  - smooth contour: stackBlur with k = 3 + 11*0.5 → 9x9, then upscale to frame size with `cv::resize`, then re-binarize at 128
  - `mask_expansion` (-30..30, positive = erode), `feather` (default 0: dilate then box filter k = 40*feather)
  - models: SINet, MediaPipe (default), Selfie, Selfie Multiclass, PPHumanSeg, RVM, TCMonoDepth
- Maxine: alpha can be thresholded to hard 0/255 or kept as confidence for soft edges — [NVIDIA Maxine AIGS](https://docs.nvidia.com/maxine/vfx/latest/Filters/AIGreenScreen.html)
- RVM is trained with temporal coherence losses and stabilises within ~15 frames; MODNet needed a neighbour-frame smoothing trick and still fluctuated — [RVM paper](https://arxiv.org/abs/2108.11515)

### Inferences (concrete recommendations for Page2Cam)
- **RVM path:** stop resizing to 640x360. Feed RVM the native webcam frame (e.g. 1280x720, or 640x480 / 960x720 for 4:3) with `downsample_ratio` chosen so the encoder sees ~256-384 px on the long side (e.g. 0.25 at 1280x720 → 320x180, close to the current 256x144 cost; 0.375 matches the README's CoreML 720p setting). DGF then produces alpha *and* foreground at the full webcam resolution, guided by the full-res frame — exactly the Teams/RVM design — and `write_alpha`'s bilinear upscale disappears. DGF cost at 720p is a few elementwise/box-filter ops; must be measured on DirectML/CPU. (RVM ONNX exports accept dynamic H/W since `downsample_ratio` is an input; confirm the shipped file isn't fixed-shape.)
- **Do not EMA-smooth RVM's alpha heavily**: it's already temporally trained; extra EMA adds motion lag/ghost trails. At most a light EMA (0.2-0.3 of previous) or none.
- **MediaPipe/segmenter path:** (1) upsample with a guided filter (radius ~4-8 px at 720p, eps ~1e-3..1e-2 on 0..1 intensities) using the webcam luma as guide — Fast Guided Filter computed at 1/2 or 1/4 res is cheap on CPU; (2) temporal EMA on the low-res mask (OBS's `temporal_smooth_factor` default is 0.85 in an `addWeighted(current, previous)` blend; the exact weighting direction was not verified from the snippet. Suggested start for Page2Cam: weight ~0.5-0.7 on the new frame, with a per-pixel reset to the new value when |new - old| is large, so fast motion doesn't trail); (3) soft remap instead of hard threshold: `a = smoothstep(lo, hi, p)` with e.g. lo=0.35, hi=0.65 (a soft ramp avoids binary aliasing yet kills low-confidence fog); (4) hysteresis per pixel (enter fg at >0.6, leave at <0.4) if flicker remains; (5) keep largest connected component(s) / drop blobs <~2-5% of area (OBS's contour filter); (6) erode 1-2 px at output res before feathering, to pull the edge inside the halo zone.
- **Skip inference on static frames** (PSNR gate as in OBS) saves CPU on a static presenter, but with RVM it also stalls the recurrent state; safe only for the segmenter path.

### Gaps
- No published Teams/Zoom parameter values (radius, eps, EMA factors). Zoom publishes nothing technical on its segmentation; a third-party article says Zoom's "I have a green screen" mode switches to chroma keying — [Fora Soft](https://www.forasoft.com/learn/ai-for-video-engineering/articles-ai/virtual-backgrounds-segmentation-composite) (low authority).
- Flow-guided temporal smoothing for masks: no real-time shipped example found; optical flow at 720p on CPU would likely cost more than the model itself (inference, not sourced).

## 3. Edge colour: foreground estimation, fgr, decontamination, light wrap, premultiplied alpha

### Takeaway
The halo comes from compositing raw webcam pixels: at edge pixels I = aF + (1-a)B, so straight `I*a + page*(1-a)` re-adds the room colour B. The fix is to composite an estimated foreground F instead of I. RVM already outputs F (`fgr`), trained against ground truth with a temporal loss; for segmenter models a cheap per-pixel decontamination (or Germer's multi-level estimator at reduced resolution) is the option. Meet adds light wrap to blend the person into the new background.

### Cited Findings
- RVM returns `fgr, pha, *rec`; composite is `com = fgr * pha + bgr * (1 - pha)` — [RVM README](https://github.com/PeterL1n/RobustVideoMatting). Foreground is learned with L1 + temporal-coherence loss on pixels with a>0; its foreground MSE beats FBA and MODNet (MODNet "does not predict foreground so it is evaluated on the input image"), e.g. VM-D646 FG MSE: RVM 2.93 vs MODNet 6.31; AIM: 5.01 vs 9.51 — [RVM paper](https://arxiv.org/abs/2108.11515)
- Fast Multi-Level Foreground Estimation (Germer et al., ICPR 2020; older): solves the foreground at a coarse level, upsamples as initialisation for the next level; avg runtime ~2.04 s (HPC) / 1.48 s (Mac) per benchmark image vs ~26-28 s for closed-form; available as `pymatting.estimate_foreground_ml(image, alpha)` — [arXiv 2006.14970](https://arxiv.org/abs/2006.14970); [poster](https://tuelwer.github.io/data/posters/germer2020fast.pdf)
- Meet: for replacement, "light wrapping" blends the person into the new background; for blur, separable filters replace a Gaussian pyramid to reduce halo artifacts — [Google Research blog 2020](https://research.google/blog/background-features-in-google-meet-powered-by-web-ml/)
- Slack: blur must not sample from inside the mask, otherwise haloing appears — [Slack Engineering](https://slack.engineering/building-background-effects-for-clips/)

### Inferences (concrete recommendations)
- **Use RVM's `fgr`** (at full webcam resolution via DGF, see section 2) as the colour source in the cut-out: write `fgr` into BGR and `pha` into A. This removes most room-colour spill at hair and shoulders at near-zero extra cost (it's already computed; only the copy/conversion is added). Clamp fgr to 0..1.
- **Hybrid to keep sharpness**: RVM's fgr can be softer than the camera image in the interior. Use `F = lerp(I, fgr, w(a))` with w=1 where 0<a<~0.95 and w=0 for a≈1 (i.e. only replace colour in the edge band). Inference, needs visual check.
- **Segmenter path decontamination (cheap, per pixel)**: estimate a local background colour B as a blur of I weighted by (1-a) (masked blur, computed at low res), then F = (I - (1-a)B)/max(a, eps), clamped; or simply pull edge colours toward a masked blur of I weighted by a (inner colour push). Germer's ML estimator is ~1-2 s per large image on CPU (2020 numbers), so only viable at low res (e.g. 320x180) and/or every N frames; prefer the cheap approximation for real time.
- **Premultiplied vs straight**: the compositor currently scales the cut-out with `scale_straight` (`fast_image_resize` with `use_alpha(false)`, i.e. alpha is not treated as premultiplication; confirmed in `compose.rs::compose`). When a cut-out with straight alpha is bilinearly resampled (PIP resize), transparent pixels' (room) colours bleed into the edge — the classic dark/coloured fringe. Resample in premultiplied space (multiply RGB by a, scale, then composite with `out = Fp + page*(1-a)`), which also matches RVM's `fgr*pha` form. Applies equally to mask upsampling: upsample a, and if colours are resampled, resample a*F.
- **Light wrap** (optional polish): add `k * blur(page) * (1 - a_blurred) * a` near edges so page colour wraps slightly onto the person; low k (0.1-0.3), blur radius ~1-2% of PIP width. Cheap on the composited PIP-size region only.
- **Erode then feather**: 1-2 px erosion of alpha before feathering helps when no foreground estimate is available.

### Gaps
- No published numbers on how much fgr reduces halo in webcam use; no Teams/Zoom/Apple descriptions of colour decontamination found.
- Maxine docs describe only alpha output, nothing on foreground colour estimation.

## 4. How Teams, Zoom, Meet, NVIDIA Broadcast/Maxine, Apple and OBS do it

### Takeaway
Published details converge on: tiny NAS-tuned segmentation backbone at ~256x144-512x288, edge-aware upsampling to full resolution guided by the camera frame (joint bilateral or deep guided filter), some temporal stabilisation, and render-time tricks (light wrap, halo-free blur). Only Meet and the OBS plugin publish concrete details.

### Cited Findings
- Google Meet: see section 1 and 2 (MobileNetV3-small 256x144/160x96 → HD EfficientNet-Lite 512x288 on WebGL; joint bilateral refinement; light wrap; separable blur) — [2020 blog](https://research.google/blog/background-features-in-google-meet-powered-by-web-ml/); [2022 blog](https://research.google/blog/high-definition-segmentation-in-google-meet/)
- Teams: NAS backbones, lighting-adaptive parameter mapping to cut flicker, deep guided filter for full-res edges (2022 poster, no numbers) — [Microsoft Research](https://www.microsoft.com/en-us/research/?p=1172105). Microsoft Garage page says blur shipped in 2018 built with Microsoft Research Asia and claims cloud processing — [Microsoft Garage](https://microsoft.com/en-us/garage/wall-of-fame/mixed-reality-video-calling-for-microsoft-teams-together-mode); low-authority guides say processing is local — conflicting, unresolved.
- Zoom: no technical publication found; "with green screen" mode reportedly uses chroma key — [Fora Soft](https://www.forasoft.com/learn/ai-for-video-engineering/articles-ai/virtual-backgrounds-segmentation-composite) (third-party).
- NVIDIA Maxine AIGS (basis of NVIDIA Broadcast): stateful temporal tracking, confidence alpha at input resolution, quality/performance modes, ~1-2 ms on RTX, 16:9 input preferred — [Maxine AIGS docs](https://docs.nvidia.com/maxine/vfx/latest/Filters/AIGreenScreen.html)
- Apple Vision `VNGeneratePersonSegmentationRequest`: quality levels accurate / balanced / fast; balanced for video frames, fast for streaming; accurate much larger and slower — [Kodeco tutorial](https://www.kodeco.com/29650263-person-segmentation-in-the-vision-framework?page=2); [Better Programming](https://betterprogramming.pub/new-in-ios-15-vision-person-segmentation-5c031a2f3822) (not relevant on Win/Linux, but shows the same tiered approach).
- OBS Background Removal plugin: GPL-3.0; models and full post-processing chain listed in section 2; GPU backends CUDA/ROCm/MIGraphX (Linux from source), CoreML (macOS); "A 2-thread setting works best" — [royshil/obs-backgroundremoval](https://github.com/royshil/obs-backgroundremoval)

### Inferences
- Page2Cam's current pipeline matches the OBS plugin's minus its cleanup steps, and is below Meet/Teams in edge handling (plain bilinear instead of guided upsampling, raw I instead of F). Closing that gap is mostly integration, not a new model.
- The OBS plugin being GPL-3.0 means its post-processing logic can be ported directly.

### Gaps
- Apple and Zoom publish no model/refinement internals; Teams publishes no numbers.

## 5. Aspect ratio and model input resolution

### Takeaway
Don't squash. RVM is fully convolutional and accepts any resolution via `downsample_ratio`, so feed native aspect. Fixed-shape models (MediaPipe landscape 256x144) should get a letterboxed (pad) or centre-cropped input of the right aspect with the mask un-padded afterwards; or use the 256x256 general model for 4:3.

### Cited Findings
- RVM: resolution handled via `downsample_ratio` ("adjust based on your video"; auto when None); CoreML exports are fixed-size only because CoreML lacks dynamic resolution — [RVM README](https://github.com/PeterL1n/RobustVideoMatting)
- RVM DGF design: encoder at s x resolution, DGF restores full resolution with the full-res frame as guide — [RVM paper](https://arxiv.org/abs/2108.11515)
- MediaPipe landscape intended for inputs that always have that shape (video calls); general 256x256 for others — [MediaPipe guide](https://ai.google.dev/mediapipe/solutions/vision/image_segmenter)
- Maxine: "aspect ratio... must be 16:9 for best results" and min 512x288 — [Maxine AIGS](https://docs.nvidia.com/maxine/vfx/latest/Filters/AIGreenScreen.html)
- Meet: doubling input resolution roughly quadruples compute; moved 256x144 → 512x288 on GPU for crisper hair/fingers — [Google 2022 blog](https://research.google/blog/high-definition-segmentation-in-google-meet/)

### Inferences
- RVM: feed e.g. 640x480 webcam at s≈0.5 (encoder 320x240) or 1280x720 at s=0.25 (320x180); for 4:3 1280x960/960x720 use s so the encoder side is ~256-384 px. This both fixes the 4:3 distortion (squashing changes the person's proportions the model was trained on) and gives full-res alpha+fgr.
- MediaPipe: for 4:3, letterbox to 16:9 with black/edge-replicated bars (person shrinks a little) or switch to the 256x256 general model; centre-crop is better only if the PIP crops anyway. After inference, crop the mask back to the real image area before upsampling.
- Resolution tiers on a laptop: Fast = MediaPipe 256x144 + guided upsampling; Quality = RVM-MobileNetV3 encoder ~320x180 + DGF at full res + fgr; optional Max (GPU/DirectML) = RVM s=0.375-0.5 at 720p (encoder 480x270-640x360) or RVM-ResNet50.

### Gaps
- No measured DirectML/CPU timings for RVM at different (resolution, downsample_ratio) pairs on laptop hardware; must benchmark (ort session with DirectML EP vs CPU, 720p, s in {0.25, 0.375, 0.5}).
- Whether the shipped `rvm_mobilenetv3_fp32.onnx` has dynamic spatial axes was not checked in this research (needs inspection of the model file's input shape).
