use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::File;
use std::io::{self, BufRead, Write};
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};

static ACTIVE_PID: AtomicI32 = AtomicI32::new(0);

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

fn get_playing_players() -> Vec<String> {
    let output = Command::new("playerctl").arg("-l").output();
    let mut playing = Vec::new();
    if let Ok(out) = output {
        let players_str = String::from_utf8_lossy(&out.stdout);
        for p in players_str.lines() {
            let p_trim = p.trim();
            if p_trim.is_empty() {
                continue;
            }
            if let Ok(status) = Command::new("playerctl")
                .args(["-p", p_trim, "status"])
                .output()
            {
                let s = String::from_utf8_lossy(&status.stdout).to_lowercase();
                if s.trim() == "playing" {
                    playing.push(p_trim.to_string());
                }
            }
        }
    }
    playing
}

fn pause_players(players: &[String]) {
    for p in players {
        let _ = Command::new("playerctl")
            .args(["-p", p, "pause"])
            .output();
    }
}

fn resume_players(players: &[String]) {
    for p in players {
        let _ = Command::new("playerctl")
            .args(["-p", p, "play"])
            .output();
    }
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

    let voice = args
        .get("voice")
        .and_then(|v| v.as_str())
        .unwrap_or("en-US-AvaNeural");

    let rate = args
        .get("rate")
        .and_then(|v| v.as_str())
        .unwrap_or("+5%");

    let volume = args
        .get("volume")
        .and_then(|v| v.as_i64())
        .unwrap_or(115);

    let audio_device = args
        .get("audio_device")
        .and_then(|v| v.as_str())
        .unwrap_or("pulse/alsa_output.pci-0000_00_1f.3.analog-stereo");

    let paused = get_playing_players();

    // Prepend a pause symbol so the synthesized speech has a natural 300ms leading silence
    let formatted_text = format!("... {}", cleaned);

    let temp_dir = std::env::temp_dir();
    let txt_path = temp_dir.join(format!("edge_tts_{}.txt", std::process::id()));
    let mp3_path = temp_dir.join(format!("edge_tts_{}.mp3", std::process::id()));

    if let Ok(mut f) = File::create(&txt_path) {
        let _ = f.write_all(formatted_text.as_bytes());
    }

    let edge_bin = dirs_next().unwrap_or_else(|| "edge-tts".to_string());

    let tts_res = Command::new(&edge_bin)
        .args([
            "--file",
            txt_path.to_str().unwrap(),
            "--voice",
            voice,
            "--rate",
            rate,
            "--write-media",
            mp3_path.to_str().unwrap(),
        ])
        .output();

    let _ = std::fs::remove_file(&txt_path);

    if let Err(e) = tts_res {
        resume_players(&paused);
        return Err(format!("Failed to execute edge-tts: {}", e));
    }

    let out = tts_res.unwrap();
    if !out.status.success() {
        resume_players(&paused);
        return Err(format!(
            "edge-tts error: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }

    pause_players(&paused);
    // Pre-playback settle time (0.2s) to allow audio sink/hardware routing to open cleanly
    std::thread::sleep(std::time::Duration::from_millis(200));

    let mut mpv_cmd = Command::new("mpv");
    mpv_cmd.args([
        "--no-terminal",
        &format!("--volume={}", volume),
        "--gapless-audio=yes",
        "--audio-wait-open=0.35",
    ]);

    if !audio_device.is_empty() && audio_device != "auto" {
        mpv_cmd.arg(format!("--audio-device={}", audio_device));
    }

    mpv_cmd.arg(mp3_path.to_str().unwrap());

    if let Ok(mut child) = mpv_cmd.spawn() {
        ACTIVE_PID.store(child.id() as i32, Ordering::SeqCst);
        let _ = child.wait();
        ACTIVE_PID.store(0, Ordering::SeqCst);
        // Post-playback flush time (0.5s) to allow hardware buffer to drain fully
        std::thread::sleep(std::time::Duration::from_millis(500));
    }

    resume_players(&paused);
    let _ = std::fs::remove_file(&mp3_path);

    Ok("Spoken successfully.".to_string())
}

fn stop_handler() -> String {
    let pid = ACTIVE_PID.swap(0, Ordering::SeqCst);
    if pid > 0 {
        unsafe {
            libc_kill(pid, 15);
        }
        return format!("Stopped active playback (PID {}).", pid);
    }
    let _ = Command::new("killall").arg("mpv").output();
    let _ = Command::new("playerctl").arg("play").output();
    "Stopped mpv playback.".to_string()
}

unsafe fn libc_kill(pid: i32, sig: i32) {
    let _ = Command::new("kill")
        .args([&format!("-{}", sig), &pid.to_string()])
        .output();
}

fn dirs_next() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = format!("{}/.local/bin/edge-tts", home);
    if std::path::Path::new(&path).exists() {
        Some(path)
    } else {
        Some("edge-tts".to_string())
    }
}

fn handle_call_tool(params: &Option<Value>) -> Value {
    let p = match params {
        Some(p) => p,
        None => return json!({"isError": true, "content": [{"type": "text", "text": "Missing params"}]}),
    };

    let tool_name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let default_args = json!({});
    let arguments = p.get("arguments").unwrap_or(&default_args);

    match tool_name {
        "speak" => match speak_handler(arguments) {
            Ok(msg) => json!({"content": [{"type": "text", "text": msg}]}),
            Err(err) => json!({"isError": true, "content": [{"type": "text", "text": err}]}),
        },
        "stop" => {
            let msg = stop_handler();
            json!({"content": [{"type": "text", "text": msg}]})
        }
        _ => json!({"isError": true, "content": [{"type": "text", "text": format!("Unknown tool: {}", tool_name)}]}),
    }
}

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.trim().is_empty() {
            continue;
        }

        let req: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let err_resp = json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {"code": -32700, "message": format!("Parse error: {}", e)}
                });
                writeln!(stdout, "{}", err_resp)?;
                stdout.flush()?;
                continue;
            }
        };

        let response = match req.method.as_str() {
            "initialize" => {
                json!({
                    "jsonrpc": "2.0",
                    "id": req.id,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {
                            "tools": {}
                        },
                        "serverInfo": {
                            "name": "edge-tts-mcp",
                            "version": "0.1.0"
                        },
                        "instructions": "This server provides text-to-speech narration using edge-tts. Agents should pass clean text or file paths. Use the `speak` tool ONLY when the user explicitly requests spoken audio output, wants a file read aloud, or is in hands-free interaction mode. Do NOT call this tool for standard text-based responses."
                    }
                })
            }
            "notifications/initialized" => continue,
            "tools/list" => {
                json!({
                    "jsonrpc": "2.0",
                    "id": req.id,
                    "result": {
                        "tools": [
                            {
                                "name": "speak",
                                "description": "Synthesizes and speaks text or file content aloud using edge-tts. Call this tool ONLY when the user explicitly requests audio speech or is interacting in hands-free mode. Supports clean markdown stripping, background player ducking, and hardware routing.",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {
                                        "text": {
                                            "type": "string",
                                            "description": "Direct text or markdown string to read aloud. Optional if file_path is provided."
                                        },
                                        "file_path": {
                                            "type": "string",
                                            "description": "Absolute filesystem path to a file to read completely from top to bottom (like an audio cat). Optional if text is provided."
                                        },
                                        "voice": {
                                            "type": "string",
                                            "description": "Edge TTS voice model (default: en-US-AvaNeural). Other options: en-US-JennyNeural, en-US-GuyNeural."
                                        },
                                        "rate": {
                                            "type": "string",
                                            "description": "Speech rate speed adjustment (default: +5%). E.g. +10%, -5%."
                                        },
                                        "volume": {
                                            "type": "integer",
                                            "description": "Playback volume level (default: 115)."
                                        },
                                        "audio_device": {
                                            "type": "string",
                                            "description": "Hardware audio device sink (default: pulse/alsa_output.pci-0000_00_1f.3.analog-stereo)."
                                        }
                                    }
                                }
                            },
                            {
                                "name": "stop",
                                "description": "Immediately halts any active TTS speech playback and resumes paused media players.",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {}
                                }
                            }
                        ]
                    }
                })
            }
            "tools/call" => {
                let res = handle_call_tool(&req.params);
                json!({
                    "jsonrpc": "2.0",
                    "id": req.id,
                    "result": res
                })
            }
            "ping" => {
                json!({
                    "jsonrpc": "2.0",
                    "id": req.id,
                    "result": {}
                })
            }
            _ => {
                json!({
                    "jsonrpc": "2.0",
                    "id": req.id,
                    "error": {"code": -32601, "message": format!("Method not found: {}", req.method)}
                })
            }
        };

        writeln!(stdout, "{}", response)?;
        stdout.flush()?;
    }

    Ok(())
}
