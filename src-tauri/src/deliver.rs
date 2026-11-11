// 交付模块 —— 由 commands.rs 拆出（结构铁律 8：commands 回归事件→效果编排壳）。
// 职责：识别结果的最后一公里——记历史、焦点探测调度、注入执行、失败带路、迟到守卫。
use crate::config;
use crate::system;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

// 迟到守卫：cycle0 取自状态机接受 SttDone 的时刻（Effect::Deliver 携带，唯一正确采样
// 点——deliver 入口再读会落进"锁释放→读"的缝隙）；stale 在主线程闭包内复读——热键
// 处理与该闭包同走主线程队列、彼此串行，复读值闭包执行期间不会再变。迟到的面板信号
// 会砸掉新现场（injected=true 直接收起录音中的面板），注入与剪贴板则照常（文字必交付）。
fn flow_cycle(app: &AppHandle) -> u64 {
    use tauri::Manager;
    let state = app.state::<crate::state::SharedState>();
    state.lock().map(|s| s.flow.cycle).unwrap_or(0)
}

pub fn deliver(
    app: &AppHandle,
    cfg: &config::Config,
    outcome: crate::dictation::SttOutcome,
    cycle0: u64,
) {
    match outcome {
        crate::dictation::SttOutcome::Text(text) => {
            let mode = cfg.injection.mode.clone();
            let app3 = app.clone();
            // 记入历史（注入与否都记）——"文字不丢"的兜底：设置→历史，逐条显式复制
            crate::history::record_history(app, &text);
            std::thread::spawn(move || {
                let report = crate::focus::probe(Duration::from_millis(300));
                if report.verdict != crate::focus::Verdict::Input {
                    println!("[focus] 无输入判定（{:?}）：{}", report.verdict, report.basis);
                }
                let app4 = app3.clone();
                let _ = app3.run_on_main_thread(move || {
                    let stale = flow_cycle(&app4) != cycle0;
                    if stale {
                        println!("[focus] 交付迟到（周期已翻篇）：抑制面板事件，注入照常");
                    }
                    // 注入契约（终稿 E；2026-11-11 增补：Unknown∧无 TTY 也走按钮）：
                    // Input（AX/TTY 判出）→ 注入；NoInput（AX 权威）→ 按钮；Unknown（TTY
                    // 也没救回，如系统设置等只报 AXWindow 的应用）→ 按钮不注入——注入大概率
                    // 空打，亮按钮是诚实反馈。当年"Unknown 不注入走按钮"因 ZCode 永久盲被否，
                    // 现依据已变：终端有 TTY 兜底、Electron 靠 AX 唤醒只盲首轮，重启此案。
                    // 按钮**不写剪贴板**（剪贴板最小侵入）；文字不丢由听写历史保证。
                    // 例外：辅助功能未授权时放行走注入路径——产出 NeedPermission 并自动带路
                    if let Some(reason) =
                        skip_inject_reason(report.verdict, crate::inject::accessibility_ok())
                    {
                        println!("[focus] 跳过注入（{:?}）：{}", report.verdict, report.basis);
                        if !stale {
                            let _ = app4.emit(
                                "result",
                                serde_json::json!({
                                    "text": text,
                                    "injected": false,
                                    "reason": reason
                                }),
                            );
                        }
                        return;
                    }
                    // 确认输入态（或未授权走注入兜底）：**先发 result，随即原生关窗，再做注入**。
                    // 两段史：① fe516e2 把 emit 提到注入前（剪贴板读写不再阻塞事件送达）；
                    // ② 但面板收起走前端回环（收到 result → invoke hide_panel → 主线程命令），
                    //    命令排在本闭包后面，被同步注入（剪贴板+⌘V，log show 实测 ~1.3s）挡住——
                    //    "result 到了面板还挂 1.3s"的真相（2026-11-11 毫秒级日志定位）。
                    // 现由 Rust 侧直接关窗（GoIdle 效果同款先例），前端回环保留作双保险（幂等）。
                    // 乐观报 injected:true；万一注入失败文字仍在剪贴板 + 历史，面板已隐藏是可接受的取舍
                    if !stale {
                        let _ = app4.emit(
                            "result",
                            serde_json::json!({ "text": text, "injected": true, "reason": null }),
                        );
                        crate::panel::hide_panel(app4.clone());
                    }
                    let t_inject = std::time::Instant::now();
                    let outcome = if mode == "type" {
                        crate::inject::type_text(&text)
                    } else {
                        crate::inject::paste_text(&text)
                    };
                    println!("[inject] {mode} 模式注入耗时 {}ms", t_inject.elapsed().as_millis());
                    match outcome {
                        crate::inject::InjectOutcome::Injected => {}
                        crate::inject::InjectOutcome::NeedPermission => {
                            // 权限缺失：面板回来亮文字与"去开启"按钮（点击才跳系统设置——
                            // 2026-11-11 用户拍板，不再自动带路；与权限门/no_model 同款提示式）。
                            // 迟到交付不弹——别抢新现场。文字已在剪贴板 + 历史兑底
                            if !stale {
                                crate::panel::show_panel(&app4);
                                let _ = app4.emit(
                                    "result",
                                    serde_json::json!({
                                        "text": text,
                                        "injected": false,
                                        "reason": "accessibility"
                                    }),
                                );
                            }
                        }
                        crate::inject::InjectOutcome::Failed(e) => {
                            eprintln!("[inject] 注入失败（面板已收，文字在剪贴板+历史）：{e}");
                        }
                    }
                });
            });
        }
        crate::dictation::SttOutcome::Failed { error, reason } => {
            let _ = app.emit(
                "result",
                serde_json::json!({ "error": error, "reason": reason }),
            );
            // 死麦自动带路（2026-11-11 用户拍板）：结论出现即自动开麦克风设置页——权限被拒
            // 的麦静默送全零，用户只看到"没采到声音"不知道去哪开权限。与权限门 B 案同款
            // 节流（AUTO_OPENED_MIC，每次运行一次）；MicDead 只在当前周期产生、SttDone
            // 已被状态机周期守卫过滤，到这里的 Failed 必然非迟到，无抢新轮焦点之虞
            if reason == Some("silent_mic")
                && !system::AUTO_OPENED_MIC.swap(true, Ordering::Relaxed)
            {
                // 走主线程开设置页：与 NeedPermission 带路同款（本分支可能跑在
                // 识别/观测线程，opener 虽大概率线程安全，带路对齐主线程零成本）
                let app5 = app.clone();
                let _ = app.run_on_main_thread(move || {
                    let _ = system::open_mic_settings(app5);
                });
            }
        }
    }
}



/// 纯核心：跳过注入的判据与徽标 reason（断言见 mod tests）。
/// Input → 注入；NoInput → no_input_focus；Unknown（TTY 未救回，probe 内已裁决）→
/// unsure_focus。辅助功能未授权 → 一律 None（放行走注入路径产出 NeedPermission 带路）
fn skip_inject_reason(verdict: crate::focus::Verdict, ax_ok: bool) -> Option<&'static str> {
    use crate::focus::Verdict;
    if !ax_ok {
        return None;
    }
    match verdict {
        Verdict::Input => None,
        Verdict::NoInput => Some("no_input_focus"),
        Verdict::Unknown => Some("unsure_focus"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::focus::Verdict;

    #[test]
    fn 跳过注入_三值判定与未授权例外() {
        assert_eq!(skip_inject_reason(Verdict::Input, true), None);
        assert_eq!(skip_inject_reason(Verdict::NoInput, true), Some("no_input_focus"));
        // Unknown∧无 TTY（TTY 已在 probe 内参与，Unknown 即未救回）→ 按钮
        assert_eq!(skip_inject_reason(Verdict::Unknown, true), Some("unsure_focus"));
        // 未授权例外：放行走注入路径带路（NeedPermission）
        assert_eq!(skip_inject_reason(Verdict::Unknown, false), None);
        assert_eq!(skip_inject_reason(Verdict::NoInput, false), None);
    }
}
