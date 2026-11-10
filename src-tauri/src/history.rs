// 听写历史 —— 由 commands.rs 拆出（结构铁律 8：单一职责；搬家零行为变化）
// "文字不丢"的兜底：设置→历史，最近 N 条本地持久化（history.json），逐条显式复制。
use tauri::{AppHandle, Manager};

/// 历史上限：足够回溯，不至于让 history.json 无限膨胀
const HISTORY_CAP: usize = 100;

/// 纯核心：倒序插入 + 截断到上限（断言见 mod tests）
fn push_history(list: &mut Vec<crate::state::HistoryEntry>, entry: crate::state::HistoryEntry, cap: usize) {
    list.insert(0, entry);
    list.truncate(cap);
}

fn history_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join("history.json"))
}

/// 启动时加载（lib.rs setup 调用）。损坏则备份 .broken 忽略——与 config 同款防御
pub fn load_history(app: &AppHandle) {
    let Some(path) = history_path(app) else { return };
    let loaded = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<crate::state::HistoryEntry>>(&s).ok());
    match loaded {
        Some(list) => {
            let state = app.state::<crate::state::SharedState>();
            if let Ok(s) = state.lock() {
                if let Ok(mut h) = s.history.lock() {
                    *h = list;
                }
            }
        }
        None => {
            // 文件存在但解析失败：备份保命，从空历史开始
            if path.exists() {
                let _ = std::fs::rename(&path, path.with_extension("json.broken"));
                eprintln!("[history] 解析失败，已备份为 .broken 并从空历史开始");
            }
        }
    }
}

fn save_history_locked(list: &Vec<crate::state::HistoryEntry>, app: &AppHandle) {
    let Some(path) = history_path(app) else { return };
    if let Ok(json) = serde_json::to_string(list) {
        let _ = std::fs::write(path, json);
    }
}

/// 记一条（deliver 调用）：内存倒序插入 + 整文件重写（100 条的小文件，不搞增量）
pub(crate) fn record_history(app: &AppHandle, text: &str) {
    let entry = crate::state::HistoryEntry {
        ts_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        text: text.to_string(),
    };
    let state = app.state::<crate::state::SharedState>();
    if let Ok(s) = state.lock() {
        if let Ok(mut h) = s.history.lock() {
            push_history(&mut h, entry, HISTORY_CAP);
            save_history_locked(&h, app);
        }
    }
}

#[tauri::command]
pub fn get_history(app: AppHandle) -> Vec<crate::state::HistoryEntry> {
    let state = app.state::<crate::state::SharedState>();
    state
        .lock()
        .ok()
        .and_then(|s| s.history.lock().ok().map(|h| h.clone()))
        .unwrap_or_default()
}

#[tauri::command]
pub fn delete_history(app: AppHandle, ts_ms: u64) {
    let state = app.state::<crate::state::SharedState>();
    if let Ok(s) = state.lock() {
        if let Ok(mut h) = s.history.lock() {
            h.retain(|e| e.ts_ms != ts_ms);
            save_history_locked(&h, &app);
        }
    }
}

#[tauri::command]
pub fn clear_history(app: AppHandle) {
    let state = app.state::<crate::state::SharedState>();
    if let Ok(s) = state.lock() {
        if let Ok(mut h) = s.history.lock() {
            h.clear();
            save_history_locked(&h, &app);
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::HistoryEntry;

    #[test]
    fn 历史插入_倒序新在前_上限截断() {
        let mut list = Vec::new();
        let e = |ts: u64, t: &str| HistoryEntry { ts_ms: ts, text: t.into() };
        push_history(&mut list, e(1, "第一条"), 3);
        push_history(&mut list, e(2, "第二条"), 3);
        push_history(&mut list, e(3, "第三条"), 3);
        assert_eq!(list[0].text, "第三条", "新的在前");
        assert_eq!(list[2].text, "第一条");
        // 上限 3：第四条挤掉最旧的
        push_history(&mut list, e(4, "第四条"), 3);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].text, "第四条");
        assert_eq!(list[2].text, "第二条", "最旧的被截掉");
    }
}
