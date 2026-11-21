//! JSON-RPC 2.0 and the MCP methods we answer.

use base64::Engine as _;
use serde_json::{json, Map, Value};

use crate::redact::redact;
use crate::tools::{parse_call, tool_defs, Backend, BackendReply};

/// The protocol version we prefer.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Versions we answer with when the client asks for them.
pub const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26"];

const INSTRUCTIONS: &str = "Page2Cam turns a web page plus the user's webcam into a virtual camera \
(Teams, Zoom, Meet) and a live stream. Call status first; start it if stopped. The person in a call \
sees what snapshot shows. Changes are visible live to the people in the user's call.";

/// Who is calling (from `initialize`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientInfo {
    pub name: String,
}

impl Default for ClientInfo {
    fn default() -> Self {
        ClientInfo {
            name: "Agent".into(),
        }
    }
}

/// The MCP server: answers one JSON-RPC message at a time.
pub struct Server<B: Backend> {
    backend: B,
}

impl<B: Backend> Server<B> {
    pub fn new(backend: B) -> Self {
        Server { backend }
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// The answer to `msg`, or `None` for notifications and responses.
    /// `client` is filled by `initialize` and used for later calls.
    pub fn handle(&self, msg: &Value, client: &mut ClientInfo) -> Option<Value> {
        let Some(obj) = msg.as_object() else {
            return Some(error(Value::Null, -32600, "invalid request"));
        };
        let id = obj.get("id").cloned();
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            // A response from the client (we send no requests) or junk.
            return match id {
                Some(id) if !obj.contains_key("result") && !obj.contains_key("error") => {
                    Some(error(id, -32600, "invalid request"))
                }
                _ => None,
            };
        };
        // Notifications (no id) get no answer.
        let id = id?;
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        Some(match method {
            "initialize" => {
                if let Some(n) = params["clientInfo"]["name"].as_str() {
                    let n: String = n.chars().filter(|c| !c.is_control()).take(64).collect();
                    if !n.trim().is_empty() {
                        client.name = n;
                    }
                }
                let asked = params["protocolVersion"].as_str().unwrap_or("");
                let version = if SUPPORTED_VERSIONS.contains(&asked) {
                    asked
                } else {
                    PROTOCOL_VERSION
                };
                result(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": { "listChanged": false } },
                        "serverInfo": { "name": "page2cam", "version": env!("CARGO_PKG_VERSION") },
                        "instructions": INSTRUCTIONS,
                    }),
                )
            }
            "ping" => result(id, json!({})),
            "tools/list" => result(id, json!({ "tools": tool_defs() })),
            "tools/call" => self.call(id, &params, client),
            _ => error(id, -32601, &format!("method not found: {method}")),
        })
    }

    fn call(&self, id: Value, params: &Value, client: &ClientInfo) -> Value {
        let Some(name) = params["name"].as_str() else {
            return error(id, -32602, "tools/call needs a tool name");
        };
        let empty = Map::new();
        let args = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(m)) => m,
            Some(_) => return result(id, tool_error("arguments must be an object")),
        };
        let call = match parse_call(name, args) {
            Ok(c) => c,
            Err(None) => return error(id, -32602, &format!("unknown tool: {name}")),
            Err(Some(msg)) => return result(id, tool_error(&msg)),
        };
        let r = match self.backend.call(&client.name, call) {
            Ok(BackendReply::Json(v)) => {
                let text = serde_json::to_string_pretty(&v).unwrap_or_default();
                json!({ "content": [{ "type": "text", "text": redact(&text) }], "isError": false })
            }
            Ok(BackendReply::Image(jpg)) => json!({
                "content": [{
                    "type": "image",
                    "data": base64::engine::general_purpose::STANDARD.encode(jpg),
                    "mimeType": "image/jpeg",
                }],
                "isError": false,
            }),
            Ok(BackendReply::Done(t)) => {
                json!({ "content": [{ "type": "text", "text": redact(&t) }], "isError": false })
            }
            Err(e) => tool_error(&e),
        };
        result(id, r)
    }
}

fn tool_error(msg: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": redact(msg) }], "isError": true })
}

fn result(id: Value, r: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": r })
}

fn error(id: Value, code: i64, msg: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": redact(msg) } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{tool_names, BackendCall, Nav};
    use std::cell::RefCell;

    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<(String, BackendCall)>>,
        reply: RefCell<Option<Result<BackendReply, String>>>,
    }

    impl Backend for Fake {
        fn call(&self, agent: &str, req: BackendCall) -> Result<BackendReply, String> {
            self.calls.borrow_mut().push((agent.into(), req));
            self.reply
                .borrow()
                .clone()
                .unwrap_or(Ok(BackendReply::Done("ok".into())))
        }
    }

    fn server() -> Server<Fake> {
        Server::new(Fake::default())
    }

    fn call(s: &Server<Fake>, name: &str, args: Value) -> Value {
        let mut c = ClientInfo::default();
        s.handle(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": name, "arguments": args } }),
            &mut c,
        )
        .unwrap()
    }

    #[test]
    fn initialize_negotiates_version() {
        let s = server();
        for (asked, want) in [
            ("2025-06-18", "2025-06-18"),
            ("2025-03-26", "2025-03-26"),
            ("2024-11-05", "2025-06-18"),
            ("", "2025-06-18"),
        ] {
            let mut c = ClientInfo::default();
            let r = s
                .handle(
                    &json!({ "jsonrpc": "2.0", "id": 7, "method": "initialize", "params": {
                        "protocolVersion": asked, "capabilities": {},
                        "clientInfo": { "name": "Claude Desktop", "version": "1" } } }),
                    &mut c,
                )
                .unwrap();
            assert_eq!(r["id"], 7);
            assert_eq!(r["result"]["protocolVersion"], want, "{asked}");
            assert!(r["result"]["capabilities"]["tools"].is_object());
            assert_eq!(r["result"]["serverInfo"]["name"], "page2cam");
            assert_eq!(c.name, "Claude Desktop");
        }
    }

    #[test]
    fn tools_list_has_all_tools_with_schemas() {
        let want = [
            "status",
            "start",
            "stop",
            "force_restart",
            "open_url",
            "navigate",
            "show_page",
            "list_pages",
            "click",
            "scroll",
            "type_text",
            "press_key",
            "snapshot",
            "get_overlay",
            "set_overlay",
            "set_cutout_quality",
            "get_crop",
            "set_crop",
            "reset_crop",
            "start_stream",
            "stop_stream",
            "set_audio",
            "list_gpus",
            "set_gpu",
            "install_update",
            "get_background",
            "set_background",
            "upload_background_image",
            "list_screens",
        ];
        let mut names = tool_names();
        names.sort();
        let mut w: Vec<String> = want.iter().map(|s| s.to_string()).collect();
        w.sort();
        assert_eq!(names, w);
        let r = server()
            .handle(
                &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }),
                &mut ClientInfo::default(),
            )
            .unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 29);
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
            assert!(
                t["description"].as_str().unwrap().len() > 10,
                "{}",
                t["name"]
            );
        }
        // The canvas is explained where pages and overlays are made.
        for n in ["show_page", "set_overlay", "snapshot"] {
            let t = tools.iter().find(|t| t["name"] == n).unwrap();
            let d = t["description"].as_str().unwrap();
            assert!(d.contains("1920x1080") && d.contains("cutout_full"), "{n}");
        }
    }

    #[test]
    fn tools_call_maps_to_backend() {
        use BackendCall as C;
        let table: Vec<(&str, Value, C)> = vec![
            ("status", json!({}), C::Status),
            ("start", json!({}), C::Start),
            ("stop", json!({}), C::Stop),
            ("force_restart", json!({}), C::ForceRestart),
            (
                "open_url",
                json!({ "url": " https://example.com " }),
                C::OpenUrl("https://example.com".into()),
            ),
            (
                "navigate",
                json!({ "action": "back" }),
                C::Navigate(Nav::Back),
            ),
            (
                "show_page",
                json!({ "name": "agenda", "html": "<p>x</p>" }),
                C::ShowPage {
                    name: "agenda".into(),
                    html: "<p>x</p>".into(),
                },
            ),
            ("list_pages", json!({}), C::ListPages),
            (
                "click",
                json!({ "x": 0.5, "y": 0.25 }),
                C::Input(json!({ "kind": "click", "x": 0.5, "y": 0.25 })),
            ),
            (
                "scroll",
                json!({ "x": 0.5, "y": 0.5, "dy": 300 }),
                C::Input(json!({ "kind": "scroll", "x": 0.5, "y": 0.5, "dx": 0.0, "dy": 300.0 })),
            ),
            (
                "type_text",
                json!({ "text": "hi" }),
                C::Input(json!({ "kind": "text", "text": "hi" })),
            ),
            (
                "press_key",
                json!({ "key": "enter" }),
                C::Input(json!({ "kind": "key", "key": "enter" })),
            ),
            ("snapshot", json!({}), C::Snapshot),
            ("get_overlay", json!({}), C::GetOverlay),
            (
                "set_overlay",
                json!({ "x": 1, "style": "cutout_full" }),
                C::SetOverlay(json!({ "x": 1.0, "style": "cutout-full" })),
            ),
            (
                "set_cutout_quality",
                json!({ "quality": "fast" }),
                C::SetCutoutQuality("fast".into()),
            ),
            ("get_crop", json!({}), C::GetCrop),
            (
                "set_crop",
                json!({ "x": 0.1, "y": 0, "w": 0.8, "h": 1 }),
                C::SetCrop(json!({ "x": 0.1, "y": 0.0, "w": 0.8, "h": 1.0 })),
            ),
            ("reset_crop", json!({}), C::ResetCrop),
            ("start_stream", json!({}), C::StartStream),
            ("stop_stream", json!({}), C::StopStream),
            (
                "set_audio",
                json!({ "mic_on": true, "page_gain": 0.5 }),
                C::SetAudio(json!({ "mic_on": true, "page_gain": 0.5 })),
            ),
            ("list_gpus", json!({}), C::ListGpus),
            (
                "set_gpu",
                json!({ "choice": "auto" }),
                C::SetGpu("auto".into()),
            ),
            ("install_update", json!({}), C::InstallUpdate),
            ("get_background", json!({}), C::GetBackground),
            (
                "set_background",
                json!({ "full": "youtube", "youtube": "https://youtu.be/dQw4w9WgXcQ" }),
                C::SetBackground(
                    json!({ "full": "youtube", "full_youtube": "https://youtu.be/dQw4w9WgXcQ" }),
                ),
            ),
            (
                "set_background",
                json!({ "full": "screen", "screen": " DISPLAY2 " }),
                C::SetBackground(json!({ "full": "screen", "full_screen": "DISPLAY2" })),
            ),
            ("list_screens", json!({}), C::ListScreens),
            (
                "set_background",
                json!({ "in_box": "colour", "box_colour": "#AABBCC", "refined_edges": true }),
                C::SetBackground(
                    json!({ "in_box": "colour", "box_colour": "#aabbcc", "refined_edges": true }),
                ),
            ),
            (
                "upload_background_image",
                json!({ "row": "box", "base64": "iVBORw0KGgo=" }),
                C::UploadBackgroundImage {
                    row: "box".into(),
                    bytes: b"\x89PNG\r\n\x1a\n".to_vec(),
                },
            ),
        ];
        for (name, args, want) in table {
            let s = server();
            let r = call(&s, name, args);
            assert_eq!(r["result"]["isError"], false, "{name}: {r}");
            let calls = s.backend().calls.borrow();
            assert_eq!(calls.len(), 1, "{name}");
            assert_eq!(calls[0].1, want, "{name}");
            assert_eq!(calls[0].0, "Agent");
        }
    }

    #[test]
    fn upload_reads_a_local_file() {
        let dir = std::env::temp_dir().join(format!("p2c-mcp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("pic.png");
        std::fs::write(&f, b"\x89PNG\r\n\x1a\nrest").unwrap();
        let s = server();
        let r = call(
            &s,
            "upload_background_image",
            json!({ "row": "full", "path": f.to_str().unwrap() }),
        );
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert_eq!(
            s.backend().calls.borrow()[0].1,
            BackendCall::UploadBackgroundImage {
                row: "full".into(),
                bytes: b"\x89PNG\r\n\x1a\nrest".to_vec()
            }
        );
        let r = call(
            &s,
            "upload_background_image",
            json!({ "row": "full", "path": dir.to_str().unwrap() }),
        );
        assert_eq!(r["result"]["isError"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_arguments_are_tool_errors() {
        let big = "x".repeat(crate::MAX_HTML_BYTES + 1);
        let cases: Vec<(&str, Value)> = vec![
            ("click", json!({ "x": 5, "y": 0.5 })),
            ("click", json!({ "x": "a", "y": 0.5 })),
            ("click", json!({ "y": 0.5 })),
            ("click", json!({ "x": 0.5, "y": 0.5, "z": 1 })),
            ("show_page", json!({ "name": "a" })),
            ("show_page", json!({ "name": "a", "html": big })),
            ("show_page", json!({ "name": " ", "html": "<p>" })),
            ("set_overlay", json!({ "style": "circle" })),
            ("set_overlay", json!({})),
            ("set_overlay", json!({ "width": 0.9 })),
            ("navigate", json!({ "action": "up" })),
            ("set_cutout_quality", json!({ "quality": "best" })),
            ("set_crop", json!({ "x": 0, "y": 0, "w": 2, "h": 1 })),
            ("set_audio", json!({ "mic_gain": -1 })),
            ("type_text", json!({ "text": "" })),
            ("open_url", json!({ "url": "" })),
            ("status", json!({ "verbose": true })),
            ("status", json!([1])),
        ];
        for (name, args) in cases {
            let s = server();
            let r = call(&s, name, args.clone());
            assert_eq!(r["result"]["isError"], true, "{name} {args}: {r}");
            assert!(s.backend().calls.borrow().is_empty(), "{name} {args}");
        }
    }

    #[test]
    fn network_shares_are_refused() {
        // A share would make Windows send the user's credentials (NTLM) to
        // whatever host the agent names.
        let refused: Vec<(&str, Value)> = vec![
            (
                "upload_background_image",
                json!({ "row": "full", "path": "\\\\evil\\share\\a.png" }),
            ),
            (
                "upload_background_image",
                json!({ "row": "full", "path": "//evil/share/a.png" }),
            ),
            (
                "upload_background_image",
                json!({ "row": "full", "path": "\\\\?\\UNC\\evil\\s\\a.png" }),
            ),
            (
                "upload_background_image",
                json!({ "row": "full", "path": "relative\\a.png" }),
            ),
            ("open_url", json!({ "url": "file://evil/share/x.html" })),
            ("open_url", json!({ "url": "FILE:////evil/share/x.html" })),
            ("open_url", json!({ "url": "file:\\\\evil\\share\\x.html" })),
            ("open_url", json!({ "url": "file:/\\evil\\share" })),
        ];
        for (name, args) in refused {
            let s = server();
            let t = std::time::Instant::now();
            let r = call(&s, name, args.clone());
            assert_eq!(r["result"]["isError"], true, "{args}: {r}");
            // Refused by the rule, not by a failed attempt to reach it.
            let text = r["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("local"), "{args}: {text}");
            assert!(
                t.elapsed() < std::time::Duration::from_millis(200),
                "{args}: tried the network"
            );
            assert!(s.backend().calls.borrow().is_empty(), "{args}");
        }
        for url in [
            "file:///C:/Users/u/Documents/a.html",
            "file://localhost/C:/a.html",
            "https://example.com/",
        ] {
            let s = server();
            let r = call(&s, "open_url", json!({ "url": url }));
            assert_eq!(r["result"]["isError"], false, "{url}: {r}");
        }
    }

    #[test]
    fn background_tools_validate() {
        let cases: Vec<(&str, Value)> = vec![
            ("set_background", json!({ "full": "video" })),
            ("set_background", json!({ "in_box": "page" })),
            ("set_background", json!({ "full_colour": "red" })),
            ("set_background", json!({ "box_colour": "#12345g" })),
            ("set_background", json!({ "youtube": "" })),
            ("set_background", json!({ "refined_edges": "yes" })),
            ("set_background", json!({ "screen": "x".repeat(65) })),
            ("set_background", json!({ "screen": 2 })),
            ("list_screens", json!({ "id": "DISPLAY1" })),
            ("set_background", json!({})),
            (
                "upload_background_image",
                json!({ "row": "side", "base64": "iVBORw0KGgo=" }),
            ),
            (
                "upload_background_image",
                json!({ "row": "full", "base64": "!!!" }),
            ),
            ("upload_background_image", json!({ "row": "full" })),
            (
                "upload_background_image",
                json!({ "row": "full", "base64": "iVBORw0KGgo=", "path": "C:/x.png" }),
            ),
            (
                "upload_background_image",
                json!({ "row": "full", "path": "Z:/does/not/exist.png" }),
            ),
            (
                "upload_background_image",
                json!({ "row": "full", "base64": "A".repeat(28 * 1024 * 1024) }),
            ),
        ];
        for (name, args) in cases {
            let s = server();
            let r = call(&s, name, args.clone());
            assert_eq!(r["result"]["isError"], true, "{name}: {r}");
            assert!(s.backend().calls.borrow().is_empty(), "{name}");
        }
    }

    #[test]
    fn snapshot_returns_image_content() {
        let s = server();
        *s.backend().reply.borrow_mut() =
            Some(Ok(BackendReply::Image(vec![0xff, 0xd8, 0xff, 0xe0])));
        let r = call(&s, "snapshot", json!({}));
        let c = &r["result"]["content"][0];
        assert_eq!(c["type"], "image");
        assert_eq!(c["mimeType"], "image/jpeg");
        assert_eq!(c["data"], "/9j/4A==");
    }

    #[test]
    fn no_tool_output_contains_secrets() {
        let secrets = [
            Ok(BackendReply::Json(
                json!({ "stream_url": "rtmp://a.rtmp.youtube.com/live2/abcd-1234",
                "log": "GET /?t=SECRET", "token": "SECRET" }),
            )),
            Ok(BackendReply::Done(
                "Authorization: Bearer SECRET; rtmp://a.rtmp.youtube.com/live2/abcd-1234".into(),
            )),
            Err(
                "ffmpeg: rtmp://a.rtmp.youtube.com/live2/abcd-1234: I/O error, url ?t=SECRET"
                    .into(),
            ),
        ];
        for reply in secrets {
            for tool in tool_names() {
                let s = server();
                *s.backend().reply.borrow_mut() = Some(reply.clone());
                let args = match tool.as_str() {
                    "open_url" => json!({ "url": "https://x.example/?t=SECRET" }),
                    "navigate" => json!({ "action": "reload" }),
                    "show_page" => json!({ "name": "p", "html": "<p>" }),
                    "click" => json!({ "x": 0, "y": 0 }),
                    "scroll" => json!({ "x": 0, "y": 0, "dy": 1 }),
                    "type_text" => json!({ "text": "a" }),
                    "press_key" => json!({ "key": "a" }),
                    "set_overlay" => json!({ "visible": true }),
                    "set_cutout_quality" => json!({ "quality": "fast" }),
                    "set_crop" => json!({ "x": 0, "y": 0, "w": 1, "h": 1 }),
                    "set_audio" => json!({ "mic_on": true }),
                    "set_gpu" => json!({ "choice": "auto" }),
                    "set_background" => json!({ "full": "page" }),
                    "upload_background_image" => json!({ "row": "full", "base64": "iVBORw0KGgo=" }),
                    _ => json!({}),
                };
                let out = call(&s, &tool, args).to_string();
                assert!(
                    !out.contains("abcd-1234") && !out.contains("SECRET"),
                    "{tool}: {out}"
                );
            }
        }
    }

    #[test]
    fn notification_has_no_reply() {
        let s = server();
        let mut c = ClientInfo::default();
        assert!(s
            .handle(
                &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                &mut c
            )
            .is_none());
        assert!(s
            .handle(&json!({ "jsonrpc": "2.0", "id": 3, "result": {} }), &mut c)
            .is_none());
    }

    #[test]
    fn unknown_method_is_jsonrpc_error_32601() {
        let s = server();
        let mut c = ClientInfo::default();
        let r = s
            .handle(
                &json!({ "jsonrpc": "2.0", "id": "a", "method": "resources/list" }),
                &mut c,
            )
            .unwrap();
        assert_eq!(r["error"]["code"], -32601);
        assert_eq!(r["id"], "a");
        let r = call(&s, "no_such_tool", json!({}));
        assert_eq!(r["error"]["code"], -32602);
        let r = s.handle(&json!([1, 2]), &mut c).unwrap();
        assert_eq!(r["error"]["code"], -32600);
        let r = s
            .handle(
                &json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
                &mut c,
            )
            .unwrap();
        assert_eq!(r["result"], json!({}));
    }
}
