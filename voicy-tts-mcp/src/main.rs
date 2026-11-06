use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::os::unix::net::UnixStream;

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

fn strip_markdown(text: &str) -> String {
    let re_code = Regex::new(r"(?s)```.*?```").unwrap();
    let text = re_code.replace_all(text, "");

    let re_inline_code = Regex::new(r"`([^`]+)`").unwrap();
    let text = re_inline_code.replace_all(&text, "$1");

    let re_html = Regex::new(r"<[^>]+>").unwrap();
    let text = re_html.replace_all(&text, "");

    let re_img = Regex::new(r"!\[([^\]]*)\]\([^\)]+\)").unwrap();
    let text = re_img.replace_all(&text, "");

    let re_link = Regex::new(r"\[([^\]]+)\]\([^\)]+\)").unwrap();
    let text = re_link.replace_all(&text, "$1");

    let re_latex_arrow = Regex::new(r"\$\\(?:left)?rightarrow\$").unwrap();
    let text = re_latex_arrow.replace_all(&text, "to");

    let re_latex = Regex::new(r"\$[^$]+\$").unwrap();
    let text = re_latex.replace_all(&text, "");

    let re_heading = Regex::new(r"(?m)^#{1,6}\s*").unwrap();
    let text = re_heading.replace_all(&text, "");

    let re_hr = Regex::new(r"(?m)^[-*_]{3,}\s*$").unwrap();
    let text = re_hr.replace_all(&text, "");

    let re_quote = Regex::new(r"(?m)^>\s*").unwrap();
    let text = re_quote.replace_all(&text, "");

    let re_table_border = Regex::new(r"(?m)^\|?\s*[-:]+[-| :]*\|?\s*$").unwrap();
    let text = re_table_border.replace_all(&text, "");

    let re_pipe = Regex::new(r"\|").unwrap();
    let text = re_pipe.replace_all(&text, " ");

    let re_bullet = Regex::new(r"(?m)^\s*[-*+]\s*(\[[ xX]\])?\s*").unwrap();
    let text = re_bullet.replace_all(&text, "");

    let re_num = Regex::new(r"(?m)^\s*\d+\.\s+").unwrap();
    let text = re_num.replace_all(&text, "");

    let re_bold1 = Regex::new(r"\*\*(.*?)\*\*").unwrap();
    let text = re_bold1.replace_all(&text, "$1");

    let re_bold2 = Regex::new(r"__(.*?)__").unwrap();
    let text = re_bold2.replace_all(&text, "$1");

    let re_italic1 = Regex::new(r"\*(.*?)\*").unwrap();
    let text = re_italic1.replace_all(&text, "$1");

    let re_italic2 = Regex::new(r"_(.*?)_").unwrap();
    let text = re_italic2.replace_all(&text, "$1");

    let re_tree = Regex::new(r"[├└│─┌┐┘└┬┴┼═║╒╓╔╕╖╗╘╙╚╛╜╝╞╟╠╡╢╣╤╥╦╧╨╩╪╫╬]+").unwrap();
    let text = re_tree.replace_all(&text, "");

    let re_colon = Regex::new(r":\s+").unwrap();
    let text = re_colon.replace_all(&text, ". ");

    let mut cleaned = text.trim().to_string();
    if !cleaned.is_empty() && !cleaned.ends_with(['.', '!', '?']) {
        cleaned.push('.');
    }
    cleaned
}

fn get_voicy_socket() -> String {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        format!("{}/voicy.sock", runtime_dir)
    } else {
        let uid = unsafe { libc::getuid() };
        format!("/run/user/{}/voicy.sock", uid)
    }
}

fn send_daemon_cmd(cmd: &str) -> Result<String, String> {
    let sock_path = get_voicy_socket();
    let mut stream = UnixStream::connect(&sock_path)
        .map_err(|e| format!("Could not connect to Voicy daemon at '{}': {}", sock_path, e))?;
    stream
        .write_all(format!("{}\n", cmd).as_bytes())
        .map_err(|e| format!("Failed to send command to daemon: {}", e))?;
    stream
        .flush()
        .map_err(|e| format!("Failed to flush stream: {}", e))?;

    let mut reader = std::io::BufReader::new(stream);
    let mut response = String::new();
    reader
        .read_line(&mut response)
        .map_err(|e| format!("Failed to read response from daemon: {}", e))?;
    Ok(response.trim().to_string())
}

fn speak_handler(args: &Value) -> Result<String, String> {
    let mut raw_text = String::new();

    if let Some(path) = args.get("file_path").and_then(|v| v.as_str()) {
        if !path.is_empty() {
            raw_text = std::fs::read_to_string(path)
                .map_err(|e| format!("Failed to read file '{}': {}", path, e))?;
        }
    }

    if raw_text.is_empty() {
        if let Some(t) = args.get("text").and_then(|v| v.as_str()) {
            raw_text = t.to_string();
        }
    }

    if raw_text.is_empty() {
        return Err("Either 'text' or 'file_path' must be provided.".to_string());
    }

    let cleaned = strip_markdown(&raw_text);
    if cleaned.is_empty() {
        return Ok("Nothing to speak.".to_string());
    }

    let voice = args.get("voice").and_then(|v| v.as_str()).map(|s| s.to_string());
    let volume = args.get("volume").and_then(|v| v.as_f64()).map(|f| (f / 100.0) as f32);

    let payload = json!({
        "text": cleaned,
        "voice": voice,
        "volume": volume,
    });

    let resp = send_daemon_cmd(&format!("SPEAK:{}", payload))?;
    if resp == "ok" {
        Ok("Speech queued to Voicy daemon.".to_string())
    } else {
        Err(format!("Voicy daemon error: {}", resp))
    }
}

fn stop_handler() -> Result<String, String> {
    let resp = send_daemon_cmd("STOP_TTS")?;
    Ok(format!("Voicy TTS stopped: {}", resp))
}

fn queue_free_handler() -> Result<String, String> {
    let resp = send_daemon_cmd("TTS_CLEAR_AND_STOP")?;
    Ok(format!("Voicy TTS queue cleared and stopped: {}", resp))
}

fn list_voices_handler() -> Result<String, String> {
    let voices_dir = std::path::Path::new("/mnt/sda4/models/phonon-31m-en/voices");
    if !voices_dir.exists() {
        return Err("Voices directory not found".to_string());
    }

    let mut voices = Vec::new();
    if let Ok(entries) = std::fs::read_dir(voices_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("safetensors") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    voices.push(stem.to_string());
                }
            }
        }
    }
    voices.sort();

    let json_list = serde_json::json!({
        "total": voices.len(),
        "default": "Alba",
        "favorites": ["Alba", "Noa", "Aldo", "Teddy", "Spencer", "Alfie", "Edwin", "Hugh", "Toby"],
        "voices": voices
    });

    serde_json::to_string_pretty(&json_list).map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    let mut lines = stdin.lock().lines();

    while let Some(Ok(line)) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }

        let req: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let response = match req.method.as_str() {
            "initialize" => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: Some(json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "voicy-tts",
                        "version": "1.0.0"
                    }
                })),
                error: None,
            },
            "notifications/initialized" => continue,
            "tools/list" => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: Some(json!({
                    "tools": [
                        {
                            "name": "speak",
                            "description": "Clean markdown text, synthesize speech using Voicy on-device Phonon TTS, and play audio seamlessly in a lookahead queue.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "text": {
                                        "type": "string",
                                        "description": "The text or markdown to read aloud."
                                    },
                                    "file_path": {
                                        "type": "string",
                                        "description": "Optional path to a text file to read aloud."
                                    },
                                    "voice": {
                                        "type": "string",
                                        "description": "Optional voice name (e.g. 'Alfie', 'Edwin', 'Hugh', 'Freya', 'Maeve', etc.). Defaults to Alfie."
                                    },
                                    "volume": {
                                        "type": "integer",
                                        "description": "Playback volume (0 to 100). Default is 100."
                                    }
                                }
                            }
                        },
                        {
                            "name": "stop",
                            "description": "Stop current TTS speech immediately.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {}
                            }
                        },
                        {
                            "name": "queue_free",
                            "description": "Stop active speech and clear the entire upcoming speech queue.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {}
                            }
                        },
                        {
                            "name": "list_voices",
                            "description": "List all 102 available on-device voices for Voicy TTS, including default voice and favorite voices.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {}
                            }
                        }
                    ]
                })),
                error: None,
            },
            "tools/call" => {
                let params = req.params.unwrap_or(Value::Null);
                let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

                let (text, is_err) = match tool_name {
                    "speak" => match speak_handler(&arguments) {
                        Ok(msg) => (msg, false),
                        Err(e) => (e, true),
                    },
                    "stop" => match stop_handler() {
                        Ok(msg) => (msg, false),
                        Err(e) => (e, true),
                    },
                    "queue_free" => match queue_free_handler() {
                        Ok(msg) => (msg, false),
                        Err(e) => (e, true),
                    },
                    "list_voices" => match list_voices_handler() {
                        Ok(msg) => (msg, false),
                        Err(e) => (e, true),
                    },
                    _ => (format!("Unknown tool: {}", tool_name), true),
                };

                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: req.id,
                    result: Some(json!({
                        "content": [
                            {
                                "type": "text",
                                "text": text
                            }
                        ],
                        "isError": is_err
                    })),
                    error: None,
                }
            }
            _ => JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: None,
                error: Some(json!({
                    "code": -32601,
                    "message": "Method not found"
                })),
            },
        };

        if let Ok(res_str) = serde_json::to_string(&response) {
            let _ = writeln!(stdout, "{}", res_str);
            let _ = stdout.flush();
        }
    }
}
