//! CEF callback objects: App, Client, RenderHandler, LifeSpanHandler.
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

use cef::*;
use p2c_core::{AudioRing, Frame, FrameSlot, SAMPLE_RATE};
use parking_lot::Mutex;

wrap_app! {
    struct P2cApp {
        // `use-adapter-luid` value (browser process, GPU path only).
        adapter_luid: Option<String>,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefStringUtf16>,
            command_line: Option<&mut CommandLine>,
        ) {
            if let Some(cmd) = command_line {
                crate::add_switches(cmd, self.adapter_luid.as_deref());
            }
        }
    }
}

/// `adapter_luid`: the `use-adapter-luid` switch value, or `None`.
pub(crate) fn app(adapter_luid: Option<String>) -> App {
    P2cApp::new(adapter_luid)
}

/// Called with each shared page texture (see `crate::GpuPage`).
pub(crate) type TextureFn = Arc<dyn Fn(isize, u32, u32) -> bool + Send + Sync>;

/// The shared NT handle and size of an accelerated paint (Windows only).
#[cfg(windows)]
fn shared_texture(info: &AcceleratedPaintInfo) -> Option<(isize, u32, u32)> {
    let handle = info.shared_texture_handle as isize;
    let s = &info.extra.coded_size;
    (handle != 0).then_some((handle, s.width.max(0) as u32, s.height.max(0) as u32))
}

#[cfg(not(windows))]
fn shared_texture(_info: &AcceleratedPaintInfo) -> Option<(isize, u32, u32)> {
    None
}

wrap_render_handler! {
    struct P2cRenderHandler {
        slot: FrameSlot,
        width: u32,
        height: u32,
        // GPU path: receives `on_accelerated_paint` textures.
        on_texture: Option<TextureFn>,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            if let Some(rect) = rect {
                rect.x = 0;
                rect.y = 0;
                rect.width = self.width as _;
                rect.height = self.height as _;
            }
        }

        fn screen_info(
            &self,
            _browser: Option<&mut Browser>,
            screen_info: Option<&mut ScreenInfo>,
        ) -> ::std::os::raw::c_int {
            // Fixed 1.0 scale so the paint buffer is exactly width x height.
            match screen_info {
                Some(info) => {
                    info.device_scale_factor = 1.0;
                    1
                }
                None => 0,
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            // Popups (e.g. <select> dropdowns) are not composited yet.
            if type_ != PaintElementType::default() || buffer.is_null() || width <= 0 || height <= 0 {
                return;
            }
            let len = width as usize * height as usize * 4;
            // SAFETY: CEF guarantees `buffer` holds the full width*height BGRA view
            // for the duration of this callback, regardless of dirty rects.
            let data = unsafe { std::slice::from_raw_parts(buffer, len) }.to_vec();
            if let Ok(frame) = Frame::from_bgra(width as u32, height as u32, data) {
                self.slot.put(frame);
            }
        }

        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            _dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            // Popups are not composited (as in on_paint).
            if type_ != PaintElementType::default() {
                return;
            }
            if let (Some(f), Some((handle, w, h))) =
                (&self.on_texture, info.and_then(shared_texture))
            {
                // The handle is valid only during this callback; `f` copies it.
                f(handle, w, h);
            }
        }
    }
}

wrap_life_span_handler! {
    struct P2cLifeSpan {
        closed: Arc<AtomicBool>,
    }

    impl LifeSpanHandler {
        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }
}

wrap_display_handler! {
    struct P2cDisplay {
        url: Arc<Mutex<String>>,
    }

    impl DisplayHandler {
        fn on_address_change(&self, _browser: Option<&mut Browser>, frame: Option<&mut cef::Frame>, url: Option<&CefString>) {
            if let (Some(frame), Some(url)) = (frame, url) {
                if frame.is_main() == 1 {
                    *self.url.lock() = url.to_string();
                }
            }
        }
    }
}

wrap_audio_handler! {
    struct P2cAudio {
        ring: AudioRing,
        channels: Arc<AtomicI32>,
    }

    impl AudioHandler {
        fn audio_parameters(&self, _browser: Option<&mut Browser>, params: Option<&mut AudioParameters>) -> ::std::os::raw::c_int {
            if let Some(p) = params {
                p.channel_layout = ChannelLayout::LAYOUT_STEREO;
                p.sample_rate = SAMPLE_RATE as _;
                p.frames_per_buffer = 480;
            }
            1
        }

        fn on_audio_stream_started(&self, _browser: Option<&mut Browser>, _params: Option<&AudioParameters>, channels: ::std::os::raw::c_int) {
            self.channels.store(channels, Ordering::SeqCst);
        }

        fn on_audio_stream_packet(&self, _browser: Option<&mut Browser>, data: *mut *const f32, frames: ::std::os::raw::c_int, _pts: i64) {
            let ch = self.channels.load(Ordering::SeqCst).max(1) as usize;
            if data.is_null() || frames <= 0 {
                return;
            }
            // SAFETY: CEF passes `channels` planar buffers of `frames` samples each.
            let planes: Vec<&[f32]> = (0..ch)
                .map(|c| unsafe { std::slice::from_raw_parts(*data.add(c), frames as usize) })
                .collect();
            self.ring.push(&interleave_stereo(&planes));
        }
    }
}

/// Planar channels → interleaved stereo (mono is duplicated; extra channels dropped).
pub(crate) fn interleave_stereo(planes: &[&[f32]]) -> Vec<f32> {
    let n = planes.first().map_or(0, |p| p.len());
    let l = planes.first().copied().unwrap_or(&[]);
    let r = planes.get(1).copied().unwrap_or(l);
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        out.push(l[i]);
        out.push(r[i]);
    }
    out
}

wrap_client! {
    struct P2cClient {
        render: RenderHandler,
        life_span: LifeSpanHandler,
        display: DisplayHandler,
        audio: Option<AudioHandler>,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }

        fn audio_handler(&self) -> Option<AudioHandler> {
            self.audio.clone()
        }
    }
}

pub(crate) struct ClientParts {
    pub slot: FrameSlot,
    pub width: u32,
    pub height: u32,
    pub closed: Arc<AtomicBool>,
    pub url: Arc<Mutex<String>>,
    pub audio: Option<AudioRing>,
    pub on_texture: Option<TextureFn>,
}

pub(crate) fn client(p: ClientParts) -> Client {
    P2cClient::new(
        P2cRenderHandler::new(p.slot, p.width, p.height, p.on_texture),
        P2cLifeSpan::new(p.closed),
        P2cDisplay::new(p.url),
        p.audio
            .map(|ring| P2cAudio::new(ring, Arc::new(AtomicI32::new(2)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleave_planar() {
        let l = [1.0, 2.0];
        let r = [-1.0, -2.0];
        assert_eq!(interleave_stereo(&[&l, &r]), vec![1.0, -1.0, 2.0, -2.0]);
        assert_eq!(interleave_stereo(&[&l]), vec![1.0, 1.0, 2.0, 2.0]);
        assert!(interleave_stereo(&[]).is_empty());
    }
}
