// 听写生命周期状态机（行为测试基座 v2 的纯核心）。
// 这里没有 IO、没有时间、没有线程——只有"什么相位收到什么事件，迁移到哪、产出什么效果"。
// 线程/面板/音频都是壳（commands.rs）：向这里喂数据、执行产出的效果。
// 全部迁移的可穷举断言固化在 mod tests——今天踩过的每一类编排事故在这里都有对应测试。

/// 生命周期相位（面板的 result 态不是相位：那是结果内容，随 result 事件出现）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,         // 空闲
    Opening,      // 开麦中（面板显示"唤醒中"）
    Recording,    // 已装流（announced 之前设备可能还在预热）
    Transcribing, // 已停止，识别中
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub phase: Phase,
    /// 听写周期号：每接受一次 TogglePressed（开新篇）+1。识别完成时若周期已翻篇，
    /// 结果一律丢弃——迟到的注入会污染用户正在用的新现场
    pub cycle: u64,
    /// Recording 相位下是否已向面板承诺"可以说话"（真信号或 2s 兜底），每轮至多一次
    pub announced: bool,
}

impl State {
    pub const fn new() -> Self {
        State { phase: Phase::Idle, cycle: 0, announced: false }
    }
    /// Esc 是否有意义（tap 无锁读取的镜像由此派生）
    pub fn abortable(&self) -> bool {
        matches!(self.phase, Phase::Opening | Phase::Recording)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SttOutcome {
    Text(String),
    /// error 面向结果位；reason 供面板挂"去设置"按钮（如 silent_mic）
    Failed { error: String, reason: Option<&'static str> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// 热键/面板按钮。toggle 语义由相位解释：空闲/识别中 = 开新篇；录音中 = 停止；开麦中 = 忽略
    TogglePressed,
    /// 开麦线程成功（流由壳随身携带，事件本身不携带）
    MicReady,
    /// 开麦重试后仍失败
    MicFailed(String),
    /// Esc 中止
    Abort,
    /// 首个真实信号（RMS 越过死麦线）
    Signal,
    /// 开录判定 2s 兜底
    ArmTimeout,
    /// 识别线程完成；携带它出发时的周期号
    SttDone { cycle: u64, outcome: SttOutcome },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// 起开麦线程（面板进"唤醒中"；失败自动重试一次仍是壳的活）
    SpawnOpener,
    /// 装流 + 起音量/开录判定线程（尚未向面板宣布开录）
    InstallAndWatch,
    /// 声纹亮起 = 向用户承诺"可以说话"
    AnnounceRecording,
    /// 收流写 WAV，按整段峰值决定起识别还是直接静音结论
    StopAndTranscribe { cycle: u64 },
    /// 交付识别结果（Text→注入+结果；Failed→结果位错误）。
    /// cycle = 状态机接受 SttDone 时的周期号（唯一正确采样点——壳据此识别"交付迟到"：
    /// 交付完成时周期已翻篇 = 用户开了新一轮，面板事件须抑制而注入照常）
    Deliver { outcome: SttOutcome, cycle: u64 },
    /// 周期已翻篇：迟到的识别结果丢弃（日志可查，不打扰用户）
    DropLateResult,
    /// 开麦失败：结果位报错
    FailOpen(String),
    /// 流已无主（中止后开麦线程才交货）：丢弃，不留状态不留文件
    DiscardStream,
    /// 广播 recording:false 并收起面板
    GoIdle,
    /// 当前相位下无意义的事件：显式记录而非静默吞掉
    Ignored,
}

/// 唯一的迁移函数。壳不得绕过它改变相位——那是这个基座存在的全部意义。
pub fn step(s: State, ev: &Event) -> (State, Vec<Effect>) {
    use Effect::*;
    use Phase::*;

    // 通则：迟到的识别结果（周期已翻篇）在任何相位都直接丢弃，相位纹丝不动
    if let Event::SttDone { cycle, .. } = ev {
        if *cycle != s.cycle {
            return (s, vec![DropLateResult]);
        }
    }

    let start_new = State { phase: Opening, cycle: s.cycle + 1, announced: false };

    match (s.phase, ev) {
        // —— 开新篇（空闲或识别中按热键 = 翻篇；后者使上一轮的识别结果作废）
        (Idle | Transcribing, Event::TogglePressed) => (start_new, vec![SpawnOpener]),

        // —— 开麦中
        (Opening, Event::TogglePressed) => (s, vec![Ignored]), // 闩锁：开麦中不重复开
        (Opening, Event::MicReady) => (State { phase: Recording, ..s }, vec![InstallAndWatch]),
        (Opening, Event::MicFailed(e)) => (State { phase: Idle, ..s }, vec![FailOpen(e.clone())]),
        (Opening, Event::Abort) => (State { phase: Idle, ..s }, vec![GoIdle]),
        // 中止后开麦线程才交货：流已无主
        (Idle, Event::MicReady) => (s, vec![DiscardStream]),

        // —— 录音中
        (Recording, Event::TogglePressed) => {
            (State { phase: Transcribing, ..s }, vec![StopAndTranscribe { cycle: s.cycle }])
        }
        (Recording, Event::Abort) => (State { phase: Idle, ..s }, vec![DiscardStream, GoIdle]),
        (Recording, Event::Signal | Event::ArmTimeout) if !s.announced => {
            (State { announced: true, ..s }, vec![AnnounceRecording])
        }
        (Recording, Event::Signal | Event::ArmTimeout) => (s, vec![Ignored]),

        // —— 识别中（同周期的结果照常交付；Esc/杂音无意义）
        (Transcribing, Event::SttDone { outcome, .. }) => (
            State { phase: Idle, ..s },
            vec![Deliver { outcome: outcome.clone(), cycle: s.cycle }],
        ),

        // —— 其余组合（中止后开麦失败、识别中的 Esc/信号等）
        _ => (s, vec![Ignored]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idle() -> State {
        State::new()
    }

    /// 常用前置：一路走到 Recording（announced 可选）
    fn to_recording(announced: bool) -> State {
        let (s, _) = step(idle(), &Event::TogglePressed);
        let (s, _) = step(s, &Event::MicReady);
        if announced {
            let (s, _) = step(s, &Event::Signal);
            s
        } else {
            s
        }
    }

    #[test]
    fn 正常全程_相位与效果逐步精确() {
        let (s, e) = step(idle(), &Event::TogglePressed);
        assert_eq!((s.phase, s.cycle), (Phase::Opening, 1));
        assert_eq!(e, vec![Effect::SpawnOpener]);

        let (s, e) = step(s, &Event::MicReady);
        assert_eq!(s.phase, Phase::Recording);
        assert!(!s.announced);
        assert_eq!(e, vec![Effect::InstallAndWatch]);

        let (s, e) = step(s, &Event::Signal);
        assert_eq!(e, vec![Effect::AnnounceRecording]);
        assert!(s.announced);

        let (s, e) = step(s, &Event::TogglePressed); // 录音中再按 = 停止
        assert_eq!(s.phase, Phase::Transcribing);
        assert_eq!(e, vec![Effect::StopAndTranscribe { cycle: 1 }]);

        let (s, e) = step(s, &Event::SttDone { cycle: 1, outcome: SttOutcome::Text("你好".into()) });
        assert_eq!(s.phase, Phase::Idle);
        assert_eq!(e, vec![Effect::Deliver { outcome: SttOutcome::Text("你好".into()), cycle: 1 }]);
    }

    #[test]
    fn 声纹只宣布一次_信号与兜底不叠加() {
        let s = to_recording(true);
        let (s2, e) = step(s, &Event::Signal);
        assert_eq!(e, vec![Effect::Ignored]);
        let (_, e) = step(s2, &Event::ArmTimeout);
        assert_eq!(e, vec![Effect::Ignored]);
    }

    #[test]
    fn 两秒兜底也能宣布() {
        let s = to_recording(false);
        let (s2, e) = step(s, &Event::ArmTimeout);
        assert_eq!(e, vec![Effect::AnnounceRecording]);
        assert!(s2.announced);
    }

    #[test]
    fn 开麦中重复按键被闩住_周期不翻() {
        let (s, _) = step(idle(), &Event::TogglePressed);
        let (s2, e) = step(s, &Event::TogglePressed);
        assert_eq!((s2.phase, s2.cycle), (Phase::Opening, 1));
        assert_eq!(e, vec![Effect::Ignored]);
    }

    #[test]
    fn 开麦失败回空闲_错误直达结果位() {
        let (s, _) = step(idle(), &Event::TogglePressed);
        let (s2, e) = step(s, &Event::MicFailed("无法打开麦克风".into()));
        assert_eq!(s2.phase, Phase::Idle);
        assert_eq!(e, vec![Effect::FailOpen("无法打开麦克风".into())]);
    }

    #[test]
    fn 开麦中被Esc_面板收起_迟到的流被丢弃() {
        let (s, _) = step(idle(), &Event::TogglePressed);
        let (s2, e) = step(s, &Event::Abort);
        assert_eq!(s2.phase, Phase::Idle);
        assert_eq!(e, vec![Effect::GoIdle]);
        // 开麦线程稍后交货：流已无主
        let (s3, e) = step(s2, &Event::MicReady);
        assert_eq!((s3.phase, e), (Phase::Idle, vec![Effect::DiscardStream]));
        assert!(!s3.abortable());
    }

    #[test]
    fn 开麦中被Esc_迟到的失败也不打扰() {
        let (s, _) = step(idle(), &Event::TogglePressed);
        let (s, _) = step(s, &Event::Abort);
        let (_, e) = step(s, &Event::MicFailed("x".into()));
        assert_eq!(e, vec![Effect::Ignored]);
    }

    #[test]
    fn 录音中被Esc_丢流并收面板() {
        let s = to_recording(true);
        let (s2, e) = step(s, &Event::Abort);
        assert_eq!(s2.phase, Phase::Idle);
        assert_eq!(e, vec![Effect::DiscardStream, Effect::GoIdle]);
    }

    #[test]
    fn 迟到的识别结果被丢弃_不掰走新一轮任何相位() {
        let s = to_recording(true);
        let (s, _) = step(s, &Event::TogglePressed); // 第一轮停止 → Transcribing, cycle=1
        let (s, _) = step(s, &Event::TogglePressed); // 翻篇 → Opening, cycle=2
        let late = Event::SttDone { cycle: 1, outcome: SttOutcome::Text("旧文本".into()) };

        // Opening、Recording(未宣布)、Recording(已宣布)三个相位下送达，都不得被掰走
        let (after_opening, e) = step(s, &late);
        assert_eq!(e, vec![Effect::DropLateResult]);
        assert_eq!(after_opening.phase, Phase::Opening);

        let (s, _) = step(s, &Event::MicReady);
        let (after_rec, e) = step(s, &late);
        assert_eq!(e, vec![Effect::DropLateResult]);
        assert_eq!(after_rec.phase, Phase::Recording);
        assert!(!after_rec.announced); // 连"是否已宣布"都不受污染

        let (s, _) = step(s, &Event::Signal);
        let (after_announced, e) = step(s, &late);
        assert_eq!(e, vec![Effect::DropLateResult]);
        assert_eq!(after_announced.phase, Phase::Recording);
    }

    #[test]
    fn 翻篇后本轮自己的结果照常交付() {
        let s = to_recording(true);
        let (s, _) = step(s, &Event::TogglePressed); // Transcribing, cycle=1
        let (s, _) = step(s, &Event::TogglePressed); // 翻篇, cycle=2
        let (s, _) = step(s, &Event::MicReady); // Recording
        let (s, _) = step(s, &Event::TogglePressed); // 停止 → Transcribing, cycle=2
        let (_, e) = step(s, &Event::SttDone { cycle: 2, outcome: SttOutcome::Text("新文本".into()) });
        assert_eq!(e, vec![Effect::Deliver { outcome: SttOutcome::Text("新文本".into()), cycle: 2 }]);
    }

    #[test]
    fn 识别中Esc无效_识别中按热键是翻篇() {
        let s = to_recording(true);
        let (s, _) = step(s, &Event::TogglePressed);
        let (s2, e) = step(s, &Event::Abort);
        assert_eq!(e, vec![Effect::Ignored]);
        let (s3, e) = step(s2, &Event::TogglePressed);
        assert_eq!((s3.phase, s3.cycle), (Phase::Opening, 2));
        assert_eq!(e, vec![Effect::SpawnOpener]);
    }

    #[test]
    fn 静音结论与识别失败同形_都走交付通道() {
        let s = to_recording(true);
        let (s, _) = step(s, &Event::TogglePressed);
        let outcome = SttOutcome::Failed {
            error: "没采到任何声音——麦克风可能未授权或被系统静音".into(),
            reason: Some("silent_mic"),
        };
        let (s2, e) = step(s, &Event::SttDone { cycle: 1, outcome: outcome.clone() });
        assert_eq!(s2.phase, Phase::Idle);
        assert_eq!(e, vec![Effect::Deliver { outcome, cycle: 1 }]);
    }

    #[test]
    fn 周期号只在开新篇时递增() {
        let (s, _) = step(idle(), &Event::TogglePressed); // cycle 1
        let (s, _) = step(s, &Event::TogglePressed); // 闩住：不递增
        assert_eq!(s.cycle, 1);
        let (s, _) = step(s, &Event::MicReady);
        let (s, _) = step(s, &Event::TogglePressed); // 停止：不递增
        assert_eq!(s.cycle, 1);
        let (s, _) = step(s, &Event::SttDone { cycle: 1, outcome: SttOutcome::Text(String::new()) });
        let (s, _) = step(s, &Event::TogglePressed); // 开新篇
        assert_eq!(s.cycle, 2);
    }

    #[test]
    fn abortable只覆盖开麦与录音() {
        assert!(!idle().abortable());
        let (s, _) = step(idle(), &Event::TogglePressed);
        assert!(s.abortable());
        let (s, _) = step(s, &Event::MicReady);
        assert!(s.abortable());
        let (s, _) = step(s, &Event::TogglePressed);
        assert!(!s.abortable()); // 识别中 Esc 无效
    }
}
