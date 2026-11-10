// 事件序列断言：面板状态机的全部迁移走查。与后端 dictation.rs 的迁移测试呼应——
// 后端裁定"事件何时发"，这里裁定"事件到了面板进什么态"。
import { describe, expect, it } from "vitest";
import {
  badgeKeyOf,
  injectedOk,
  lampOf,
  nextState,
  type PanelEvent,
  type PanelState,
} from "./panelState";

const idle: PanelState = { kind: "idle" };

/** 从初始态依序喂事件，返回终态 */
function run(evs: PanelEvent[], start: PanelState = idle): PanelState {
  return evs.reduce(nextState, start);
}

const rec = (active = true, opening?: boolean): PanelEvent => ({
  type: "recording",
  active,
  opening,
});
const phase = (): PanelEvent => ({ type: "phase", stage: "transcribing" });
const result = (
  p: Partial<Extract<PanelEvent, { type: "result" }>> = {},
): Extract<PanelEvent, { type: "result" }> => ({
  type: "result",
  text: p.text,
  error: p.error,
  injected: p.injected,
  reason: p.reason,
});

describe("正常全程", () => {
  it("空闲 → 唤醒中 → 声纹 → 识别中 → 结果", () => {
    expect(run([rec(true, true)]).kind).toBe("opening");
    expect(run([rec(true, true), rec(true)]).kind).toBe("recording");
    const s = run([rec(true, true), rec(true), phase()]);
    expect(s).toEqual({ kind: "transcribing", note: undefined });
    const done = run([rec(true, true), rec(true), phase(), result({ text: "你好", injected: false })]);
    expect(done).toEqual({
      kind: "result",
      text: "你好",
      injected: false,
      reason: undefined,
    });
  });

  it("注入成功：结果即收起（回 idle）——成功路径零打扰", () => {
    const s = run([rec(true), phase(), result({ text: "你好", injected: true })]);
    expect(s.kind).toBe("idle");
  });

  it("桌面回退（injected:false）：进结果态展示复制按钮", () => {
    const s = run([
      rec(true),
      phase(),
      result({ text: "你好", injected: false, reason: "no_input_focus" }),
    ]);
    expect(s).toEqual({
      kind: "result",
      text: "你好",
      error: undefined,
      injected: false,
      reason: "no_input_focus",
    });
  });

  it("injectedOk 判据：无 error 且 injected 为 true", () => {
    expect(injectedOk(result({ text: "t", injected: true }))).toBe(true);
    expect(injectedOk(result({ error: "x", injected: true }))).toBe(false);
    expect(injectedOk(result({ text: "t", injected: false }))).toBe(false);
  });

  it("识别失败：进结果态并保留错误与 reason", () => {
    const s = run([
      rec(true),
      phase(),
      result({ error: "没采到任何声音", injected: undefined, reason: "silent_mic" }),
    ]);
    expect(s).toEqual({
      kind: "result",
      text: undefined,
      error: "没采到任何声音",
      injected: undefined,
      reason: "silent_mic",
    });
  });
});

describe("中止与复位", () => {
  it("唤醒中收到 recording:false → idle（Esc 中止开麦）", () => {
    expect(run([rec(true, true), rec(false)]).kind).toBe("idle");
  });

  it("录音中收到 recording:false → idle（Esc 中止录音）", () => {
    expect(run([rec(true), rec(false)]).kind).toBe("idle");
  });

  it("结果态按热键开新一段：opening 事件覆盖结果", () => {
    const s = run([rec(true), phase(), result({ error: "x" }), rec(true, true)]);
    expect(s.kind).toBe("opening");
  });
});

describe("事件语义边界（忠实镜像现状）", () => {
  it("phase 的非 transcribing stage 不改变状态", () => {
    const s = run([rec(true)], { kind: "recording" });
    expect(nextState(s, { type: "phase", stage: "whatever" })).toBe(s);
  });

  it("识别中再收到 opening（翻篇开新篇）→ 回加载态", () => {
    const s = run([rec(true), phase(), rec(true, true)]);
    expect(s.kind).toBe("opening");
  });

  it("录音中收到 result 切到结果态——现实可达，非竞态", () => {
    // 两条常态路径都发生在 phase(transcribing) 事件之前：静音结论（峰值判定早退）
    // 与 handle.stop() 失败。此刻面板还在声纹态，result 到达即切换——固化前端现状；
    // 若未来要加前端防线，此测试先红，逼人显式决策
    const silent = run([rec(true), result({ error: "没采到任何声音", reason: "silent_mic" })]);
    expect(silent.kind).toBe("result");
    const injected = run([rec(true), result({ text: "迟到", injected: true })]);
    expect(injected.kind).toBe("idle"); // injected=true 即收起
  });

  it("result 落在 opening 态（开麦失败路径）→ 进结果态显示错误", () => {
    const s = run([rec(true, true), result({ error: "无法打开麦克风：x" })]);
    expect(s).toEqual({
      kind: "result",
      text: undefined,
      error: "无法打开麦克风：x",
      injected: undefined,
      reason: undefined,
    });
  });

  it("result 落在 idle 态（冷启动无模型路径）→ 进结果态并带 reason", () => {
    const s = run([result({ error: "还没有可用的识别模型", reason: "no_model" })]);
    expect(s).toEqual({
      kind: "result",
      text: undefined,
      error: "还没有可用的识别模型",
      injected: undefined,
      reason: "no_model",
    });
  });

  it("result(undefined) 未注入成功也进结果态（未自动插入 · 文本在剪贴板）", () => {
    const s = run([rec(true), phase(), result({ text: "t", injected: false, reason: null })]);
    expect(s.kind).toBe("result");
  });
});

describe("相位视图与徽标映射（纯函数）", () => {
  it("lampOf：听写三态进固定布局，idle/result 不在；灯档位一一对应", () => {
    expect(lampOf("opening")).toEqual({ live: true, dot: "warm", label: "唤醒中" });
    expect(lampOf("recording")).toEqual({ live: true, dot: "live", label: "录音中" });
    expect(lampOf("transcribing")).toEqual({ live: true, dot: "busy", label: "识别中" });
    expect(lampOf("idle")).toEqual({ live: false, dot: "", label: "" });
    expect(lampOf("result")).toEqual({ live: false, dot: "", label: "" });
    // 标签恒为三字（live-label 恒宽的前提）
    for (const k of ["opening", "recording", "transcribing"] as const) {
      expect([...lampOf(k).label]).toHaveLength(3);
    }
  });

  it("badgeKeyOf：后端每个 reason 都有专属徽标，不落默认", () => {
    expect(badgeKeyOf({ injected: true })).toBe("injected");
    expect(badgeKeyOf({ injected: true, reason: "no_input_focus" })).toBe("injected"); // 注入成功优先
    // 后端 deliver 产出的两个非错误 reason（commands.rs）逐一有专属键
    expect(badgeKeyOf({ injected: false, reason: "accessibility" })).toBe("accessibility");
    expect(badgeKeyOf({ injected: false, reason: "no_input_focus" })).toBe("no_input_focus");
  expect(badgeKeyOf({ injected: false, reason: "unsure_focus" })).toBe("unsure_focus");
    // 未知/缺失 reason 才落默认徽标
    expect(badgeKeyOf({ injected: false, reason: null })).toBe("miss");
    expect(badgeKeyOf({ injected: false, reason: "clipboard_failed" })).toBe("miss"); // 已删的旧 reason
    expect(badgeKeyOf({ injected: false, reason: "whatever" })).toBe("miss");
    expect(badgeKeyOf({})).toBe("miss");
  });
});
