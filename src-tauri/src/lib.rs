use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use hmac::{Hmac, Mac};
use image::{imageops::FilterType, DynamicImage};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{
    AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{
    connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream,
};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

const PRESET_TEXTS_A: [&str; 4] = ["嗯……", "哦？", "欸？", "哦！"];
const PRESET_TEXTS_B: [&str; 2] = ["我想想……", "等一下啊……"];
const PRESET_TEXTS_C: [&str; 3] = ["让我看一下啊……", "稍等，我看看……", "欸？我看到了……"];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Attachment {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    size: u64,
    original_size: u64,
    is_image: bool,
    is_text: bool,
    full_data_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Turn {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    voice_record_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attachments: Option<Vec<Attachment>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AudioSession {
    session_id: String,
    mode: String,
    target_chat_id: String,
    context: Vec<Turn>,
    settings: Value,
    pending_turns: Vec<Turn>,
    pending_screenshot: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenCollaborationPayload {
    settings: Option<Value>,
    target_chat_id: Option<String>,
    context: Option<Vec<Turn>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FishAudioRequest {
    text: Option<String>,
    api_base: Option<String>,
    api_key: Option<String>,
    voice_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct XunfeiCredentials {
    #[serde(default)]
    app_id: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    api_secret: String,
}

struct XunfeiHandle {
    audio_tx: mpsc::UnboundedSender<Vec<f32>>,
    finish_tx: oneshot::Sender<()>,
    abort_tx: oneshot::Sender<()>,
    result_rx: oneshot::Receiver<String>,
}

struct AppState {
    main_ready: AtomicBool,
    pending_main_commands: Mutex<Vec<Value>>,
    audio_session: Mutex<Option<AudioSession>>,
    audio_completion_sent: AtomicBool,
    xunfei: Mutex<Option<XunfeiHandle>>,
    desktop_shortcut: Mutex<Option<String>>,
    desktop_mouse_interactive: AtomicBool,
    developer_mode: AtomicBool,
    is_quitting: AtomicBool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            main_ready: AtomicBool::new(false),
            pending_main_commands: Mutex::new(Vec::new()),
            audio_session: Mutex::new(None),
            audio_completion_sent: AtomicBool::new(false),
            xunfei: Mutex::new(None),
            desktop_shortcut: Mutex::new(None),
            desktop_mouse_interactive: AtomicBool::new(false),
            developer_mode: AtomicBool::new(false),
            is_quitting: AtomicBool::new(false),
        }
    }
}

fn sanitize_turns(turns: Option<Vec<Turn>>) -> Vec<Turn> {
    let Some(turns) = turns else {
        return Vec::new();
    };
    turns
        .into_iter()
        .filter(|t| t.role == "user" || t.role == "assistant")
        .map(|mut t| {
            t.content = t.content.chars().take(200_000).collect();
            if t.role == "assistant" {
                t.voice_record_id = t
                    .voice_record_id
                    .as_deref()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                t.attachments = None;
            }
            if t.role == "user" {
                if let Some(attachments) = t.attachments {
                    t.attachments = Some(
                        attachments
                            .into_iter()
                            .filter(|a| a.is_image && !a.full_data_url.is_empty())
                            .map(|a| Attachment {
                                name: a.name.chars().take(200).collect(),
                                kind: a.kind.chars().take(100).collect(),
                                size: a.size,
                                original_size: a.original_size.max(a.size),
                                is_image: true,
                                is_text: false,
                                full_data_url: a.full_data_url.chars().take(30 * 1024 * 1024).collect(),
                            })
                            .collect(),
                    );
                }
            }
            t
        })
        .filter(|t| !t.content.trim().is_empty())
        .rev()
        .take(500)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn sanitize_settings(settings: Option<Value>) -> Value {
    let Some(source) = settings else {
        return json!({});
    };
    let mut clean = serde_json::Map::new();
    if let Some(obj) = source.as_object() {
        for (key, value) in obj {
            match value {
                Value::String(s) => {
                    clean.insert(key.clone(), Value::String(s.chars().take(200_000).collect()));
                }
                Value::Number(_) | Value::Bool(_) => {
                    clean.insert(key.clone(), value.clone());
                }
                _ => {}
            }
        }
    }
    Value::Object(clean)
}

fn is_main_sender(window: &tauri::Window) -> bool {
    window.label() == "main"
}

fn is_audio_sender(window: &tauri::Window) -> bool {
    window.label() == "audio" || window.label() == "desktop"
}

fn has_conversation_window(app: &AppHandle) -> bool {
    app.get_webview_window("audio").is_some() || app.get_webview_window("desktop").is_some()
}

fn show_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        if win.is_minimized().unwrap_or(false) {
            win.unminimize().ok();
        }
        win.show().ok();
        win.set_focus().ok();
    } else {
        create_main_window(app).ok();
    }
}

fn create_main_window(app: &AppHandle) -> Result<(), String> {
    app.state::<AppState>()
        .main_ready
        .store(false, Ordering::SeqCst);

    let win = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("AIUI")
        .inner_size(1200.0, 800.0)
        .min_inner_size(400.0, 300.0)
        .build()
        .map_err(|e| e.to_string())?;

    let app_clone = app.clone();
    let win_clone = win.clone();

    win.on_window_event(move |event| match event {
        WindowEvent::CloseRequested { api, .. } => {
            let state = app_clone.state::<AppState>();
            if !state.is_quitting.load(Ordering::SeqCst) {
                api.prevent_close();
                win_clone.hide().ok();
            }
        }
        WindowEvent::Destroyed => {
            app_clone
                .state::<AppState>()
                .main_ready
                .store(false, Ordering::SeqCst);
        }
        _ => {}
    });

    Ok(())
}

fn send_main_command(app: &AppHandle, action: &str, payload: Value) {
    let state = app.state::<AppState>();
    let mut command = payload;
    command["action"] = Value::String(action.to_string());

    if let Some(win) = app.get_webview_window("main") {
        if state.main_ready.load(Ordering::SeqCst) {
            win.emit("tray:action", &command).ok();
            return;
        }
    }

    state.pending_main_commands.lock().unwrap().push(command);
}

fn flush_main_commands(app: &AppHandle) {
    let state = app.state::<AppState>();
    if !state.main_ready.load(Ordering::SeqCst) {
        return;
    }

    let commands = {
        let mut guard = state.pending_main_commands.lock().unwrap();
        std::mem::take(&mut *guard)
    };

    if let Some(win) = app.get_webview_window("main") {
        for command in commands {
            win.emit("tray:action", &command).ok();
        }
    }
}

fn emit_audio_completion(app: &AppHandle, reason: &str, turns_override: Option<Vec<Turn>>) {
    let state = app.state::<AppState>();

    if state.audio_completion_sent.swap(true, Ordering::SeqCst) {
        return;
    }

    let guard = state.audio_session.lock().unwrap();
    let Some(session) = guard.as_ref() else {
        return;
    };

    let turns = sanitize_turns(Some(turns_override.unwrap_or_else(|| session.pending_turns.clone())));

    let payload = json!({
        "sessionId": session.session_id,
        "mode": session.mode,
        "targetChatId": session.target_chat_id,
        "reason": reason,
        "turns": turns,
    });

    if let Some(win) = app.get_webview_window("main") {
        win.emit("audio-chat:completed", payload).ok();
    }
}

fn abort_xunfei_session(app: &AppHandle) {
    if let Some(handle) = app.state::<AppState>().xunfei.lock().unwrap().take() {
        handle.abort_tx.send(()).ok();
    }
}

fn clear_desktop_shortcut(app: &AppHandle) {
    let state = app.state::<AppState>();
    let shortcut_str = state.desktop_shortcut.lock().unwrap().take();

    if let Some(shortcut_str) = shortcut_str {
        if let Ok(shortcut) = shortcut_str.parse::<Shortcut>() {
            app.global_shortcut().unregister(shortcut).ok();
        }
    }
}

fn register_desktop_shortcut(app: &AppHandle, settings: &Value) -> Result<String, String> {
    clear_desktop_shortcut(app);

    let accelerator = settings
        .get("desktopShortcut")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Control+A".to_string());

    let shortcut = accelerator
        .parse::<Shortcut>()
        .map_err(|e| format!("无法解析快捷键 {accelerator}: {e}"))?;

    app.global_shortcut()
        .register(shortcut)
        .map_err(|e| format!("无法注册传图快捷键：{accelerator}: {e}"))?;

    app.state::<AppState>()
        .desktop_shortcut
        .lock()
        .unwrap()
        .replace(accelerator.clone());

    Ok(accelerator)
}

fn preset_directory_key(api_base: &str, voice_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{api_base}|{voice_id}").as_bytes());
    let result = hasher.finalize();
    hex::encode(result)[..24].to_string()
}

fn fish_audio_config_from_value(value: &Value) -> (String, String, String) {
    let api_base = value
        .get("apiBase")
        .and_then(|v| v.as_str())
        .unwrap_or("https://fishaudio.org")
        .trim_end_matches('/')
        .to_string();
    let api_key = value
        .get("apiKey")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let voice_id = value
        .get("voiceId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    (api_base, api_key, voice_id)
}

async fn request_fish_audio(text: &str, config: &Value) -> Result<Vec<u8>, String> {
    let (base, api_key, voice_id) = fish_audio_config_from_value(config);

    if api_key.is_empty() || voice_id.is_empty() {
        return Err("请先填写 Fish Audio API Key 和音色 ID".into());
    }

    let url = format!("{base}/api/open/v1/speech/tts");
    let body = json!({
        "text": text,
        "voiceId": voice_id,
        "format": "mp3",
        "speed": 1,
    })
    .to_string();

    let client = Client::new();
    let mut last_error = String::new();

    for attempt in 0..2 {
        let response_result = tokio::time::timeout(
            Duration::from_secs(90),
            client
                .post(&url)
                .bearer_auth(&api_key)
                .header("Content-Type", "application/json")
                .body(body.clone())
                .send(),
        )
        .await;

        let response = match response_result {
            Ok(Ok(resp)) => resp,
            Ok(Err(e)) => {
                last_error = format!("Fish Audio 请求失败: {e}");
                if attempt == 0 {
                    continue;
                }
                return Err(last_error);
            }
            Err(_) => {
                last_error = "Fish Audio 请求超时".to_string();
                if attempt == 0 {
                    continue;
                }
                return Err(last_error);
            }
        };

        if response.status().is_success() {
            let bytes = response.bytes().await.map_err(|e| e.to_string())?;
            if bytes.is_empty() {
                return Err("Fish Audio 返回了空音频".into());
            }
            return Ok(bytes.to_vec());
        }

        let status = response.status().as_u16();
        let detail = response.text().await.unwrap_or_default();
        let detail = detail.chars().take(500).collect::<String>();
        last_error = format!("Fish Audio 请求失败 ({status}): {detail}");

        if attempt == 0 && (status == 401 || status >= 500) {
            continue;
        }

        return Err(last_error);
    }

    Err(if last_error.is_empty() {
        "Fish Audio 请求失败".into()
    } else {
        last_error
    })
}

async fn generate_voice_preset(app: &AppHandle, config: &Value) -> Result<Value, String> {
    let (api_base, api_key, voice_id) = fish_audio_config_from_value(config);

    if api_key.is_empty() || voice_id.is_empty() {
        return Err("请先填写 Fish Audio API Key 和音色 ID".into());
    }

    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    let dir = data_dir
        .join("voice-presets")
        .join(preset_directory_key(&api_base, &voice_id));

    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;

    let mut manifest = json!({
        "apiBase": api_base,
        "voiceId": voice_id,
        "groupA": [],
        "groupB": [],
        "groupC": [],
        "updatedAt": chrono::Utc::now().timestamp_millis(),
    });

    for (group, texts, prefix) in [
        ("groupA", PRESET_TEXTS_A.as_slice(), 'a'),
        ("groupB", PRESET_TEXTS_B.as_slice(), 'b'),
        ("groupC", PRESET_TEXTS_C.as_slice(), 'c'),
    ] {
        let mut files = Vec::new();
        for (index, text) in texts.iter().enumerate() {
            let filename = format!("{prefix}-{index}.mp3");
            let audio = request_fish_audio(text, config).await?;
            tokio::fs::write(dir.join(&filename), &audio)
                .await
                .map_err(|e| e.to_string())?;
            files.push(Value::String(filename));
        }
        manifest[group] = Value::Array(files);
    }

    tokio::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;

    let count = PRESET_TEXTS_A.len() + PRESET_TEXTS_B.len() + PRESET_TEXTS_C.len();

    Ok(json!({ "ok": true, "count": count }))
}

async fn load_preset_group(dir: &Path, files: &Value) -> Option<Value> {
    let files = files.as_array()?;
    let mut out = Vec::new();
    for file in files {
        let filename = file.as_str()?;
        let bytes = tokio::fs::read(dir.join(filename)).await.ok()?;
        out.push(Value::String(BASE64.encode(bytes)));
    }
    Some(Value::Array(out))
}

async fn read_voice_preset(app: &AppHandle, config: &Value) -> Option<Value> {
    let (api_base, _, voice_id) = fish_audio_config_from_value(config);
    if voice_id.is_empty() {
        return None;
    }

    let data_dir = app.path().app_data_dir().ok()?;
    let dir = data_dir
        .join("voice-presets")
        .join(preset_directory_key(&api_base, &voice_id));

    let manifest: Value = serde_json::from_str(
        &tokio::fs::read_to_string(dir.join("manifest.json"))
            .await
            .ok()?,
    )
    .ok()?;

    let group_a = load_preset_group(&dir, manifest.get("groupA").unwrap_or(&json!([]))).await;
    let group_b = load_preset_group(&dir, manifest.get("groupB").unwrap_or(&json!([]))).await;
    let group_c = load_preset_group(&dir, manifest.get("groupC").unwrap_or(&json!([]))).await;

    Some(json!({
        "groupA": group_a,
        "groupB": group_b,
        "groupC": group_c,
    }))
}

fn xunfei_build_url(api_key: &str, api_secret: &str) -> Result<String, String> {
    let host = "iat-api.xfyun.cn";
    let date = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string();
    let request_line = "GET /v2/iat HTTP/1.1";
    let signature_origin = format!("host: {host}\ndate: {date}\n{request_line}");

    let mut mac = Hmac::<Sha256>::new_from_slice(api_secret.as_bytes())
        .map_err(|e| e.to_string())?;
    mac.update(signature_origin.as_bytes());
    let signature = BASE64.encode(mac.finalize().into_bytes());

    let authorization_origin = format!(
        "api_key=\"{api_key}\", algorithm=\"hmac-sha256\", headers=\"host date request-line\", signature=\"{signature}\""
    );
    let authorization = BASE64.encode(authorization_origin.as_bytes());

    Ok(format!(
        "wss://{host}/v2/iat?authorization={}&date={}&host={host}",
        urlencoding::encode(&authorization),
        urlencoding::encode(&date)
    ))
}

fn xunfei_frame(app_id: &str, status: i32, pcm: &[u8]) -> String {
    json!({
        "common": { "app_id": app_id },
        "business": {
            "language": "zh_cn",
            "domain": "iat",
            "accent": "mandarin",
            "dwa": "wpgs",
            "vad_eos": 60000,
            "ptt": 0
        },
        "data": {
            "status": status,
            "format": "audio/L16;rate=16000",
            "encoding": "raw",
            "audio": BASE64.encode(pcm)
        }
    })
    .to_string()
}

async fn send_xunfei_frame(ws: &mut WsStream, app_id: &str, status: i32, pcm: &[u8]) -> Result<(), String> {
    ws.send(Message::Text(xunfei_frame(app_id, status, pcm)))
        .await
        .map_err(|e| e.to_string())
}

async fn flush_xunfei_samples(
    ws: &mut WsStream,
    app_id: &str,
    samples: &mut Vec<f32>,
    flush_all: bool,
) {
    const CHUNK_SIZE: usize = 640;

    while samples.len() >= CHUNK_SIZE || (flush_all && !samples.is_empty()) {
        let count = samples.len().min(CHUNK_SIZE);
        let chunk: Vec<f32> = samples.drain(..count).collect();

        let mut pcm = Vec::with_capacity(chunk.len() * 2);
        for sample in chunk {
            let clamped = sample.clamp(-1.0, 1.0);
            let int16 = if clamped < 0.0 {
                (clamped * 32768.0).round() as i16
            } else {
                (clamped * 32767.0).round() as i16
            };
            pcm.extend_from_slice(&int16.to_le_bytes());
        }

        send_xunfei_frame(ws, app_id, 1, &pcm).await.ok();
    }
}

fn handle_xunfei_message(
    app: &AppHandle,
    label: &str,
    raw: &str,
    latest_text: &mut String,
    finishing: bool,
    deadline: &mut Option<tokio::time::Instant>,
) -> bool {
    let response: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => {
            app.emit_to(label, "xunfei:error", format!("讯飞响应解析失败: {e}"))
                .ok();
            return false;
        }
    };

    if response.get("code").and_then(|v| v.as_i64()).unwrap_or(0) != 0 {
        let message = response
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("未知错误");
        app.emit_to(label, "xunfei:error", message).ok();
        return false;
    }

    if let Some(result) = response.pointer("/data/result").and_then(|v| v.as_object()) {
        if let Some(ws) = result.get("ws").and_then(|v| v.as_array()) {
            let mut text = String::new();
            for segment in ws {
                if let Some(cw) = segment.get("cw").and_then(|v| v.as_array()) {
                    for candidate in cw {
                        if let Some(w) = candidate.get("w").and_then(|v| v.as_str()) {
                            text.push_str(w);
                        }
                    }
                }
            }

            if !text.trim().is_empty() {
                *latest_text = text.clone();
                let pgs = result
                    .get("pgs")
                    .and_then(|v| v.as_str())
                    .unwrap_or("partial");
                let event = if pgs == "rpl" {
                    "xunfei:final"
                } else {
                    "xunfei:partial"
                };
                app.emit_to(label, event, &text).ok();

                if finishing {
                    if let Some(ref mut deadline) = deadline {
                        *deadline = tokio::time::Instant::now() + Duration::from_millis(700);
                    }
                }
            }
        }
    }

    if response
        .pointer("/data/status")
        .and_then(|v| v.as_i64())
        .unwrap_or(0)
        == 2
    {
        if let Some(ref mut deadline) = deadline {
            *deadline = tokio::time::Instant::now() + Duration::from_millis(250);
        }
    }

    true
}

async fn run_xunfei_session(
    app: AppHandle,
    creds: XunfeiCredentials,
    mut audio_rx: mpsc::UnboundedReceiver<Vec<f32>>,
    mut finish_rx: oneshot::Receiver<()>,
    mut abort_rx: oneshot::Receiver<()>,
    result_tx: oneshot::Sender<String>,
    label: String,
) {
    let url = match xunfei_build_url(&creds.api_key, &creds.api_secret) {
        Ok(url) => url,
        Err(e) => {
            app.emit_to(label.clone(), "xunfei:error", e).ok();
            result_tx.send(String::new()).ok();
            return;
        }
    };

    let (mut ws, _) = match connect_async(url).await {
        Ok(v) => v,
        Err(e) => {
            app.emit_to(
                label.clone(),
                "xunfei:error",
                format!("讯飞 WebSocket 连接失败: {e}"),
            )
            .ok();
            result_tx.send(String::new()).ok();
            return;
        }
    };

    if send_xunfei_frame(&mut ws, &creds.app_id, 0, &[])
        .await
        .is_err()
    {
        result_tx.send(String::new()).ok();
        return;
    }

    let mut samples = Vec::<f32>::new();
    let mut latest_text = String::new();
    let mut finishing = false;

    loop {
        tokio::select! {
            samples_opt = audio_rx.recv() => {
                match samples_opt {
                    Some(input) => {
                        samples.extend(input);
                        flush_xunfei_samples(&mut ws, &creds.app_id, &mut samples, false).await;
                    }
                    None => {
                        result_tx.send(latest_text.clone()).ok();
                        let _ = ws.close(None).await;
                        return;
                    }
                }
            }

            _ = &mut finish_rx => {
                finishing = true;
                flush_xunfei_samples(&mut ws, &creds.app_id, &mut samples, true).await;
                send_xunfei_frame(&mut ws, &creds.app_id, 2, &[]).await.ok();
                break;
            }

            _ = &mut abort_rx => {
                result_tx.send(latest_text.clone()).ok();
                let _ = ws.close(None).await;
                return;
            }

            msg = ws.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let mut deadline: Option<tokio::time::Instant> = None;
                        if !handle_xunfei_message(&app, &label, &text, &mut latest_text, finishing, &mut deadline) {
                            result_tx.send(latest_text.clone()).ok();
                            let _ = ws.close(None).await;
                            return;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        result_tx.send(latest_text.clone()).ok();
                        return;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        app.emit_to(label.clone(), "xunfei:error", e.to_string()).ok();
                        result_tx.send(latest_text.clone()).ok();
                        let _ = ws.close(None).await;
                        return;
                    }
                }
            }
        }
    }

    let mut deadline = tokio::time::Instant::now() + Duration::from_millis(3500);

    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }

        match tokio::time::timeout(deadline - now, ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let mut deadline_opt = Some(deadline);
                if !handle_xunfei_message(&app, &label, &text, &mut latest_text, true, &mut deadline_opt) {
                    break;
                }
                if let Some(new_deadline) = deadline_opt {
                    deadline = new_deadline;
                }
            }
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) => break,
            Ok(Some(Err(e))) => {
                app.emit_to(label.clone(), "xunfei:error", e.to_string()).ok();
                break;
            }
            Ok(Some(Ok(_))) => {}
            Err(_) => break,
        }
    }

    result_tx.send(latest_text.clone()).ok();
    let _ = ws.close(None).await;
}

fn screenshot_quality_settings(value: &str) -> (u32, u8) {
    match value.to_lowercase().as_str() {
        "low" => (1280, 72),
        "high" => (2560, 92),
        _ => (1920, 85),
    }
}

async fn capture_current_display_screenshot(app: &AppHandle) -> Result<(), String> {
    let quality = {
        let state = app.state::<AppState>();
        let guard = state.audio_session.lock().unwrap();
        guard
            .as_ref()
            .and_then(|s| s.settings.get("desktopImageQuality"))
            .and_then(|v| v.as_str())
            .unwrap_or("balanced")
            .to_string()
    };

    let audio_window = app.get_webview_window("desktop");
    if let Some(win) = &audio_window {
        win.hide().ok(); // 代替 set_opacity(0.0)
    }
    tokio::time::sleep(Duration::from_millis(70)).await;

    let result = async {
        let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
        let monitor = monitors
            .into_iter()
            .next()
            .ok_or_else(|| "无法获取当前显示器截图".to_string())?;

        let image = monitor.capture_image().map_err(|e| e.to_string())?;
        let (w, h) = image.dimensions();

        let (max_edge, jpeg_quality) = screenshot_quality_settings(&quality);
        let max_edge_px = max_edge;
        let resized = if w.max(h) > max_edge_px {
            let scale = max_edge_px as f64 / w.max(h) as f64;
            let new_w = (w as f64 * scale).round() as u32;
            let new_h = (h as f64 * scale).round() as u32;
            image::imageops::resize(&image, new_w, new_h, FilterType::Lanczos3)
        } else {
            image
        };

        let mut jpeg_bytes = Vec::new();
        DynamicImage::ImageRgba8(resized)
            .write_to(&mut Cursor::new(&mut jpeg_bytes), image::ImageFormat::Jpeg)
            .map_err(|e| e.to_string())?;

        Ok::<String, String>(format!(
            "data:image/jpeg;base64,{}",
            BASE64.encode(&jpeg_bytes)
        ))
    }
    .await;

    if let Some(win) = &audio_window {
        win.show().ok(); // 代替 set_opacity(1.0)
    }

    let data_url = result?;

    {
        let state = app.state::<AppState>();
        let mut guard = state.audio_session.lock().unwrap();
        if let Some(session) = guard.as_mut() {
            session.pending_screenshot = Some(data_url.clone());
        }
    }

    if let Some(win) = audio_window {
        win.emit(
            "desktop-work:screenshot-captured",
            json!({
                "dataUrl": data_url,
                "displayId": "0",
                "quality": quality,
            }),
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(())
}

async fn open_collaboration_window(
    app: AppHandle,
    window: tauri::Window,
    payload: Option<OpenCollaborationPayload>,
    mode: &str,
) -> Result<Value, String> {
    if !is_main_sender(&window) {
        return Err("非法的协作窗口请求".into());
    }

    if has_conversation_window(&app) {
        if let Some(win) = app.get_webview_window("audio").or_else(|| app.get_webview_window("desktop")) {
            win.set_focus().ok();
        }
        let session_id = app
            .state::<AppState>()
            .audio_session
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.session_id.clone())
            .unwrap_or_default();
        return Ok(json!({
            "ok": true,
            "sessionId": session_id,
            "reused": true,
        }));
    }

    app.state::<AppState>()
        .audio_completion_sent
        .store(false, Ordering::SeqCst);

    let payload = payload.unwrap_or(OpenCollaborationPayload {
        settings: None,
        target_chat_id: None,
        context: None,
    });

    let settings = sanitize_settings(payload.settings);

    let session = AudioSession {
        session_id: uuid::Uuid::new_v4().to_string(),
        mode: mode.to_string(),
        target_chat_id: payload.target_chat_id.unwrap_or_default(),
        context: sanitize_turns(payload.context),
        settings: settings.clone(),
        pending_turns: Vec::new(),
        pending_screenshot: None,
    };

    let mut shortcut = String::new();

    let label = if mode == "desktop" {
        shortcut = register_desktop_shortcut(&app, &settings)?;

        let (display_width, display_height, display_x, display_y) =
            match app.primary_monitor() {
                Ok(Some(monitor)) => {
                    let size = monitor.size();
                    let position = monitor.position();
                    (
                        size.width as f64,
                        size.height as f64,
                        position.x as f64,
                        position.y as f64,
                    )
                }
                _ => (1920.0, 1080.0, 0.0, 0.0),
            };

        let width = (display_width * 0.8).max(720.0);
        let height = 156.0;
        let x = display_x + (display_width - width) / 2.0;
        let y = display_y + display_height - height - 12.0;

        WebviewWindowBuilder::new(&app, "desktop", WebviewUrl::App("desktopwork.html".into()))
            .inner_size(width, height)
            .min_inner_size(640.0, height)
            .max_inner_size(10_000.0, height)
            .position(x, y)
            .visible(false)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .build()
            .map_err(|e| e.to_string())?;

        if let Some(win) = app.get_webview_window("desktop") {
            win.set_always_on_top(true).ok();
            win.set_ignore_cursor_events(true).map_err(|e| e.to_string())?;
        }

        if let Some(main) = app.get_webview_window("main") {
            main.minimize().ok();
        }

        "desktop"
    } else {
        WebviewWindowBuilder::new(&app, "audio", WebviewUrl::App("audiochat.html".into()))
            .inner_size(600.0, 600.0)
            .min_inner_size(520.0, 520.0)
            .visible(false)
            .build()
            .map_err(|e| e.to_string())?;
        "audio"
    };

    {
        let state = app.state::<AppState>();
        let mut guard = state.audio_session.lock().unwrap();
        *guard = Some(session.clone());
    }

    if let Some(win) = app.get_webview_window(label) {
        let app_clone = app.clone();
        let label_owned = label.to_string();
        win.on_window_event(move |event| match event {
            WindowEvent::CloseRequested { .. } => {
                emit_audio_completion(&app_clone, "window-close", None);
                abort_xunfei_session(&app_clone);
                clear_desktop_shortcut(&app_clone);

                if label_owned == "desktop" {
                    if let Some(main) = app_clone.get_webview_window("main") {
                        if main.is_minimized().unwrap_or(false) {
                            main.unminimize().ok();
                        }
                        main.show().ok();
                        main.set_focus().ok();
                    }
                }
            }
            WindowEvent::Destroyed => {
                let state = app_clone.state::<AppState>();
                *state.audio_session.lock().unwrap() = None;
                state.audio_completion_sent.store(false, Ordering::SeqCst);
                state.desktop_mouse_interactive.store(false, Ordering::SeqCst);
                refresh_tray_menu(&app_clone);
            }
            _ => {}
        });

        win.show().ok();
        win.set_focus().ok();
    }

    refresh_tray_menu(&app);

    Ok(json!({
        "ok": true,
        "sessionId": session.session_id,
        "reused": false,
        "shortcut": shortcut,
    }))
}

fn build_tray_menu(app: &AppHandle) -> Result<Menu<tauri::Wry>, String> {
    let collaboration_disabled = has_conversation_window(app);
    let menu = Menu::new(app).map_err(|e| e.to_string())?;

    let open_main = MenuItem::with_id(app, "open_main", "打开主页面", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let new_audio = MenuItem::with_id(
        app,
        "new_audio",
        "开启新语音聊天",
        !collaboration_disabled,
        None::<&str>,
    )
    .map_err(|e| e.to_string())?;
    let new_desktop = MenuItem::with_id(
        app,
        "new_desktop",
        "开启新桌面协作",
        !collaboration_disabled,
        None::<&str>,
    )
    .map_err(|e| e.to_string())?;
    let recent_audio = MenuItem::with_id(
        app,
        "recent_audio",
        "从最近会话开启语音聊天",
        !collaboration_disabled,
        None::<&str>,
    )
    .map_err(|e| e.to_string())?;
    let recent_desktop = MenuItem::with_id(
        app,
        "recent_desktop",
        "从最近会话开启桌面协作",
        !collaboration_disabled,
        None::<&str>,
    )
    .map_err(|e| e.to_string())?;

    menu.append(&open_main).map_err(|e| e.to_string())?;
    menu.append(&new_audio).map_err(|e| e.to_string())?;
    menu.append(&new_desktop).map_err(|e| e.to_string())?;
    menu.append(&recent_audio).map_err(|e| e.to_string())?;
    menu.append(&recent_desktop).map_err(|e| e.to_string())?;

    if app
        .state::<AppState>()
        .developer_mode
        .load(Ordering::SeqCst)
    {
        menu.append(&PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let clear_logs = MenuItem::with_id(app, "clear_logs", "清除日志", true, None::<&str>)
            .map_err(|e| e.to_string())?;
        let export_logs = MenuItem::with_id(app, "export_logs", "导出日志", true, None::<&str>)
            .map_err(|e| e.to_string())?;
        menu.append(&clear_logs).map_err(|e| e.to_string())?;
        menu.append(&export_logs).map_err(|e| e.to_string())?;
    }

    menu.append(&PredefinedMenuItem::separator(app).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let quit = MenuItem::with_id(app, "quit", "退出AIUI", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    menu.append(&quit).map_err(|e| e.to_string())?;

    Ok(menu)
}

fn refresh_tray_menu(app: &AppHandle) {
    let menu = build_tray_menu(app).ok();
    if let Some(tray) = app.tray_by_id("main-tray") {
        tray.set_menu(menu).ok();
    }
}

fn create_tray(app: &AppHandle) -> Result<(), String> {
    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| "缺少默认图标".to_string())?;

    let menu = build_tray_menu(app)?;

    TrayIconBuilder::with_id("main-tray")
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open_main" => show_main_window(app),
            "new_audio" => send_main_command(app, "new-audio-chat", json!({})),
            "new_desktop" => send_main_command(app, "new-desktop-work", json!({})),
            "recent_audio" => send_main_command(app, "recent-audio-chat", json!({})),
            "recent_desktop" => send_main_command(app, "recent-desktop-work", json!({})),
            "clear_logs" => send_main_command(app, "clear-logs", json!({})),
            "export_logs" => send_main_command(app, "export-logs", json!({})),
            "quit" => {
                app.state::<AppState>()
                    .is_quitting
                    .store(true, Ordering::SeqCst);
                app.exit(0);
            }
            _ => {}
        })
        .build(app)
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
async fn audio_chat_open(
    app: AppHandle,
    window: tauri::Window,
    payload: Option<OpenCollaborationPayload>,
) -> Result<Value, String> {
    open_collaboration_window(app, window, payload, "audio").await
}

#[tauri::command]
async fn desktop_work_open(
    app: AppHandle,
    window: tauri::Window,
    payload: Option<OpenCollaborationPayload>,
) -> Result<Value, String> {
    open_collaboration_window(app, window, payload, "desktop").await
}

#[tauri::command]
async fn audio_chat_get_session(
    app: AppHandle,
    window: tauri::Window,
) -> Result<Option<AudioSession>, String> {
    if !is_audio_sender(&window) {
        return Err("语音会话不存在".into());
    }
    let state = app.state::<AppState>();
    let guard = state.audio_session.lock().unwrap();
    Ok(guard.clone())
}

#[tauri::command]
async fn desktop_work_get_session(
    app: AppHandle,
    window: tauri::Window,
) -> Result<Option<AudioSession>, String> {
    if !is_audio_sender(&window) {
        return Err("桌面协作会话不存在".into());
    }
    let state = app.state::<AppState>();
    let guard = state.audio_session.lock().unwrap();
    match guard.as_ref() {
        Some(session) if session.mode == "desktop" => Ok(guard.clone()),
        _ => Err("桌面协作会话不存在".into()),
    }
}

#[tauri::command]
async fn audio_chat_checkpoint(
    app: AppHandle,
    window: tauri::Window,
    turns: Option<Vec<Turn>>,
) -> Result<Value, String> {
    if !is_audio_sender(&window) {
        return Ok(json!({ "ok": false }));
    }

    let state = app.state::<AppState>();
    let mut guard = state.audio_session.lock().unwrap();
    if let Some(session) = guard.as_mut() {
        session.pending_turns = sanitize_turns(turns);
    }

    Ok(json!({ "ok": true }))
}

#[tauri::command]
async fn audio_chat_complete(
    app: AppHandle,
    window: tauri::Window,
    turns: Option<Vec<Turn>>,
) -> Result<Value, String> {
    if !is_audio_sender(&window) {
        return Ok(json!({ "ok": false }));
    }

    let sanitized = sanitize_turns(turns);
    {
        let state = app.state::<AppState>();
        let mut guard = state.audio_session.lock().unwrap();
        if let Some(session) = guard.as_mut() {
            session.pending_turns = sanitized.clone();
        }
    }

    emit_audio_completion(&app, "ended", Some(sanitized));
    abort_xunfei_session(&app);

    tokio::time::sleep(Duration::from_millis(120)).await;
    if let Some(win) = app.get_webview_window("audio") {
        win.close().ok();
    }

    Ok(json!({ "ok": true }))
}

#[tauri::command]
async fn desktop_work_complete(
    app: AppHandle,
    window: tauri::Window,
    turns: Option<Vec<Turn>>,
) -> Result<Value, String> {
    if !is_audio_sender(&window) {
        return Ok(json!({ "ok": false }));
    }

    let sanitized = sanitize_turns(turns);
    {
        let state = app.state::<AppState>();
        let mut guard = state.audio_session.lock().unwrap();
        if let Some(session) = guard.as_mut() {
            session.pending_turns = sanitized.clone();
        }
    }

    emit_audio_completion(&app, "ended", Some(sanitized));
    abort_xunfei_session(&app);
    clear_desktop_shortcut(&app);

    if let Some(main) = app.get_webview_window("main") {
        if main.is_minimized().unwrap_or(false) {
            main.unminimize().ok();
        }
        main.show().ok();
        main.set_focus().ok();
    }

    tokio::time::sleep(Duration::from_millis(120)).await;
    if let Some(win) = app.get_webview_window("desktop") {
        win.close().ok();
    }

    Ok(json!({ "ok": true }))
}

#[tauri::command]
async fn audio_chat_generate_preset(
    app: AppHandle,
    window: tauri::Window,
    config: Option<Value>,
) -> Result<Value, String> {
    if !is_main_sender(&window) {
        return Err("非法的预制语音请求".into());
    }
    generate_voice_preset(&app, &config.unwrap_or_else(|| json!({}))).await
}

#[tauri::command]
async fn audio_chat_get_preset(
    app: AppHandle,
    window: tauri::Window,
    config: Option<Value>,
) -> Result<Option<Value>, String> {
    if !is_audio_sender(&window) {
        return Ok(None);
    }
    Ok(read_voice_preset(&app, &config.unwrap_or_else(|| json!({}))).await)
}

#[tauri::command]
async fn desktop_work_get_preset(
    app: AppHandle,
    window: tauri::Window,
    config: Option<Value>,
) -> Result<Option<Value>, String> {
    if !is_audio_sender(&window) {
        return Ok(None);
    }
    Ok(read_voice_preset(&app, &config.unwrap_or_else(|| json!({}))).await)
}

#[tauri::command]
async fn audio_chat_tts(
    app: AppHandle,
    window: tauri::Window,
    request: FishAudioRequest,
) -> Result<String, String> {
    if !is_audio_sender(&window) {
        return Err("非法的 Fish Audio 请求".into());
    }

    let text = request
        .text
        .unwrap_or_default()
        .chars()
        .take(1000)
        .collect::<String>();

    let config = json!({
        "apiBase": request.api_base.unwrap_or_else(|| "https://fishaudio.org".into()),
        "apiKey": request.api_key.unwrap_or_default(),
        "voiceId": request.voice_id.unwrap_or_default(),
    });

    let audio = request_fish_audio(&text, &config).await?;
    Ok(BASE64.encode(audio))
}

#[tauri::command]
async fn desktop_work_tts(
    app: AppHandle,
    window: tauri::Window,
    request: FishAudioRequest,
) -> Result<String, String> {
    if !is_audio_sender(&window) {
        return Err("非法的 Fish Audio 请求".into());
    }

    let text = request
        .text
        .unwrap_or_default()
        .chars()
        .take(1000)
        .collect::<String>();

    let config = json!({
        "apiBase": request.api_base.unwrap_or_else(|| "https://fishaudio.org".into()),
        "apiKey": request.api_key.unwrap_or_default(),
        "voiceId": request.voice_id.unwrap_or_default(),
    });

    let audio = request_fish_audio(&text, &config).await?;
    Ok(BASE64.encode(audio))
}

#[tauri::command]
async fn desktop_work_set_interactive(
    app: AppHandle,
    window: tauri::Window,
    value: bool,
) -> Result<Value, String> {
    if !is_audio_sender(&window) {
        return Ok(json!({ "ok": false, "interactive": false }));
    }

    let state = app.state::<AppState>();
    let ok = state
        .audio_session
        .lock()
        .unwrap()
        .as_ref()
        .map(|s| s.mode == "desktop")
        .unwrap_or(false);

    if !ok || app.get_webview_window("desktop").is_none() {
        return Ok(json!({ "ok": false, "interactive": false }));
    }

    if let Some(win) = app.get_webview_window("desktop") {
        win.set_ignore_cursor_events(!value)
            .map_err(|e| e.to_string())?;
    }

    state.desktop_mouse_interactive.store(value, Ordering::SeqCst);

    Ok(json!({ "ok": true, "interactive": value }))
}

#[tauri::command]
async fn xunfei_start(
    app: AppHandle,
    window: tauri::Window,
    credentials: XunfeiCredentials,
) -> Result<Value, String> {
    if !is_audio_sender(&window) {
        return Err("非法的讯飞请求".into());
    }

    abort_xunfei_session(&app);

    let (audio_tx, audio_rx) = mpsc::unbounded_channel();
    let (finish_tx, finish_rx) = oneshot::channel();
    let (abort_tx, abort_rx) = oneshot::channel();
    let (result_tx, result_rx) = oneshot::channel();

    let app_clone = app.clone();
    let label = window.label().to_string();

    tauri::async_runtime::spawn(async move {
        run_xunfei_session(
            app_clone,
            credentials,
            audio_rx,
            finish_rx,
            abort_rx,
            result_tx,
            label,
        )
        .await;
    });

    *app.state::<AppState>().xunfei.lock().unwrap() = Some(XunfeiHandle {
        audio_tx,
        finish_tx,
        abort_tx,
        result_rx,
    });

    Ok(json!({ "ok": true }))
}

#[tauri::command]
async fn xunfei_audio(
    app: AppHandle,
    window: tauri::Window,
    samples: Vec<f32>,
) -> Result<(), String> {
    if !is_audio_sender(&window) {
        return Err("非法的讯飞请求".into());
    }

    if let Some(handle) = app.state::<AppState>().xunfei.lock().unwrap().as_ref() {
        handle.audio_tx.send(samples).ok();
    }

    Ok(())
}

#[tauri::command]
async fn xunfei_finish(app: AppHandle, window: tauri::Window) -> Result<String, String> {
    if !is_audio_sender(&window) {
        return Err("非法的讯飞请求".into());
    }

    let handle = app
        .state::<AppState>()
        .xunfei
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| "讯飞会话不存在".to_string())?;

    handle.finish_tx.send(()).map_err(|_| "讯飞会话已关闭".to_string())?;

    handle
        .result_rx
        .await
        .map_err(|_| "讯飞会话已关闭".to_string())
}

#[tauri::command]
async fn xunfei_abort(app: AppHandle, window: tauri::Window) -> Result<Value, String> {
    if !is_audio_sender(&window) {
        return Ok(json!({ "ok": false }));
    }

    abort_xunfei_session(&app);

    Ok(json!({ "ok": true }))
}

#[tauri::command]
async fn renderer_ready(app: AppHandle, window: tauri::Window) -> Result<(), String> {
    if !is_main_sender(&window) {
        return Ok(());
    }

    app.state::<AppState>()
        .main_ready
        .store(true, Ordering::SeqCst);
    flush_main_commands(&app);

    Ok(())
}

#[tauri::command(rename_all = "camelCase")]
async fn app_state_update(
    app: AppHandle,
    window: tauri::Window,
    state_value: Value,
) -> Result<(), String> {
    if !is_main_sender(&window) {
        return Ok(());
    }

    let developer_mode = state_value
        .get("developerMode")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    app.state::<AppState>()
        .developer_mode
        .store(developer_mode, Ordering::SeqCst);

    refresh_tray_menu(&app);

    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = capture_current_display_screenshot(&app).await {
                                if let Some(win) = app.get_webview_window("desktop") {
                                    win.emit("desktop-work:screenshot-error", e).ok();
                                }
                            }
                        });
                    }
                })
                .build(),
        )
        .setup(|app| {
            let app_handle = app.handle();
            create_main_window(app_handle).ok();
            create_tray(app_handle).ok();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            audio_chat_open,
            desktop_work_open,
            audio_chat_get_session,
            desktop_work_get_session,
            audio_chat_checkpoint,
            audio_chat_complete,
            desktop_work_complete,
            audio_chat_generate_preset,
            audio_chat_get_preset,
            desktop_work_get_preset,
            audio_chat_tts,
            desktop_work_tts,
            desktop_work_set_interactive,
            xunfei_start,
            xunfei_audio,
            xunfei_finish,
            xunfei_abort,
            renderer_ready,
            app_state_update,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                clear_desktop_shortcut(app_handle);
            }
        });
}