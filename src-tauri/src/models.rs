// 模型库与下载器（第 4 讲）：策展清单 + 五道工序（目标→流式落盘→校验→登记→广播）
// 断点续传靠 HTTP Range：.part 文件存在就从它的长度接着下
use crate::config;
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

// ---------- 精选货架（链接已于 2026-11 经 hf-mirror 核实） ----------

pub struct CuratedFile {
    pub url: String,
    pub filename: String,
}

pub struct CuratedModel {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub license: &'static str,
    pub size_hint: &'static str,
    pub recommended: bool,
    pub files: Vec<CuratedFile>,
}

pub fn catalog() -> Vec<CuratedModel> {
    let base = "https://hf-mirror.com/ggml-org";
    vec![
        CuratedModel {
            id: "qwen3-asr-0.6b",
            name: "Qwen3-ASR 0.6B",
            description: "千问语音识别 · 中英混说 · 均衡之选",
            license: "Apache 2.0（以模型页为准）",
            size_hint: "约 971 MB",
            recommended: true,
            files: vec![
                CuratedFile {
                    url: format!("{base}/Qwen3-ASR-0.6B-GGUF/resolve/main/Qwen3-ASR-0.6B-Q8_0.gguf"),
                    filename: "Qwen3-ASR-0.6B-Q8_0.gguf".into(),
                },
                CuratedFile {
                    url: format!("{base}/Qwen3-ASR-0.6B-GGUF/resolve/main/mmproj-Qwen3-ASR-0.6B-Q8_0.gguf"),
                    filename: "mmproj-Qwen3-ASR-0.6B-Q8_0.gguf".into(),
                },
            ],
        },
        CuratedModel {
            id: "qwen3-asr-1.7b",
            name: "Qwen3-ASR 1.7B",
            description: "更大更准 · 慢一些 · 适合对准确率要求高时",
            license: "Apache 2.0（以模型页为准）",
            size_hint: "约 2.5 GB",
            recommended: false,
            files: vec![
                CuratedFile {
                    url: format!("{base}/Qwen3-ASR-1.7B-GGUF/resolve/main/Qwen3-ASR-1.7B-Q8_0.gguf"),
                    filename: "Qwen3-ASR-1.7B-Q8_0.gguf".into(),
                },
                CuratedFile {
                    url: format!("{base}/Qwen3-ASR-1.7B-GGUF/resolve/main/mmproj-Qwen3-ASR-1.7B-Q8_0.gguf"),
                    filename: "mmproj-Qwen3-ASR-1.7B-Q8_0.gguf".into(),
                },
            ],
        },
    ]
}

// ---------- 目录约定 ----------

pub fn models_dir(app: &AppHandle) -> PathBuf {
    app.path().app_data_dir().expect("拿不到应用数据目录").join("models")
}

fn model_dir(app: &AppHandle, id: &str) -> PathBuf {
    models_dir(app).join(id)
}

// GGUF 魔数校验：读前 4 字节
fn is_gguf(path: &PathBuf) -> bool {
    fs::File::open(path)
        .and_then(|mut f| {
            let mut magic = [0u8; 4];
            use std::io::Read;
            f.read_exact(&mut magic).map(|_| magic)
        })
        .map(|m| m == *b"GGUF")
        .unwrap_or(false)
}

// ---------- 图书馆视图（给前端） ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub license: String,
    pub size_hint: String,
    pub recommended: bool,
    pub source: String, // curated | user
    pub status: String, // ready | partial | absent
    pub model_path: Option<String>,
    pub mmproj_path: Option<String>,
}

pub fn library(app: &AppHandle) -> Vec<LibraryItem> {
    let mut items = Vec::new();

    // 精选区：看磁盘上的实际文件状态
    for m in catalog() {
        // 防御（2026-11-10 启动崩溃事后加固）：条目文件不足 2 个时跳过并留观测点，
        // 绝不 panic——设置窗口启动即调本函数，这里一炸就是全应用死
        let (Some(f_model), Some(f_mmproj)) = (m.files.first(), m.files.get(1)) else {
            eprintln!("[models] 精选条目 {} 文件数 {}（需 2），跳过", m.id, m.files.len());
            continue;
        };
        let dir = model_dir(app, m.id);
        let model_p = dir.join(&f_model.filename);
        let mmproj_p = dir.join(&f_mmproj.filename);
        let m_ok = is_gguf(&model_p);
        let w_ok = is_gguf(&mmproj_p);
        let status = match (m_ok, w_ok) {
            (true, true) => "ready",
            (false, false) => "absent",
            _ => "partial",
        };
        items.push(LibraryItem {
            id: m.id.into(),
            name: m.name.into(),
            description: m.description.into(),
            license: m.license.into(),
            size_hint: m.size_hint.into(),
            recommended: m.recommended,
            source: "curated".into(),
            status: status.into(),
            model_path: m_ok.then(|| model_p.display().to_string()),
            mmproj_path: w_ok.then(|| mmproj_p.display().to_string()),
        });
    }

    // 自带区：读配置里的登记条目
    let cfg = crate::config::load(app);
    for e in cfg.model_library {
        let m_ok = e.model_path.as_ref().map(|p| is_gguf(&PathBuf::from(p))).unwrap_or(false);
        let w_ok = e
            .mmproj_path
            .as_ref()
            .map(|p| is_gguf(&PathBuf::from(p)))
            .unwrap_or(false);
        let status = match (m_ok, w_ok) {
            (true, true) => "ready",
            (false, false) => "absent",
            _ => "partial",
        };
        items.push(LibraryItem {
            id: e.id,
            name: e.name,
            description: "本地登记的模型".into(),
            license: "—".into(),
            size_hint: "—".into(),
            recommended: false,
            source: "user".into(),
            status: status.into(),
            model_path: e.model_path,
            mmproj_path: e.mmproj_path,
        });
    }

    items
}

// ---------- 下载器 ----------

fn emit(app: &AppHandle, payload: serde_json::Value) {
    let _ = app.emit("download-progress", payload);
}

pub fn download(app: &AppHandle, id: &str, cancel: Arc<AtomicBool>) -> Result<(), String> {
    let m = catalog().into_iter().find(|m| m.id == id).ok_or("未知模型")?;
    let dir = model_dir(app, id);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;

    let file_count = m.files.len();

    for (idx, f) in m.files.iter().enumerate() {
        let final_path = dir.join(&f.filename);
        let part_path = dir.join(format!("{}.part", f.filename));

        // 已完整存在的文件直接跳过（这就是"自动认领步骤0已下载文件"的机关）
        if is_gguf(&final_path) {
            emit(app, serde_json::json!({
                "id": id, "stage": "progress", "fileIndex": idx, "fileCount": file_count,
                "file": f.filename, "received": 1, "total": 1, "pct": 100,
                "note": "已存在，跳过"
            }));
            continue;
        }

        download_file(
            app, &client, id, &f.url, &part_path, &final_path, idx, file_count, &cancel,
        )?;
        if cancel.load(Ordering::Relaxed) {
            emit(app, serde_json::json!({ "id": id, "stage": "cancelled" }));
            return Ok(());
        }
    }

    emit(app, serde_json::json!({ "id": id, "stage": "done" }));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn download_file(
    app: &AppHandle,
    client: &reqwest::blocking::Client,
    id: &str,
    url: &str,
    part_path: &PathBuf,
    final_path: &PathBuf,
    file_index: usize,
    file_count: usize,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    // 断点续传：.part 存在就从它的长度处续
    let mut start = part_path.metadata().map(|m| m.len()).unwrap_or(0);

    let mut req = client.get(url);
    if start > 0 {
        req = req.header("Range", format!("bytes={start}-"));
    }
    let resp = req.send().map_err(|e| format!("连接失败：{e}"))?;

    let mut file = match resp.status().as_u16() {
        206 => {
            // 续传成功，追加写
            std::fs::OpenOptions::new()
                .append(true)
                .open(part_path)
                .map_err(|e| e.to_string())?
        }
        200 => {
            // 服务器不理会 Range（返回完整文件）——从头来，覆盖 .part
            start = 0;
            fs::File::create(part_path).map_err(|e| e.to_string())?
        }
        416 => {
            // 请求的范围超出文件：多半是 .part 已经下完了但没改名
            fs::rename(part_path, final_path).map_err(|e| e.to_string())?;
            if is_gguf(final_path) {
                return Ok(());
            }
            return Err("续传范围异常且文件无效，请删除后重下".into());
        }
        404 => return Err("文件不存在（404）——链接可能已失效".into()),
        s => return Err(format!("服务器返回 {s}")),
    };

    let content_len = resp.content_length();
    let total = content_len.map(|l| l + start);

    let mut reader = resp;
    let mut buf = [0u8; 64 * 1024];
    let mut received: u64 = start;
    let mut last_emit = Instant::now() - Duration::from_secs(1); // 第一块立即播报

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(()); // .part 留在原地，下次接着续
        }
        let n = reader.read(&mut buf).map_err(|e| format!("读取中断：{e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| format!("写盘失败：{e}"))?;
        received += n as u64;

        // 节流：最多每 100ms 广播一次进度，别刷爆 IPC
        if last_emit.elapsed() >= Duration::from_millis(100) {
            let pct = total
                .map(|t| (received as f64 / t as f64 * 100.0).round() as u64)
                .unwrap_or(0);
            emit(app, serde_json::json!({
                "id": id, "stage": "progress", "fileIndex": file_index, "fileCount": file_count,
                "file": final_path.file_name().unwrap().to_string_lossy(),
                "received": received, "total": total.unwrap_or(0), "pct": pct,
            }));
            last_emit = Instant::now();
        }
    }

    // 校验：GGUF 魔数（第 4 讂的"指纹"——上游没公布 sha256，用格式魔数兜底）
    let mut probe = fs::File::open(part_path).map_err(|e| e.to_string())?;
    let mut magic = [0u8; 4];
    use std::io::Read as _;
    probe.read_exact(&mut magic).map_err(|e| e.to_string())?;
    if magic != *b"GGUF" {
        let _ = fs::remove_file(part_path);
        return Err("下载完成但不是有效的 GGUF 文件（已自动清理，请重试）".into());
    }

    drop(file);
    fs::rename(part_path, final_path).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------- 自带模型登记 ----------

pub fn register_user_files(app: &AppHandle, paths: &[String]) -> Result<String, String> {
    let model = paths.iter().find(|p| !p.to_lowercase().contains("mmproj"));
    let mmproj = paths.iter().find(|p| p.to_lowercase().contains("mmproj"));
    let model = model.ok_or("请选择主模型文件（.gguf）")?;

    if !is_gguf(&PathBuf::from(model)) {
        return Err(format!("{} 不是有效的 GGUF 文件", short(model)));
    }
    if let Some(m) = mmproj {
        if !is_gguf(&PathBuf::from(m)) {
            return Err(format!("{} 不是有效的 GGUF 文件", short(m)));
        }
    }

    let name = PathBuf::from(model)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "未命名模型".into());

    let id = format!(
        "user-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );

    let mut cfg = crate::config::load(app);
    cfg.model_library.push(crate::config::ModelEntry {
        id: id.clone(),
        name,
        source: "user".into(),
        requires: "llama_cpp".into(),
        model_path: Some(model.clone()),
        mmproj_path: mmproj.cloned(),
    });
    crate::config::save(app, &cfg).map_err(|e| e.to_string())?;
    Ok(id)
}

fn short(p: &str) -> String {
    PathBuf::from(p)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| p.into())
}

// ---------- 模型库命令（由 commands.rs 并入其领域） ----------
#[tauri::command]
pub fn get_model_library(app: AppHandle) -> Vec<LibraryItem> {
    library(&app)
}

#[tauri::command]
pub fn download_model(app: AppHandle, state: State<'_, crate::state::SharedState>, id: String) {
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let s = state.lock().unwrap();
        let mut dls = s.downloads.lock().unwrap();
        // 同一个模型不重复开下
        if dls.contains_key(&id) {
            return;
        }
        dls.insert(id.clone(), flag.clone());
    }

    let app = app.clone();
    std::thread::spawn(move || {
        // 点击立即有反馈：先播报"开始连接"，绝不无声无息
        let _ = app.emit(
            "download-progress",
            serde_json::json!({ "id": id, "stage": "progress", "pct": 0, "note": "开始连接…" }),
        );
        // 防崩保护：线程内 panic 也要变成可见的错误事件
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            download(&app, &id, flag)
        }));
        match result {
            Ok(Ok(())) => {} // download() 内部已广播 done/cancelled
            Ok(Err(e)) => {
                let _ = app.emit(
                    "download-progress",
                    serde_json::json!({ "id": id, "stage": "error", "message": e }),
                );
            }
            Err(_) => {
                let _ = app.emit(
                    "download-progress",
                    serde_json::json!({ "id": id, "stage": "error", "message": "下载线程内部异常，请重试" }),
                );
            }
        }
        // 收摊：无论成败都摘掉取消标记（否则点第二次会被"已在下载"挡住）
        if let Some(state) = app.try_state::<crate::state::SharedState>() {
            if let Ok(s) = state.lock() {
                if let Ok(mut dls) = s.downloads.lock() {
                    dls.remove(&id);
                }
            }
        }
    });
}

#[tauri::command]
pub fn cancel_download(app: AppHandle, state: State<'_, crate::state::SharedState>, id: String) {
    if let Ok(s) = state.lock() {
        if let Ok(dls) = s.downloads.lock() {
            if let Some(flag) = dls.get(&id) {
                flag.store(true, Ordering::Relaxed);
            }
        }
    }
    let _ = app;
}

#[tauri::command]
pub fn delete_model(app: AppHandle, id: String) -> Result<(), String> {
    // 引擎可能正跑在这个模型上，先停掉
    crate::stt::local::kill_engine(&app);
    // 精选模型：删目录；自带模型：只从清单移除（文件是用户的，不动）
    if catalog().iter().any(|m| m.id == id) {
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("models")
            .join(&id);
        std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    } else {
        let mut cfg = config::load(&app);
        cfg.model_library.retain(|e| e.id != id);
        if cfg.stt.local_model_id.as_deref() == Some(id.as_str()) {
            cfg.stt.local_model_id = None;
        }
        config::save(&app, &cfg)?;
    }
    Ok(())
}

#[tauri::command]
pub fn add_user_model(app: AppHandle, paths: Vec<String>) -> Result<String, String> {
    register_user_files(&app, &paths)
}

#[tauri::command]
pub fn set_default_model(app: AppHandle, id: String) -> Result<(), String> {
    // 只有"就绪"的模型才有资格当默认
    let item = library(&app)
        .into_iter()
        .find(|m| m.id == id && m.status == "ready")
        .ok_or("模型不存在或未就绪")?;
    let mut cfg = config::load(&app);
    if cfg.stt.local_model_id.as_deref() != Some(item.id.as_str()) {
        crate::stt::local::kill_engine(&app); // 换默认模型 = 引擎要换血
    }
    cfg.stt.local_model_id = Some(item.id);
    config::save(&app, &cfg)
}

