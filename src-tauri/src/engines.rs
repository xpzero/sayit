// 引擎管理员：三级优先级解析 llama-server 的位置 + 能力体检
// ① 设置里手动指定的路径  ② 系统探测（PATH/brew）  ③ 应用数据目录里的托管副本
use serde::Serialize;
use std::path::PathBuf;
use std::process::Command;
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub found: bool,
    pub source: String, // "settings" | "system" | "managed" | "none"
    pub path: Option<String>,
    pub capable: bool, // 帮助输出里有 --mmproj 参数 = 支持音频模型
    pub detail: String,
}

pub fn detect(app: &AppHandle, configured: Option<String>) -> EngineStatus {
    // 按优先级收集候选路径
    let mut candidates: Vec<(String, PathBuf)> = Vec::new();

    if let Some(p) = configured {
        candidates.push(("settings".into(), PathBuf::from(p)));
    }
    if let Some(p) = which_llama_server() {
        candidates.push(("system".into(), p));
    }
    let managed = managed_path(app);
    if managed.exists() {
        candidates.push(("managed".into(), managed));
    }

    // 逐个体检，第一个存在且可执行的就用
    for (source, path) in candidates {
        if !path.exists() {
            continue;
        }
        let (capable, detail) = probe(&path);
        return EngineStatus {
            found: true,
            source,
            path: Some(path.display().to_string()),
            capable,
            detail,
        };
    }

    EngineStatus {
        found: false,
        source: "none".into(),
        path: None,
        capable: false,
        detail: "未找到 llama-server，可在首次引导中下载".into(),
    }
}

fn which_llama_server() -> Option<PathBuf> {
    // 已知常见位置（brew Apple Silicon / Intel），比扫 PATH 更稳
    let known = ["/opt/homebrew/bin/llama-server", "/usr/local/bin/llama-server"];
    for k in known {
        let p = PathBuf::from(k);
        if p.exists() {
            return Some(p);
        }
    }
    Command::new("which")
        .arg("llama-server")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

fn managed_path(app: &AppHandle) -> PathBuf {
    // 我们的托管目录（下载器将来安装到这里）
    app.path()
        .app_data_dir()
        .expect("拿不到应用数据目录")
        .join("engines/llama.cpp/bin/llama-server")
}

// 能力体检：看 --help 里有没有 --mmproj 参数（版本号会撒谎，参数列表不会）
fn probe(bin: &PathBuf) -> (bool, String) {
    let capable = Command::new(bin)
        .arg("--help")
        .output()
        .map(|o| {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            text.contains("--mmproj")
        })
        .unwrap_or(false);

    let detail = Command::new(bin)
        .arg("--version")
        .output()
        .ok()
        .map(|o| {
            let text = String::from_utf8_lossy(&o.stdout);
            text.lines()
                .find(|l| l.to_lowercase().contains("version"))
                .unwrap_or(text.lines().next().unwrap_or(""))
                .trim()
                .to_string()
        })
        .unwrap_or_else(|| "版本未知".into());

    (capable, detail)
}
