# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

Design stage, no code yet. The full design is in `docs/superpowers/specs/2026-11-17-page2cam-design.md`; read it before making changes. Add build, test and lint commands here once the Cargo workspace exists.

## What Page2Cam is

A Rust desktop app that renders a live, interactive web page offscreen and outputs it, with the user's real webcam as a movable picture-in-picture overlay, to:
- a **virtual camera** (picked up by Teams, Zoom, Meet and similar apps), and/or
- an **RTMP stream** (YouTube Live), with page and/or mic audio.

The operator controls it through a local admin web UI on the same machine: navigation, live click/scroll/type into the page, PIP placement, audio mix, start/stop outputs.

## Hard constraints (decided by the user)

- Targets **Windows 11 and Linux only**. No macOS, no Windows 10.
- **No OBS dependency.** The app provides its own virtual camera:
  - Linux: `v4l2loopback` device.
  - Windows 11: `MFCreateVirtualCamera`. The media source is a COM DLL running inside the Windows Frame Server process, so frames cross a process boundary through shared memory.
- Page audio goes to the stream only, not into call apps.

## Architecture

Pipeline: CEF offscreen browser (frames + PCM) and webcam → compositor (fixed fps, latest-frame slots, not queues) → virtual cam sink + ffmpeg sidecar (RTMP). Mic + page audio → mixer → ffmpeg. An axum admin server on `127.0.0.1` provides REST settings, WebSocket preview frames and input forwarding.

Security invariants: bind to localhost only; every admin request needs the per-launch random token; the stream key lives in the OS keyring and never appears in logs or the UI.
