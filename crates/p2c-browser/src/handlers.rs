//! CEF callback objects: App, Client, RenderHandler, LifeSpanHandler.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cef::*;
use p2c_core::{Frame, FrameSlot};

wrap_app! {
    struct P2cApp {}

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefStringUtf16>,
            command_line: Option<&mut CommandLine>,
        ) {
            if let Some(cmd) = command_line {
                crate::add_switches(cmd);
            }
        }
    }
}

pub(crate) fn app() -> App {
    P2cApp::new()
}

wrap_render_handler! {
    struct P2cRenderHandler {
        slot: FrameSlot,
        width: u32,
        height: u32,
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

wrap_client! {
    struct P2cClient {
        render: RenderHandler,
        life_span: LifeSpanHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }
    }
}

pub(crate) fn client(slot: FrameSlot, width: u32, height: u32, closed: Arc<AtomicBool>) -> Client {
    P2cClient::new(
        P2cRenderHandler::new(slot, width, height),
        P2cLifeSpan::new(closed),
    )
}
