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

    // 3) 干活
    post_transcribe(port, wav)
}

/// 传输层：multipart 上传 WAV 给 llama-server，收回正文（与引擎管理解耦，可独立测试）
pub fn post_transcribe(port: u16, wav: &Path) -> Result<String, String> {
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

fn probe_health(port: u16) -> bool {
    let Ok(c) = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    c.get(format!("http://127.0.0.1:{port}/health"))
        .timeout(Duration::from_secs(2))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

// 等引擎把模型装进内存（冷启动 10~30 秒）。log_path 仅用于进程死亡时的报错指引。
fn wait_health(app: &AppHandle, port: u16, log_path: Option<&std::path::Path>) -> Result<u16, String> {
    let _ = app.emit(
        "phase",
        serde_json::json!({ "stage": "transcribing", "note": "正在启动本地引擎，加载模型…" }),
    );
    let deadline = Instant::now() + Duration::from_secs(120);
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
                    "引擎进程意外退出——模型与引擎版本可能不匹配{}",
                    log_path
                        .map(|p| format!("（日志：{}）", p.display()))
                        .unwrap_or_default()
                ));
            }
        }
        if probe_health(port) {
            return Ok(port);
        }
        if Instant::now() > deadline {
            kill_engine(app);
            return Err("引擎启动超时（120 秒）——模型文件可能损坏".into());
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn ensure_server(
    app: &AppHandle,
    model_id: &str,
    model_path: &str,
    mmproj: Option<&str>,
) -> Result<u16, String> {
    // 快路径：同模型的进程活着，且健康检查通过才复用。
    // "活着但没就绪"（预热正在装模型）时直接进等待循环——绝不杀掉重拉，否则预热前功尽弃。
    {
        let state = app.state::<SharedState>();
        let s = state.lock().map_err(|e| e.to_string())?;
        let mut guard = s.engine.lock().map_err(|e| e.to_string())?;
        if let Some(slot) = guard.as_mut() {
            if slot.model_key == model_id && slot.child.try_wait().ok().flatten().is_none() {
                let port = slot.port;
                drop(guard);
                drop(s);
                if probe_health(port) {
                    return Ok(port);
                }
                return wait_health(app, port, None);
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
    wait_health(app, port, Some(&log_path))
}

// 引擎预热：应用启动即后台拉起 llama-server 装载模型，
// 第一次听写命中热引擎，不再看到"首次启动本地引擎"等待。
// 云端模式/未配模型/文件缺失时静默跳过——首次使用路径照常兜底。
pub fn warmup(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let cfg = crate::config::load(&app);
        if cfg.stt.provider == "openai_compatible" {
            return; // 云端模式不需要本地引擎
        }
        let Some(model_id) = cfg.stt.local_model_id.clone() else {
            return; // 还没配模型，等模型库引导
        };
        let Some(item) = crate::models::library(&app)
            .into_iter()
            .find(|m| m.id == model_id && m.status == "ready")
        else {
            return;
        };
        let Some(model_path) = item.model_path.clone() else {
            return;
        };
        // 预热期间的 phase 事件发到隐藏面板，用户不可见
        if let Err(e) = ensure_server(&app, &model_id, &model_path, item.mmproj_path.as_deref()) {
            eprintln!("[engine] 预热失败（首次使用时会再试）：{e}");
        }
    });
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

// 收割上次运行留下的孤儿引擎：dev 重启/崩溃是硬杀（不走 RunEvent::Exit 退出钩子），
// 引擎进程会变孤儿、每个常驻上 GB 内存（实测一晚攒了 8 个）。
// 只杀命令行带本应用模型目录的 llama-server——别人的 llama-server 不碰。
pub fn reap_orphan_engines(app: &AppHandle) {
    let Ok(dir) = app.path().app_data_dir() else { return };
    let pattern = format!("llama-server.*{}", dir.join("models").display());
    let Ok(out) = std::process::Command::new("pgrep")
        .arg("-f")
        .arg(&pattern)
        .output()
    else {
        return;
    };
    // 启动早期调用：此刻不可能有自己的引擎在跑，匹配到的全是孤儿
    let stdout = String::from_utf8_lossy(&out.stdout);
    for pid in stdout.split_whitespace().filter_map(|p| p.parse::<u32>().ok()) {
        // 只杀 ppid==1 的真孤儿（父进程已死、被 launchd 收养）；
        // 双实例场景（dev 旧实例还活着）里别人的引擎 ppid 不是 1，不碰
        let Ok(ppid_out) = std::process::Command::new("ps")
            .args(["-o", "ppid=", "-p", &pid.to_string()])
            .output()
        else {
            continue;
        };
        if String::from_utf8_lossy(&ppid_out.stdout).trim() != "1" {
            continue;
        }
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 标记剥离_有标记取正文_无标记去首尾空白() {
        assert_eq!(strip_marker("language Chinese<asr_text>你好世界"), "你好世界");
        assert_eq!(strip_marker("  干净文本  "), "干净文本");
        assert_eq!(strip_marker(""), "");
    }

    // 进程守卫：测试任何路径退出（含断言失败）都带走 llama-server
    struct KillOnDrop(Child);
    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// 数据级 E2E：已知内容的真实语音 WAV → 独立拉起 llama-server → post_transcribe →
    /// 断言转写包含已知关键词。只断"非空"测的是链路通；关键词断的是链路对（复核 P1 修正）。
    /// fixture 不入库（含真实语音）：从 recordings 拷一段**已知说了什么**的录音为
    /// tests/fixtures/e2e.wav，并把下方 EXPECTED 关键词改成那句里的稳定词。
    #[test]
    #[ignore = "数据级 E2E：需本地模型与 llama-server（cargo test -- --ignored 显式跑）"]
    fn e2e_本地识别链路_真实音频到文本() {
        // 当前 fixture 内容："我在对着，非输入态的内容在说话，但是好像不太对。"
        const EXPECTED: &str = "输入";
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/e2e.wav");
        assert!(
            fixture.exists(),
            "缺少 fixture（{}）——从 recordings 拷一段真实语音过来，并更新 EXPECTED 关键词",
            fixture.display()
        );
        let home = std::env::var("HOME").unwrap();
        let app_dir = std::path::Path::new(&home).join("Library/Application Support/com.sayit.app");
        let cfg: crate::config::Config = serde_json::from_reader(
            std::fs::File::open(app_dir.join("config.json")).expect("读 config.json"),
        )
        .expect("解析 config.json");
        let model_id = cfg.stt.local_model_id.expect("未配置本地模型");
        let model = crate::models::catalog()
            .into_iter()
            .find(|m| m.id == model_id)
            .unwrap_or_else(|| panic!("模型 {model_id} 不在精选清单——E2E 暂只支持精选模型"));
        let dir = app_dir.join("models").join(&model_id);
        let model_path = dir.join(&model.files[0].filename);
        let mmproj = dir.join(&model.files[1].filename);
        assert!(model_path.exists() && mmproj.exists(), "模型文件缺失");

        // 引擎定位：与应用三级候选一致（设置路径 → brew → 托管目录），都找不到才报错
        let managed = app_dir.join("engines/llama.cpp/bin/llama-server");
        let bin = cfg
            .engine
            .path
            .clone()
            .filter(|p| std::path::Path::new(p).exists())
            .or_else(|| {
                ["/opt/homebrew/bin/llama-server".to_string(), managed.display().to_string()]
                    .into_iter()
                    .find(|p| std::path::Path::new(p).exists())
            })
            .expect("找不到 llama-server（设置路径/brew/托管目录都没有）");

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        // 引擎日志写临时文件：启动失败有的查（与应用落盘 llama-server.log 同理）
        let log_path = std::env::temp_dir().join(format!("sayit-e2e-llama-{port}.log"));
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .expect("建引擎日志");
        // 从 spawn 起就交给守卫：健康等待期断言失败也带走引擎
        let mut engine = KillOnDrop(
            Command::new(&bin)
                .arg("-m").arg(&model_path)
                .arg("--mmproj").arg(&mmproj)
                .arg("--host").arg("127.0.0.1")
                .arg("--port").arg(port.to_string())
                .arg("-c").arg("4096")
                .stdout(Stdio::null())
                .stderr(Stdio::from(log_file))
                .spawn()
                .expect("启动 llama-server"),
        );

        let deadline = Instant::now() + Duration::from_secs(120);
        while !probe_health(port) {
            // 进程先死就别傻等 120s（与应用 wait_health 同理）
            if let Ok(Some(_)) = engine.0.try_wait() {
                panic!("E2E 引擎进程提前退出——日志：{}", log_path.display());
            }
            assert!(Instant::now() < deadline, "引擎 120s 未就绪——日志：{}", log_path.display());
            std::thread::sleep(Duration::from_millis(400));
        }

        let text = post_transcribe(port, &fixture).expect("识别请求失败");
        eprintln!("E2E 识别结果：{text}");
        assert!(
            text.contains(EXPECTED),
            "识别结果应包含已知关键词『{EXPECTED}』（fixture 内容变了就同步更新关键词）：{text}"
        );
    }
}
