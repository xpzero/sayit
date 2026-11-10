// 全局共享状态：整个应用的可变中心——录音状态 + 下载取消标记 + 听写历史
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::audio::RecordingHandle;

/// 一条听写历史（设置页「历史」列表的条目；ts_ms 同时充当删除用的 id）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub ts_ms: u64,
    pub text: String,
}

pub struct AppState {
    /// 听写生命周期状态机（唯一的相位真相；迁移只经 dictation::step）
    pub flow: crate::dictation::State,
    /// 进行中的录音（None = 空闲）
    pub recording: Option<RecordingHandle>,
    /// 音量广播线程的运行开关
    pub broadcasting: Arc<AtomicBool>,
    /// 当前录音的音量（RMS*1000），广播线程读它发事件
    pub level: Option<Arc<AtomicU32>>,
    /// 进行中的下载：模型id -> 取消标记
    pub downloads: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// 常驻的本地识别引擎（llama-server 子进程），懒启动
    pub engine: Mutex<Option<crate::stt::local::EngineSlot>>,
    /// 听写历史（倒序，新在前；持久化 history.json，上限见 history::HISTORY_CAP）
    pub history: Mutex<Vec<HistoryEntry>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            flow: crate::dictation::State::new(),
            recording: None,
            broadcasting: Arc::new(AtomicBool::new(false)),
            level: None,
            downloads: Mutex::new(HashMap::new()),
            engine: Mutex::new(None),
            history: Mutex::new(Vec::new()),
        }
    }
}

pub type SharedState = Mutex<AppState>;
