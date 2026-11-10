// 配置层：应用的"记忆"
// 结构体 <-> JSON 的翻译由 serde 自动完成，这里只定义结构 + 读写规则
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

// ---------- 配置结构（字段与 JSON 一一对应） ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Config {
    pub stt: SttConfig,
    pub engine: EngineConfig,
    pub model_library: Vec<ModelEntry>,
    pub hotkeys: HotkeyConfig,
    pub injection: InjectionConfig,
    pub panel: PanelConfig,
}

// 识别：本地 还是 云端
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SttConfig {
    pub provider: String, // "local" | "openai_compatible"
    pub local_model_id: Option<String>,
    pub cloud: CloudStt,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CloudStt {
    pub endpoint: String,
    pub api_key: String,
    pub model: String,
    pub language: String,
}

// 引擎：None = 自动探测（设置指定的路径优先）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EngineConfig {
    pub path: Option<String>,
}

// 模型库条目：curated（精选下载的）| user（用户自己登记的）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelEntry {
    pub id: String,
    pub name: String,
    pub source: String,
    pub requires: String, // 需要什么引擎，如 "llama_cpp"
    pub model_path: Option<String>,
    pub mmproj_path: Option<String>,
}

// 快捷键：数组结构 = 一个功能可绑多个键；"Fn" 是特殊令牌
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HotkeyConfig {
    pub dictation: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InjectionConfig {
    pub mode: String, // "paste" | "type"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PanelConfig {
    pub auto_hide_seconds: u64,
}

// ---------- 默认值（serde(default) 依赖这些：旧配置缺字段时自动补） ----------

impl Default for Config {
    fn default() -> Self {
        Self {
            stt: SttConfig::default(),
            engine: EngineConfig::default(),
            model_library: Vec::new(),
            hotkeys: HotkeyConfig::default(),
            injection: InjectionConfig::default(),
            panel: PanelConfig::default(),
        }
    }
}
impl Default for SttConfig {
    fn default() -> Self {
        Self {
            provider: "local".into(),
            // 不预选模型：新用户在模型库自己挑（竞品同理——首启引导下载，不静默给默认值）
            local_model_id: None,
            cloud: CloudStt::default(),
        }
    }
}
impl Default for CloudStt {
    fn default() -> Self {
        Self {
            endpoint: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            model: "whisper-1".into(),
            language: "zh".into(),
        }
    }
}
impl Default for EngineConfig {
    fn default() -> Self {
        Self { path: None }
    }
}
impl Default for ModelEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            source: "user".into(),
            requires: "llama_cpp".into(),
            model_path: None,
            mmproj_path: None,
        }
    }
}
impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            dictation: vec!["Option+Space".into()],
        }
    }
}
impl Default for InjectionConfig {
    fn default() -> Self {
        Self { mode: "paste".into() }
    }
}
impl Default for PanelConfig {
    fn default() -> Self {
        Self { auto_hide_seconds: 5 }
    }
}

// ---------- 读写 ----------

fn config_path(app: &AppHandle) -> PathBuf {
    // ~/Library/Application Support/com.voiceinput.app/config.json
    app.path()
        .app_data_dir()
        .expect("拿不到应用数据目录")
        .join("config.json")
}

pub fn load(app: &AppHandle) -> Config {
    let path = config_path(app);
    let _ = fs::create_dir_all(path.parent().unwrap());
    match fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(cfg) => cfg,
            Err(e) => {
                // 文件损坏：备份保命，返回默认值（绝不让用户卡在启动）
                let _ = fs::rename(&path, path.with_extension("json.broken"));
                eprintln!("[config] 解析失败（{e}），已备份为 .broken 并使用默认配置");
                Config::default()
            }
        },
        Err(_) => {
            // 首次运行：落一份默认配置
            let cfg = Config::default();
            let _ = save(app, &cfg);
            cfg
        }
    }
}

pub fn save(app: &AppHandle, cfg: &Config) -> Result<(), String> {
    let path = config_path(app);
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    // 原子写入：先写临时文件，再改名顶替——中途断电不会留下半个文件
    fs::write(&tmp, json).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}
