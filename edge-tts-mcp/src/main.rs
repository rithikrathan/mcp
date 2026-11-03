use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, Write};
use std::os::unix::net::UnixListener;
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

static ACTIVE_PID: AtomicI32 = AtomicI32::new(0);

#[derive(Clone, Debug)]
struct SpeechTask {
    text: String,
    voice: String,
    rate: String,
    volume: i64,
    append_flush: bool,
    max_retries: u32,
}

struct QueueState {
    queue: VecDeque<SpeechTask>,
    current_child: Option<u32>,
    paused_players: Vec<String>,
}

static QUEUE_STATE: Lazy<(Mutex<QueueState>, Condvar)> = Lazy::new(|| {
    (
        Mutex::new(QueueState {
            queue: VecDeque::new(),
            current_child: None,
            paused_players: Vec::new(),
        }),
        Condvar::new(),
    )
});

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

fn dirs_next() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = format!("{}/.local/bin/edge-tts", home);
    if std::path::Path::new(&path).exists() {
        Some(path)
    } else {
        Some("edge-tts".to_string())
    }
}

unsafe fn libc_kill(pid: i32, sig: i32) {
    let _ = Command::new("kill")
        .args([&format!("-{}", sig), &pid.to_string()])
        .output();
}

fn start_worker_thread() {
    std::thread::spawn(move || {
        let (lock, cvar) = &*QUEUE_STATE;
        loop {
            let task = {
                let mut state = lock.lock().unwrap();
                while state.queue.is_empty() {
                    if !state.paused_players.is_empty() {
                        let players = std::mem::replace(&mut state.paused_players, Vec::new());
                        resume_players(&players);
                    }
                    state = cvar.wait(state).unwrap();
                }
                state.queue.pop_front().unwrap()
            };

            let formatted_text = if task.append_flush {
                format!("The, {}", task.text)
            } else {
                task.text.clone()
            };

            let temp_dir = std::env::temp_dir();
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let txt_path = temp_dir.join(format!("edge_tts_{}_{}.txt", std::process::id(), timestamp));
            let mp3_path = temp_dir.join(format!("edge_tts_{}_{}.mp3", std::process::id(), timestamp));

            if let Ok(mut f) = File::create(&txt_path) {
                let _ = f.write_all(formatted_text.as_bytes());
            }

            let edge_bin = dirs_next().unwrap_or_else(|| "edge-tts".to_string());
            let max_retries = task.max_retries.max(1);
            let mut attempts = 0;
            let mut success = false;

            while attempts < max_retries {
                attempts += 1;
                let tts_res = Command::new(&edge_bin)
                    .args([
                        "--file",
                        txt_path.to_str().unwrap(),
                        "--voice",
                        &task.voice,
                        "--rate",
                        &task.rate,
                        "--write-media",
                        mp3_path.to_str().unwrap(),
                    ])
                    .output();

                match tts_res {
                    Ok(out) if out.status.success() => {
                        success = true;
                        if attempts > 1 {
                            let _ = std::fs::write(
                                "/tmp/edge_tts_mcp.log",
                                format!("edge-tts succeeded after {} attempts.\n", attempts),
                            );
                        }
                        break;
                    }
                    Ok(out) => {
                        let err = String::from_utf8_lossy(&out.stderr);
                        let _ = std::fs::write(
                            "/tmp/edge_tts_mcp.log",
                            format!("edge-tts attempt {} failed: {}\n", attempts, err),
                        );
                        if attempts < max_retries {
                            std::thread::sleep(Duration::from_millis(400 * attempts as u64));
                        }
                    }
                    Err(e) => {
                        let _ = std::fs::write(
                            "/tmp/edge_tts_mcp.log",
                            format!("edge-tts attempt {} exec error: {}\n", attempts, e),
                        );
                        if attempts < max_retries {
                            std::thread::sleep(Duration::from_millis(400 * attempts as u64));
                        }
                    }
                }
            }

            let _ = std::fs::remove_file(&txt_path);

            if success {
                {
                    let mut state = lock.lock().unwrap();
                    let active = get_playing_players();
                    for p in active {
                        if !state.paused_players.contains(&p) {
                            state.paused_players.push(p);
                        }
                    }
                    pause_players(&state.paused_players);
                }

                std::thread::sleep(Duration::from_millis(150));

                let mut play_cmd = Command::new("ffplay");
                play_cmd.args([
                    "-nodisp",
                    "-autoexit",
                    "-loglevel", "quiet",
                    "-af", "adelay=250|250",
                    "-volume", &task.volume.to_string(),
                    mp3_path.to_str().unwrap(),
                ]);

                match play_cmd.spawn() {
                    Ok(mut child) => {
                        let pid = child.id() as i32;
                        ACTIVE_PID.store(pid, Ordering::SeqCst);
                        let _ = std::fs::write("/tmp/edge_tts_mcp.pid", pid.to_string());

                        {
                            let mut state = lock.lock().unwrap();
                            state.current_child = Some(child.id());
                        }

                        let _ = child.wait();

                        ACTIVE_PID.store(0, Ordering::SeqCst);
                        let _ = std::fs::remove_file("/tmp/edge_tts_mcp.pid");
                        {
                            let mut state = lock.lock().unwrap();
                            state.current_child = None;
                            if state.queue.is_empty() && !state.paused_players.is_empty() {
                                let players = std::mem::replace(&mut state.paused_players, Vec::new());
                                resume_players(&players);
                            }
                        }
                    }
                    Err(e) => {
                        let _ = std::fs::write("/tmp/edge_tts_mcp.log", format!("ffplay spawn error: {}\n", e));
                    }
                }
            } else {
                let mut state = lock.lock().unwrap();
                if state.queue.is_empty() && !state.paused_players.is_empty() {
                    let players = std::mem::replace(&mut state.paused_players, Vec::new());
                    resume_players(&players);
                }
            }

            let _ = std::fs::remove_file(&mp3_path);
        }
    });
}

fn start_ipc_server() {
    let socket_path = "/tmp/edge_tts_mcp.sock";
    let _ = std::fs::remove_file(socket_path);

    std::thread::spawn(move || {
        let listener = match UnixListener::bind(socket_path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("Failed to bind IPC socket {}: {}", socket_path, e);
                return;
            }
        };

        for stream in listener.incoming() {
            if let Ok(mut stream) = stream {
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() {
                    let trimmed = line.trim();
                    let resp = if trimmed.starts_with("SPEAK ") {
                        let json_str = &trimmed[6..];
                        if let Ok(val) = serde_json::from_str::<Value>(json_str) {
                            match speak_handler(&val) {
                                Ok(m) => format!("OK {}\n", m),
                                Err(e) => format!("ERR {}\n", e),
                            }
                        } else {
                            "ERR Invalid JSON\n".to_string()
                        }
                    } else {
                        match trimmed.to_lowercase().as_str() {
                            "stop" | "skip" => {
                                stop_current_item();
                                "SKIPPED\n".to_string()
                            }
                            "stop_all" | "clear" | "force" => {
                                stop_all_queue();
                                "CLEARED\n".to_string()
                            }
                            _ => "UNKNOWN\n".to_string(),
                        }
                    };
                    let _ = stream.write_all(resp.as_bytes());
                }
            }
        }
    });
}

fn stop_current_item() {
    let (lock, _cvar) = &*QUEUE_STATE;
    let mut state = lock.lock().unwrap();
    if let Some(child_pid) = state.current_child.take() {
        unsafe {
            libc_kill(child_pid as i32, 15);
        }
    }
    ACTIVE_PID.store(0, Ordering::SeqCst);
    let _ = std::fs::remove_file("/tmp/edge_tts_mcp.pid");
    let _ = Command::new("killall").arg("ffplay").output();

    // If nothing remains in queue, resume paused music right away
    if state.queue.is_empty() && !state.paused_players.is_empty() {
        let players = std::mem::replace(&mut state.paused_players, Vec::new());
        resume_players(&players);
    }
}

fn stop_all_queue() {
    let (lock, _cvar) = &*QUEUE_STATE;
    let mut state = lock.lock().unwrap();
    state.queue.clear();
    if let Some(child_pid) = state.current_child.take() {
        unsafe {
            libc_kill(child_pid as i32, 15);
        }
    }
    ACTIVE_PID.store(0, Ordering::SeqCst);
    let _ = std::fs::remove_file("/tmp/edge_tts_mcp.pid");
    let _ = Command::new("killall").arg("ffplay").output();
    let players = std::mem::replace(&mut state.paused_players, Vec::new());
    resume_players(&players);
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
        .unwrap_or("en-US-AvaNeural")
        .to_string();

    let rate = args
        .get("rate")
        .and_then(|v| v.as_str())
        .unwrap_or("+5%")
        .to_string();

    let volume = args
        .get("volume")
        .and_then(|v| v.as_i64())
        .unwrap_or(115);

    let append_flush = args
        .get("append_flush")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let max_retries = args
        .get("max_retries")
        .and_then(|v| v.as_u64())
        .unwrap_or(3) as u32;

    let task = SpeechTask {
        text: cleaned,
        voice,
        rate,
        volume,
        append_flush,
        max_retries,
    };

    let (lock, cvar) = &*QUEUE_STATE;
    let queue_len = {
        let mut state = lock.lock().unwrap();
        state.queue.push_back(task);
        cvar.notify_one();
        state.queue.len()
    };

    Ok(format!("Queued audio playback. Current queue length: {}.", queue_len))
}

fn stop_handler(args: &Value) -> String {
    let force = args
        .get("force")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if force {
        stop_all_queue();
        "Cleared TTS queue and stopped playback.".to_string()
    } else {
        stop_current_item();
        "Skipped current audio item.".to_string()
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
            let msg = stop_handler(arguments);
            json!({"content": [{"type": "text", "text": msg}]})
        }
        _ => json!({"isError": true, "content": [{"type": "text", "text": format!("Unknown tool: {}", tool_name)}]}),
    }
}

fn main() -> io::Result<()> {
    start_worker_thread();
    start_ipc_server();

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
                        "instructions": "This server provides text-to-speech narration using edge-tts with an internal queue. Agents should pass clean text or file paths. Use the `speak` tool ONLY when the user explicitly requests spoken audio output, wants a file read aloud, or is in hands-free interaction mode. Do NOT call this tool for standard text-based responses."
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
                                "description": "Enqueues text or file content to be spoken aloud using edge-tts. Supports markdown stripping, queueing, and media player ducking.",
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
                                        },
                                        "append_flush": {
                                            "type": "boolean",
                                            "description": "Whether to prepend 'The, ' to flush DAC buffer (default: false)."
                                        },
                                        "max_retries": {
                                            "type": "integer",
                                            "description": "Maximum retry attempts upon network drops (default: 3)."
                                        }
                                    }
                                }
                            },
                            {
                                "name": "stop",
                                "description": "Stops TTS playback. By default skips the current item and plays the next in queue. If force=true, clears the entire queue.",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {
                                        "force": {
                                            "type": "boolean",
                                            "description": "If true, clears the entire queue and halts all speech immediately."
                                        }
                                    }
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
