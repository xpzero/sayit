// 本地插头：雇 llama-server 当哑巴工人（第 3 讲"雇工不是嵌入"的落地）
// 懒启动：第一次识别才拉起进程，模型加载完成后保持待命；换模型/退出时杀掉
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

use crate::config::Config;
use crate::state::SharedState;

pub struct EngineSlot {
    pub model_key: String,
    pub child: Child,
    pub port: u16,
}

pub fn transcribe(app: &AppHandle, cfg: &Config, wav: &Path) -> Result<String, String> {
    // 1) 找到"就绪"的默认模型
    let item = crate::models::library(app)
        .into_iter()
        .find(|m| m.id == cfg.stt.local_model_id.clone().unwrap_or_default() && m.status == "ready")
        .ok_or("没有就绪的本地模型——去 设置 → 模型库 下载或设为默认")?;
    let model_path = item.model_path.clone().ok_or("模型文件路径缺失")?;

    // 2) 引擎就位（懒启动 + 复用）
    let port = ensure_server(
        app,
        &item.id,
        &model_path,
        item.mmproj_path.as_deref(),
    )?;

    // 3) 干活：multipart 上传 WAV，收回 JSON
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let form = reqwest::blocking::multipart::Form::new()
        .file("file", wav)
        .map_err(|e| format!("读取音频失败：{e}"))?
        .text("model", "asr");

    let resp = client
        .post(format!("http://127.0.0.1:{port}/v1/audio/transcriptions"))
        .timeout(Duration::from_secs(300))
        .multipart(form)
        .send()
        .map_err(|e| format!("引擎无响应：{e}"))?;

    if !resp.status().is_success() {
        return Err(format!("引擎返回 HTTP {}", resp.status()));
    }
    let json: serde_json::Value = resp.json().map_err(|e| format!("响应解析失败：{e}"))?;
    let raw = json["text"].as_str().unwrap_or("");
    Ok(strip_marker(raw))
}

// llama-server 会在正文前塞 "language Chinese<asr_text>" 这类内部标记（步骤 0 实测发现）
fn strip_marker(s: &str) -> String {
    match s.find("<asr_text>") {
        Some(i) => s[i + "<asr_text>".len()..].trim().to_string(),
        None => s.trim().to_string(),
    }
}

fn ensure_server(
    app: &AppHandle,
    model_id: &str,
    model_path: &str,
    mmproj: Option<&str>,
) -> Result<u16, String> {
    // 快路径：同模型的进程还活着，直接复用
    {
        let state = app.state::<SharedState>();
        let s = state.lock().map_err(|e| e.to_string())?;
        let mut guard = s.engine.lock().map_err(|e| e.to_string())?;
        if let Some(slot) = guard.as_mut() {
            if slot.model_key == model_id && slot.child.try_wait().ok().flatten().is_none() {
                return Ok(slot.port);
            }
        }
    }

    // 慢路径：先杀旧的（换模型），再拉新的
    kill_engine(app);
    let mmproj = mmproj.ok_or("该模型缺少 mmproj 音频投影文件")?;
    let bin = crate::engines::detect(app, crate::config::load(app).engine.path);
    let bin = bin
        .path
        .ok_or("未找到 llama-server 引擎——检查 设置 → 识别引擎")?;

    // 随机空闲端口：避免上次崩溃残留的进程占着老端口
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    drop(listener);

    // 引擎日志落盘：出问题时有的查（终端里看不到子进程输出）
    let log_path = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("llama-server.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| e.to_string())?;

    let child = Command::new(&bin)
        .arg("-m").arg(model_path)
        .arg("--mmproj").arg(mmproj)
        .arg("--host").arg("127.0.0.1")
        .arg("--port").arg(port.to_string())
        .arg("-c").arg("4096")
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file))
        .spawn()
        .map_err(|e| format!("启动引擎失败：{e}"))?;

    {
        let state = app.state::<SharedState>();
        let s = state.lock().map_err(|e| e.to_string())?;
        *s.engine.lock().map_err(|e| e.to_string())? = Some(EngineSlot {
            model_key: model_id.to_string(),
            child,
            port,
        });
    }

    // 等它把模型装进内存（首次 10~30 秒）
    let _ = app.emit(
        "phase",
        serde_json::json!({ "stage": "transcribing", "note": "首次启动本地引擎，加载模型（约10-30秒）…" }),
    );
    let deadline = Instant::now() + Duration::from_secs(120);
    let hc = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| e.to_string())?;
    loop {
        // 进程先死了？
        {
            let state = app.state::<SharedState>();
            let s = state.lock().map_err(|e| e.to_string())?;
            let mut guard = s.engine.lock().map_err(|e| e.to_string())?;
            let alive = guard
                .as_mut()
                .map(|slot| slot.child.try_wait().ok().flatten().is_none())
                .unwrap_or(false);
            if !alive {
                return Err(format!(
                    "引擎进程意外退出——模型与引擎版本可能不匹配（日志：{}）",
                    log_path.display()
                ));
            }
        }
        if hc
            .get(format!("http://127.0.0.1:{port}/health"))
            .timeout(Duration::from_secs(2))
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return Ok(port);
        }
        if Instant::now() > deadline {
            kill_engine(app);
            return Err("引擎启动超时（120 秒）——模型文件可能损坏".into());
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

// 杀掉引擎进程（换模型 / 退出应用时）
pub fn kill_engine(app: &AppHandle) {
    let state = app.state::<SharedState>();
    if let Ok(s) = state.lock() {
        if let Ok(mut slot) = s.engine.lock() {
            if let Some(mut old) = slot.take() {
                let _ = old.child.kill();
                let _ = old.child.wait();
            }
        }
    }
}
