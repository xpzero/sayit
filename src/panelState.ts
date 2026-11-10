// 面板状态机（行为测试基座的前端半边）：后端事件 → 面板状态的映射收敛成纯函数，
// 事件序列断言在 panelState.test.ts。Panel.tsx 的监听器只负责副作用（计时器、
// 关窗、电平引用），状态迁移一律过这里——与后端 dictation.rs 同一套形状。
// 语义与抽取前的监听器逻辑逐行一致（UI 契约：零可见变化）。

export type PanelState =
  | { kind: "idle" }
  | { kind: "opening" } // 开麦中（设备唤醒可到秒级）：加载动效
  | { kind: "recording" } // 声纹：麦克风真的在收声
  | { kind: "transcribing"; note?: string }
  | {
      kind: "result";
      text?: string;
      error?: string;
      injected?: boolean | null;
      reason?: string | null;
    };

export type PanelEvent =
  | { type: "recording"; active: boolean; opening?: boolean }
  | { type: "phase"; stage: string; note?: string }
  | {
      type: "result";
      text?: string;
      error?: string;
      injected?: boolean;
      reason?: string | null;
    };

/** 注入成功的判据：状态收起（nextState）与关窗副作用（Panel 监听器）共用，单一事实源 */
export function injectedOk(ev: Extract<PanelEvent, { type: "result" }>): boolean {
  return !ev.error && ev.injected === true;
}

/** 听写期的相位视图（Panel 的固定布局 live-row 唯一数据源）：kind → 是否听写期 + 状态灯档位。
 *  映射错一档 = 用户看到错误状态名，断言在 panelState.test.ts */
export type Lamp = { live: boolean; dot: "" | "warm" | "live" | "busy"; label: string };
export function lampOf(kind: PanelState["kind"]): Lamp {
  switch (kind) {
    case "opening":
      return { live: true, dot: "warm", label: "唤醒中" };
    case "recording":
      return { live: true, dot: "live", label: "录音中" };
    case "transcribing":
      return { live: true, dot: "busy", label: "识别中" };
    default:
      return { live: false, dot: "", label: "" };
  }
}

/** 结果徽标键：后端 reason（commands.rs deliver）→ 前端徽标分支，单一事实源。
 *  任一侧 typo = 徽标静默落默认——纯函数 + 断言把这类错挡在测试层 */
export type BadgeKey = "injected" | "accessibility" | "no_input_focus" | "miss";
export function badgeKeyOf(r: { injected?: boolean | null; reason?: string | null }): BadgeKey {
  if (r.injected) return "injected";
  switch (r.reason) {
    case "accessibility":
      return "accessibility";
    case "no_input_focus":
      return "no_input_focus";
    default:
      return "miss";
  }
}

export function nextState(s: PanelState, ev: PanelEvent): PanelState {
  switch (ev.type) {
    case "recording":
      if (!ev.active) return { kind: "idle" }; // Esc 中止等场景：复位，避免声纹冻结在屏幕上
      // opening = 设备唤醒中，先给加载态；正式开录事件（不带 opening）再来时切成声纹
      return ev.opening ? { kind: "opening" } : { kind: "recording" };
    case "phase":
      if (ev.stage === "transcribing") return { kind: "transcribing", note: ev.note };
      return s; // 其余 stage 目前不存在，保持
    case "result":
      // 注入成功：文字已落进用户光标处，面板即刻收起（关窗由监听器副作用完成）。
      // 复制按钮只出现在"没注入进去"的现场（失败/桌面回退），成功路径保持零打扰
      if (injectedOk(ev)) return { kind: "idle" };
      return {
        kind: "result",
        text: ev.text,
        error: ev.error,
        injected: ev.injected,
        reason: ev.reason,
      };
  }
}
