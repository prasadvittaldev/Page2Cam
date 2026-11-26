//! JSON-RPC 2.0 and the MCP methods we answer.

use base64::Engine as _;
use serde_json::{json, Map, Value};

use crate::redact::redact;
use crate::tools::{parse_call, tool_defs, Backend, BackendCall, BackendReply};

/// The protocol version we prefer.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Versions we answer with when the client asks for them.
pub const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26"];

/// The `initialize` capabilities.
fn capabilities() -> Value {
    json!({
        "tools": { "listChanged": false },
        "resources": { "listChanged": false },
        "prompts": { "listChanged": false },
    })
}

/// Answers that need neither Page2Cam nor the tray: the guide's resources
/// and prompts, and the `guide` tool. `None` for anything else (and for
/// notifications). The stdio bridge uses it while the tray can't be reached.
pub fn offline(msg: &Value) -> Option<Value> {
    let id = msg.get("id")?.clone();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    Some(match msg["method"].as_str()? {
        "resources/list" => {
            let list: Vec<Value> = p2c_guide::resources()
                .into_iter()
                .map(|r| {
                    json!({ "uri": r.uri, "name": r.name, "title": r.title,
                    "description": r.description, "mimeType": "text/markdown" })
                })
                .collect();
            result(id, json!({ "resources": list }))
        }
        "resources/templates/list" => result(id, json!({ "resourceTemplates": [] })),
        "resources/read" => {
            let uri = params["uri"].as_str().unwrap_or("");
            match p2c_guide::read_resource(uri) {
                Some(text) => result(
                    id,
                    json!({ "contents": [{ "uri": uri, "mimeType": "text/markdown", "text": text }] }),
                ),
                None => error(id, -32002, &format!("resource not found: {uri}")),
            }
        }
        "prompts/list" => result(id, json!({ "prompts": p2c_guide::prompts() })),
        "prompts/get" => {
            let name = params["name"].as_str().unwrap_or("");
            let args = params["arguments"].as_object().cloned().unwrap_or_default();
            match p2c_guide::prompt(name, &args) {
                Some((description, text)) => result(
                    id,
                    json!({ "description": description,
                        "messages": [{ "role": "user", "content": { "type": "text", "text": text } }] }),
                ),
                None => error(id, -32602, &format!("unknown prompt: {name}")),
            }
        }
        "tools/call" if params["name"] == "guide" => {
            let empty = Map::new();
            let args = params["arguments"].as_object().unwrap_or(&empty);
            match parse_call("guide", args) {
                Ok(BackendCall::Guide(t)) => result(id, guide_answer(t.as_deref())),
                Err(Some(m)) => result(id, tool_error(&m)),
                _ => return None,
            }
        }
        _ => return None,
    })
}

fn guide_answer(topic: Option<&str>) -> Value {
    match p2c_guide::topic(topic.unwrap_or("")) {
        Ok((_, text)) => json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
        Err(e) => tool_error(&e),
    }
}

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
                        "capabilities": capabilities(),
                        "serverInfo": { "name": "page2cam", "version": env!("CARGO_PKG_VERSION") },
                        "instructions": p2c_guide::instructions(),
                    }),
                )
            }
            "ping" => result(id, json!({})),
            "tools/list" => result(id, json!({ "tools": tool_defs() })),
            "tools/call" if params["name"] != "guide" => self.call(id, &params, client),
            "tools/call"
            | "resources/list"
            | "resources/templates/list"
            | "resources/read"
            | "prompts/list"
            | "prompts/get" => {
                return offline(msg).or_else(|| Some(error(id, -32603, "internal error")))
            }
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
            "set_stream_sites",
            "set_audio",
            "list_gpus",
            "set_gpu",
            "install_update",
            "get_background",
            "set_background",
            "upload_background_image",
            "list_screens",
            "list_layers",
            "add_layer",
            "update_layer",
            "show_layer",
            "hide_layer",
            "remove_layer",
            "clear_layers",
            "reorder_layers",
            "guide",
            "list_templates",
            "get_template",
            "save_template",
            "delete_template",
            "list_scenes",
            "save_scene",
            "load_scene",
            "delete_scene",
        ];
        assert_eq!(want.len(), 47);
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
        assert_eq!(tools.len(), 47);
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
            ("start_stream", json!({}), C::StartStream { sites: None }),
            ("stop_stream", json!({}), C::StopStream { sites: None }),
            (
                "stop_stream",
                json!({ "sites": ["youtube"] }),
                C::StopStream {
                    sites: Some(vec!["youtube".into()]),
                },
            ),
            (
                "set_stream_sites",
                json!({ "twitch": false }),
                C::SetStreamSites {
                    youtube: None,
                    twitch: Some(false),
                },
            ),
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
    fn start_stream_takes_sites() {
        let s = server();
        let r = call(&s, "start_stream", json!({ "sites": ["twitch"] }));
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert_eq!(
            s.backend().calls.borrow()[0].1,
            BackendCall::StartStream {
                sites: Some(vec!["twitch".into()])
            }
        );
        let s = server();
        let r = call(&s, "start_stream", json!({ "sites": ["kick"] }));
        assert_eq!(r["result"]["isError"], true, "{r}");
        let text = r["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("youtube") && text.contains("twitch"),
            "{text}"
        );
        assert!(s.backend().calls.borrow().is_empty());
        for args in [json!({ "sites": [] }), json!({ "sites": "twitch" })] {
            let r = call(&s, "start_stream", args.clone());
            assert_eq!(r["result"]["isError"], true, "{args}");
        }
    }

    #[test]
    fn set_stream_sites_needs_one_field() {
        for args in [
            json!({}),
            json!({ "kick": true }),
            json!({ "twitch": "yes" }),
        ] {
            let s = server();
            let r = call(&s, "set_stream_sites", args.clone());
            assert_eq!(r["result"]["isError"], true, "{args}: {r}");
            assert!(s.backend().calls.borrow().is_empty());
        }
        let s = server();
        let r = call(
            &s,
            "set_stream_sites",
            json!({ "youtube": true, "twitch": false }),
        );
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert_eq!(
            s.backend().calls.borrow()[0].1,
            BackendCall::SetStreamSites {
                youtube: Some(true),
                twitch: Some(false)
            }
        );
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
    fn layer_tools_validate() {
        let cases: Vec<(&str, Value)> = vec![
            ("add_layer", json!({ "text": "x" })),
            ("add_layer", json!({ "kind": "clock" })),
            (
                "add_layer",
                json!({ "kind": "title", "text": "x", "x": 1.5 }),
            ),
            ("add_layer", json!({ "kind": "title", "text": "x", "w": 0 })),
            (
                "add_layer",
                json!({ "kind": "title", "text": "x", "z": 0.5 }),
            ),
            (
                "add_layer",
                json!({ "kind": "title", "text": "x", "style": "fancy" }),
            ),
            (
                "add_layer",
                json!({ "kind": "title", "text": "x", "enter": "spin" }),
            ),
            (
                "add_layer",
                json!({ "kind": "title", "text": "x", "colour": "red" }),
            ),
            (
                "add_layer",
                json!({ "kind": "title", "text": "x".repeat(501) }),
            ),
            (
                "add_layer",
                json!({ "kind": "title", "text": "x", "hide_after_s": 0 }),
            ),
            (
                "add_layer",
                json!({ "kind": "title", "text": "x", "font": "Arial" }),
            ),
            ("add_layer", json!({ "kind": "ticker", "items": [] })),
            ("add_layer", json!({ "kind": "ticker", "items": [1, 2] })),
            (
                "add_layer",
                json!({ "kind": "html", "html": "x".repeat(256 * 1024 + 1) }),
            ),
            ("add_layer", json!({ "kind": "html", "html": " " })),
            (
                "add_layer",
                json!({ "kind": "image", "path": "\\\\evil\\s\\a.png" }),
            ),
            (
                "add_layer",
                json!({ "kind": "image", "path": "C:/a.png", "data_base64": "AAAA" }),
            ),
            (
                "add_layer",
                json!({ "kind": "image", "data_base64": "!!!" }),
            ),
            (
                "add_layer",
                json!({ "kind": "video", "url": "file:///C:/a.mp4" }),
            ),
            ("add_layer", json!({ "kind": "youtube", "link": " " })),
            ("update_layer", json!({ "id": "t" })),
            ("update_layer", json!({ "x": 0.1 })),
            ("update_layer", json!({ "id": "t", "kind": "ticker" })),
            ("show_layer", json!({})),
            ("hide_layer", json!({ "id": "" })),
            ("show_layer", json!({ "id": "../quit" })),
            ("remove_layer", json!({ "id": "t", "x": 1 })),
            ("reorder_layers", json!({ "ids": [] })),
            ("reorder_layers", json!({ "ids": "a,b" })),
            ("clear_layers", json!({ "all": true })),
        ];
        for (name, args) in cases {
            let s = server();
            let r = call(&s, name, args.clone());
            assert_eq!(r["result"]["isError"], true, "{name} {args}: {r}");
            assert!(s.backend().calls.borrow().is_empty(), "{name} {args}");
        }
    }

    #[test]
    fn layer_tools_build_engine_calls() {
        let s = server();
        let r = call(
            &s,
            "add_layer",
            json!({ "kind": "title", "text": "Guest", "subtitle": "CTO", "style": "brand",
                "colour": "#FFFFFF", "enter": "slide-left", "hide_after_s": 10 }),
        );
        assert_eq!(r["result"]["isError"], false, "{r}");
        let r = call(
            &s,
            "update_layer",
            json!({ "id": "title-1", "x": 0.1, "colour": null, "hide_after_s": null }),
        );
        assert_eq!(r["result"]["isError"], false, "{r}");
        call(&s, "show_layer", json!({ "id": "title-1" }));
        call(&s, "reorder_layers", json!({ "ids": ["a", "title-1"] }));
        call(&s, "list_layers", json!({}));
        let calls = s.backend().calls.borrow();
        let (agent_calls, reqs): (Vec<_>, Vec<_>) = calls.iter().cloned().unzip();
        assert_eq!(agent_calls.len(), 5);
        match &reqs[0] {
            BackendCall::AddLayer(v) => {
                assert_eq!(v["kind"], "title");
                assert_eq!(v["colour"], "#ffffff");
                assert_eq!(v["enter"], "slide-left");
                assert_eq!(v["hide_after_s"], 10);
                assert!(reqs[0].changes());
                assert_eq!(reqs[0].summary(), "add title layer");
            }
            c => panic!("{c:?}"),
        }
        match &reqs[1] {
            BackendCall::UpdateLayer { id, patch } => {
                assert_eq!(id, "title-1");
                assert_eq!(
                    patch,
                    &json!({ "x": 0.1, "colour": null, "hide_after_s": null })
                );
            }
            c => panic!("{c:?}"),
        }
        assert_eq!(reqs[2], BackendCall::ShowLayer("title-1".into()));
        assert_eq!(
            reqs[3],
            BackendCall::ReorderLayers(vec!["a".into(), "title-1".into()])
        );
        assert!(!reqs[4].changes());
    }

    #[test]
    fn guide_is_answered_without_the_backend() {
        let s = server();
        let r = call(&s, "guide", json!({}));
        assert_eq!(r["result"]["isError"], false);
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Playbooks"));
        let r = call(&s, "guide", json!({ "topic": "lower-third" }));
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("add_layer"));
        let r = call(&s, "guide", json!({ "topic": "zzqxv" }));
        assert_eq!(r["result"]["isError"], true);
        let r = call(&s, "guide", json!({ "subject": "x" }));
        assert_eq!(r["result"]["isError"], true);
        assert!(s.backend().calls.borrow().is_empty());
        // Offline (the bridge while the tray can't be reached).
        let r = offline(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "guide", "arguments": { "topic": "scenes" } } }))
        .unwrap();
        assert!(r["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("save_scene"));
        assert!(
            offline(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "status" } }))
            .is_none()
        );
    }

    #[test]
    fn resources_and_prompts() {
        let s = server();
        let mut c = ClientInfo::default();
        let rq = |m: &str, p: Value| json!({ "jsonrpc": "2.0", "id": 9, "method": m, "params": p });
        let init = s.handle(&rq("initialize", json!({})), &mut c).unwrap();
        let caps = &init["result"]["capabilities"];
        assert!(caps["resources"].is_object() && caps["prompts"].is_object());
        let instr = init["result"]["instructions"].as_str().unwrap();
        assert!(instr.contains("guide") && instr.lines().count() <= 60);
        let r = s.handle(&rq("resources/list", json!({})), &mut c).unwrap();
        let list = r["result"]["resources"].as_array().unwrap();
        assert!(list
            .iter()
            .any(|x| x["uri"] == "page2cam://playbooks/webinar"));
        let r = s
            .handle(
                &rq(
                    "resources/read",
                    json!({ "uri": "page2cam://templates/countdown" }),
                ),
                &mut c,
            )
            .unwrap();
        assert!(r["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Countdown"));
        let r = s
            .handle(
                &rq("resources/read", json!({ "uri": "file:///C:/x" })),
                &mut c,
            )
            .unwrap();
        assert_eq!(r["error"]["code"], -32002);
        let r = s.handle(&rq("prompts/list", json!({})), &mut c).unwrap();
        assert!(r["result"]["prompts"].as_array().unwrap().len() >= 26);
        let r = s
            .handle(
                &rq(
                    "prompts/get",
                    json!({ "name": "webinar", "arguments": { "topic": "Q4 plans" } }),
                ),
                &mut c,
            )
            .unwrap();
        let text = r["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();
        assert!(text.contains("Q4 plans") && text.contains("guide"));
        let r = s
            .handle(&rq("prompts/get", json!({ "name": "nope" })), &mut c)
            .unwrap();
        assert_eq!(r["error"]["code"], -32602);
        let r = s
            .handle(&rq("resources/templates/list", json!({})), &mut c)
            .unwrap();
        assert_eq!(r["result"]["resourceTemplates"], json!([]));
    }

    #[test]
    fn template_and_scene_tools() {
        use BackendCall as C;
        let table: Vec<(&str, Value, C)> = vec![
            ("list_templates", json!({}), C::ListTemplates(None)),
            (
                "list_templates",
                json!({ "category": "time" }),
                C::ListTemplates(Some("time".into())),
            ),
            (
                "get_template",
                json!({ "id": "countdown", "include_html": true }),
                C::GetTemplate {
                    id: "countdown".into(),
                    include_html: true,
                },
            ),
            (
                "delete_template",
                json!({ "id": "mine" }),
                C::DeleteTemplate("mine".into()),
            ),
            (
                "save_template",
                json!({ "from_layer": "chart", "name": "Chart" }),
                C::SaveTemplate(json!({ "from_layer": "chart", "name": "Chart" })),
            ),
            ("list_scenes", json!({}), C::ListScenes),
            (
                "save_scene",
                json!({ "name": " Intro ", "include_webcam": true }),
                C::SaveScene {
                    name: "Intro".into(),
                    include_background: false,
                    include_webcam: true,
                },
            ),
            (
                "load_scene",
                json!({ "id": "intro" }),
                C::LoadScene {
                    id: "intro".into(),
                    replace: true,
                },
            ),
            (
                "load_scene",
                json!({ "id": "intro", "mode": "add" }),
                C::LoadScene {
                    id: "intro".into(),
                    replace: false,
                },
            ),
            (
                "delete_scene",
                json!({ "id": "intro" }),
                C::DeleteScene("intro".into()),
            ),
        ];
        for (name, args, want) in table {
            let s = server();
            let r = call(&s, name, args);
            assert_eq!(r["result"]["isError"], false, "{name}: {r}");
            assert_eq!(s.backend().calls.borrow()[0].1, want, "{name}");
        }
        for (name, args) in [
            ("save_template", json!({})),
            ("save_template", json!({ "id": "x", "name": "X" })),
            (
                "save_template",
                json!({ "from_layer": "a", "copy_of": "b" }),
            ),
            ("save_template", json!({ "id": "../x", "copy_of": "b" })),
            (
                "save_template",
                json!({ "id": "x", "name": "X", "html": "<b>", "evil": 1 }),
            ),
            ("load_scene", json!({ "id": "a", "mode": "mix" })),
            ("save_scene", json!({ "name": "" })),
            ("get_template", json!({ "id": "a/b" })),
            ("list_templates", json!({ "category": "../x" })),
            (
                "add_layer",
                json!({ "kind": "template", "template": "a b" }),
            ),
            (
                "add_layer",
                json!({ "kind": "template", "template": "x", "params": [1] }),
            ),
            (
                "add_layer",
                json!({ "kind": "template", "template": "x", "params": { "logo": { "url": "x" } } }),
            ),
            (
                "add_layer",
                json!({ "kind": "template", "template": "x", "params": { "logo": { "path": "\\\\evil\\s\\a.png" } } }),
            ),
        ] {
            let s = server();
            let r = call(&s, name, args.clone());
            assert_eq!(r["result"]["isError"], true, "{name} {args}: {r}");
            assert!(s.backend().calls.borrow().is_empty());
        }
        // A picture path in params is embedded.
        let dir = std::env::temp_dir().join(format!("p2c-mcp-tpl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("logo.png");
        std::fs::write(&f, b"\x89PNG\r\n\x1a\nrest").unwrap();
        let s = server();
        let r = call(
            &s,
            "add_layer",
            json!({ "kind": "template", "template": "lower-third",
                "params": { "title": "A", "logo": { "path": f.to_str().unwrap() } } }),
        );
        assert_eq!(r["result"]["isError"], false, "{r}");
        match &s.backend().calls.borrow()[0].1 {
            C::AddLayer(v) => {
                assert!(v["params"]["logo"]
                    .as_str()
                    .unwrap()
                    .starts_with("data:image/png;base64,"));
                assert_eq!(v["template"], "lower-third");
            }
            c => panic!("{c:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
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
                &json!({ "jsonrpc": "2.0", "id": "a", "method": "sampling/createMessage" }),
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
