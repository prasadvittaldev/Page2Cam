# Webcam cut-out (background removal) — Design

Date: 2026-11-18
Status: Draft, awaiting user review
Builds on: `2026-11-17-page2cam-design.md` (Page2Cam is otherwise unchanged)

## Intent

The user presents **sitting at their desk**. Today the webcam overlay is a rectangle showing their room. They want the **web page to be their background**: their room is removed and they appear in front of the page, the way Teams' background replacement works, except that the background is the live, interactive page. Teams' cut-out quality is the bar ("good enough"). The user asked us to look for better options, and we found one.

## Decided with the user

- **Three overlay styles**, chosen on the admin page:
  - **Box:** today's rectangular PIP, unchanged.
  - **Cut-out, small:** only the person (room removed), anchored to the bottom edge of the output, and draggable and resizable like the box. The page stays mostly readable.
  - **Cut-out, full size:** the person at normal camera framing with the room replaced by the page, like a Teams background. It covers part of the page.
  
  The user picked all three. Mockups: `.superpowers/brainstorm/*/content/overlay-modes-v2.html`.
- **Two cut-out engines with a Quality / Fast switch on the admin page:**
  - **Quality:** Robust Video Matting (RVM, MobileNetV3). Soft hair edges, and it uses previous frames so the edge doesn't flicker.
  - **Fast:** MediaPipe selfie segmentation (landscape, 256×144), similar to Meet and Teams.
- **Smooth video beats edge quality.** If Quality can't keep up, the app falls back to Fast **automatically** and the admin page says that it did.
- **Nothing extra to install.** The models and the inference runtime ship inside the zip. Windows 11 and Linux, as before.
- The same composited picture goes to the virtual camera, the admin preview and the YouTube stream.
- There were no other overlay effects in scope. The user was asked about blurring the page, borders and shadows, and accepted the list as is.

## Goals

- In both cut-out styles, the person is composited over the page using a per-pixel alpha mask, not a hard on/off mask. Edges are soft.
- Output frame rate stays at the configured fps (30) in every style. Segmentation must never stall the ticker. If inference is slower than the camera, the compositor reuses the latest mask (a FrameSlot-style latest-value hand-off, like everything else in the app).
- Fast mode runs in real time on a typical laptop CPU.
- Quality mode uses a GPU where that helps (DirectML on Windows; on Linux, CPU is acceptable) and runs at a reduced internal resolution if needed.
- Auto-fallback: if Quality mode's mask rate stays below a threshold for a few seconds (our call: below about 15 masks/s for 3 s), switch to Fast and show the reason on the admin page. The operator can switch back manually.
- The admin page offers the style (Box / Cut-out small / Cut-out full), the engine (Quality / Fast) and a status line showing the active engine and its masks per second.
- Settings persist in `page2cam.toml` (style, engine).

## Non-goals

- Blurring the page behind the person, borders, drop shadows, or any other effects.
- A green-screen or chroma-key mode.
- Methods that need a photo of the empty room first (background matting v2), or heavy still-image matting models. Both were rejected as too fragile or too slow.
- Removing the background in what the user's own webcam sends to other apps. This only affects Page2Cam's output.

## Anti-goals

- Visible stutter in the output caused by segmentation, in any style or engine.
- Needing a separate download or install (Python, CUDA, drivers) on the tester's PC.
- A cut-out failure taking the camera or stream down. If a model can't load, the app logs why, shows it on the admin page, and keeps working in Box style.

## How it fits (builder's calls unless noted)

- New crate (our call: `p2c-segment`). Its own thread takes the latest webcam frame, runs the selected model, and publishes `(frame, alpha mask)` pairs as a latest value. Frame and mask must be paired, so the mask always matches the frame it was computed from. Otherwise a moving head shows a ghost edge.
- The compositor gains a style setting and an alpha-blend path. `PipLayout` grows a `style` field (or a sibling struct). The JS in the admin UI mirrors any geometry change, as it does today.
- Geometry:
  - **Cut-out small** uses the same x/width controls as Box, with the bottom edge pinned to the output's bottom.
  - **Cut-out full** scales the webcam frame to cover the output height, centred horizontally; the operator can nudge it left or right.
  
  Exact geometry is the builder's call; the mockup shows the intent.
- Inference runtime (our call): ONNX Runtime via the `ort` crate. It is the most likely way to get DirectML on Windows. It has to cross-compile with the existing cargo-xwin setup, and its DLL ships in the zip. If `ort` can't be made to cross-compile, the fallback is a pure-Rust runtime (`tract`), which is CPU-only. In that case the plan must re-check Quality mode's speed.
- Models ship in the zip next to the exe:
  - RVM `rvm_mobilenetv3_fp32.onnx` (or fp16), with its recurrent state carried between frames and reset when the camera or engine changes;
  - MediaPipe selfie segmentation landscape ONNX (onnx-community conversion).
- **Licences:**
  - The MediaPipe selfie model is Apache-2.0.
  - **RVM is GPL-3.0.** **The user decided (2026-11-18) to accept GPL:** Page2Cam is not going to be sold or kept closed-source. Consequences:
    - the workspace licence changes from MIT to GPL-3.0-or-later;
    - the zip ships the GPL-3.0 text plus attribution for RVM and MediaPipe (Apache-2.0);
    - VCamSample's MIT notice is kept.

## Calls made without the user (from the builder check)

- **Auto-fallback trigger:** measure inference time per frame against the camera's own frame interval, not the raw mask rate. That way a slow camera doesn't trigger a fallback.
  - Manually switching back to Quality re-arms the fallback.
  - An automatic switch is never saved to `page2cam.toml`; only the operator's choice is.
- **Admin preview in full-size cut-out:** the drag box would cover the whole preview and block clicks into the page. Use a small drag handle instead.
- **Stream sync:** video may lag the mic by the inference time. If it can be measured, delay the audio by the same amount; otherwise accept the lag (under ~100 ms).
- **Linux:** the models sit next to the binary (or in the data dir). There is no Linux package yet.
- **The 1 fps webcam:** log the negotiated format and the measured fps, and prefer formats that offer 30 fps.

## Testing

- Unit tests (platform-free):
  - alpha blending: mask 0, mask 1, mask 0.5, mask scaled to frame size;
  - the geometry of each style;
  - the fallback trigger logic driven by fake timings.
- An ignored integration test runs each model on a fixed sample image (a person photo in the repo or a downloaded test asset) and checks that the mask is mostly 1 on the person's centre and mostly 0 in the corners.
- On Linux, run end to end with a webcam stand-in: a video file or the test pattern plus a still photo.
- Windows: the user tests on their PC. The admin status line and the log must show the engine, GPU or CPU, and masks per second, so the report back is self-explaining.

## Open risks

- Whether `ort` with DirectML builds under cargo-xwin from Linux is untested.
- RVM speed on the user's laptop is unknown. If fallback happens almost always, Quality is mostly theoretical on that PC.
- Webcam frame rate: the Windows log reported the integrated webcam at "1280x720@1". This may be a reporting quirk, but a 1 fps webcam would make any cut-out look bad. It should be checked and fixed as part of this work.
