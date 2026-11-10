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
                if report.verdict == crate::focus::Verdict::NoInput {
                    println!("[focus] 非输入态，跳过注入（{}）", report.basis);
                }
                let app4 = app3.clone();
                let _ = app3.run_on_main_thread(move || {
                    let stale = flow_cycle(&app4) != cycle0;
                    if stale {
                        println!("[focus] 交付迟到（周期已翻篇）：抑制面板事件，注入照常");
                    }
                    // 注入契约（2026-11-10 终稿 E + 剪贴板最小侵入；黑名单层
                    // 已移除同日）：确认非输入态（AX 判定）才跳过注入并亮复制按钮——此时**不写剪贴板**
                    // （用户剪贴板里可能是自己的内容，不能霸占）；文字不丢由听写历史
                    // 保证（设置→历史，逐条显式复制），复制按钮点击时才写剪贴板。
                    // 判不出（AX 盲 App）照常注入：用户在此按键本身就是意图声明。
                    // 例外：辅助功能未授权时放行走注入路径——产出 NeedPermission 并自动带路
                    if report.verdict == crate::focus::Verdict::NoInput
                        && crate::inject::accessibility_ok()
                    {
                        if !stale {
                            let _ = app4.emit(
                                "result",
                                serde_json::json!({
                                    "text": text,
                                    "injected": false,
                                    "reason": "no_input_focus"
                                }),
                            );
                        }
                        return;
                    }
                    // 确认输入态（或未授权走注入兜底）：**先发 result 面板即收，再做注入**——
                    // 剪贴板备份/写入/⌘V/延迟还原不阻塞面板消失（用户反馈"消失很慢"，
                    // 注入里的剪贴板读写是新增耗时）。乐观报 injected:true；万一注入失败
                    // 文字仍在剪贴板 + 历史，面板已隐藏是可接受的取舍
                    if !stale {
                        let _ = app4.emit(
                            "result",
                            serde_json::json!({ "text": text, "injected": true, "reason": null }),
                        );
                    }
                    let outcome = if mode == "type" {
                        crate::inject::type_text(&text)
                    } else {
                        crate::inject::paste_text(&text)
                    };
                    match outcome {
                        crate::inject::InjectOutcome::Injected => {}
                        crate::inject::InjectOutcome::NeedPermission => {
                            // 自动带路：系统不许可弹窗，但我们可以自动翻到正确的设置页
                            // （每次运行只自动开一次；翻开关那一下永远必须用户亲手+密码。
                            // 迟到交付不弹——别在新一轮录音中途抢焦点开设置页）
                            if !stale && !system::AUTO_OPENED_ACCESSIBILITY.swap(true, Ordering::Relaxed) {
                                let _ = system::open_accessibility_page(&app4);
                            }
                            // 面板已隐藏，不再补发失败事件（文字在剪贴板+历史）
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


