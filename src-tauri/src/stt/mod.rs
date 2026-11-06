// 识别模块：trait 插座 + 分发（第 3 讲的落地）
pub mod local;
pub mod openai_compatible;

use std::path::Path;
use tauri::AppHandle;

use crate::config::Config;

// 统一入口：看配置把活派给对应插头
pub fn transcribe(app: &AppHandle, cfg: &Config, wav: &Path) -> Result<String, String> {
    match cfg.stt.provider.as_str() {
        "openai_compatible" => openai_compatible::transcribe(cfg, wav),
        _ => local::transcribe(app, cfg, wav),
    }
}
