// 悬浮面板 · 宣纸文人风
// 状态迁移一律过 panelState.ts 的 nextState（纯函数，断言在 panelState.test.ts）。
//
// 全程固定内容（用户拍板，抖动问题的终局方案）：听写期（唤醒/录音/识别）DOM 结构
// 完全固定——声纹条与状态灯同屏共存，相位变化只是属性级差异（声纹 canvas 内部起伏、
// 状态灯颜色/文字），没有任何结构切换。窗口几何固定（520×170），无任何盒级动画。
// 结果态是整个周期唯一一次内容替换（终端态，自动收起）。历史教训见 AGENTS.md。
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { badgeKeyOf, injectedOk, lampOf, nextState, type PanelState } from "./panelState";

const BARS = 30;

export default function Panel() {
  const [state, setState] = useState<PanelState>({ kind: "idle" });
  const hideTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const autoHide = useRef(5); // 面板自动收起秒数（启动时从配置读）
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const ampRef = useRef(0); // Rust 每 50ms 推来的 RMS，动画只读它

  useEffect(() => {
    invoke<{ panel: { autoHideSeconds: number } }>("get_config").then((c) => {
      autoHide.current = c.panel.autoHideSeconds;
    });

    const unsubs: Promise<UnlistenFn>[] = [
      listen<{ active: boolean; opening?: boolean }>("recording", (e) => {
        if (e.payload.active) {
          if (hideTimer.current) clearTimeout(hideTimer.current); // 新录音取消自动收起
          ampRef.current = 0;
        }
        setState((prev) =>
          nextState(prev, {
            type: "recording",
            active: e.payload.active,
            opening: e.payload.opening,
          }),
        );
      }),
      listen<{ stage: string; note?: string }>("phase", (e) => {
        setState((prev) =>
          nextState(prev, {
            type: "phase",
            stage: e.payload.stage,
            note: e.payload.note,
          }),
        );
      }),
      listen<{
        text?: string;
        error?: string;
        injected?: boolean;
        reason?: string | null;
      }>("result", (e) => {
        const ev = {
          type: "result",
          text: e.payload.text,
          error: e.payload.error,
          injected: e.payload.injected,
          reason: e.payload.reason,
        } as const;
        setState((prev) => nextState(prev, ev));
        // 注入成功：文字已落进用户光标处，面板即刻收起（判据与 nextState 共用一处）
        if (injectedOk(ev)) {
          invoke("hide_panel").catch(() => {});
          return;
        }
        // 失败/未注入：结果胶囊停 autoHide 秒。先清旧定时器——连发 result（如无模型
        // 冷启动不走 recording 事件）时，残留的旧 timer 会提前收起新结果
        if (hideTimer.current) clearTimeout(hideTimer.current);
        hideTimer.current = setTimeout(() => {
          invoke("hide_panel").catch(() => {});
          // 窗口隐藏后把面板状态复位到 idle：下次 show 的首帧不会闪上一轮结果
          // （SHOW_PENDING 已删，无人复位的话旧结果冻结在隐藏的 webview 里——三轮 D4）
          setState((prev) => nextState(prev, { type: "recording", active: false }));
        }, autoHide.current * 1000);
      }),
      listen<{ rms: number }>("level", (e) => {
        ampRef.current = e.payload.rms;
      }),
    ];
    return () => unsubs.forEach((u) => u.then((f) => f()));
  }, []);

  const waveActive = state.kind === "recording";
  // 听写期固定布局的相位视图（纯函数映射，断言在 panelState.test.ts）：
  // DOM 结构全程不变，只有属性级变化——声纹起伏在 canvas 内部，状态灯换颜色/文字
  const lamp = lampOf(state.kind);
  const live = lamp.live;

  // 声纹状态提升到 ref：听写三相（唤醒/录音/识别）共用一份条形/AGC 峰值——
  // effect 只以 live 为依赖，不因 waveActive 翻转重跑（重跑会重置条形，停止录音瞬间
  // 一帧瞬跳到底，三轮审查 D3）。跨周期（live 变 false）才清场，下一轮全新相位
  const barsRef = useRef<{ cur: number; ph: number; sp: number }[] | null>(null);
  const peakRef = useRef(0.01); // AGC 滚动峰值：麦克风电平差 50 倍，固定放大系数治不了
  const waveActiveRef = useRef(waveActive);
  waveActiveRef.current = waveActive;

  // 声纹动画：canvas 在整个听写期常驻（live 才渲染），唤醒/识别阶段目标音量为 0——
  // 条形经 dt*14 平滑落回底线（停止说话/停止录音都是落下，不是结构切换）
  useEffect(() => {
    if (!live) {
      barsRef.current = null;
      peakRef.current = 0.01;
      return;
    }
    const cv = canvasRef.current;
    const ctx = cv?.getContext("2d");
    if (!cv || !ctx) return;

    if (!barsRef.current) {
      barsRef.current = Array.from({ length: BARS }, () => ({
        cur: 0.04,
        ph: Math.random() * Math.PI * 2,
        sp: 1.6 + Math.random() * 2.2,
      }));
    }
    const bars = barsRef.current;
    const clamp01 = (v: number) => Math.max(0, Math.min(1, v));
    let raf = 0;
    let t = 0;
    let last = performance.now();

    const tick = (now: number) => {
      const dt = Math.min(0.06, (now - last) / 1000);
      last = now;
      t += dt;
      const W = cv.width;
      const H = cv.height;
      ctx.clearRect(0, 0, W, H);
      // 峰值缓慢衰减（≈每秒 ×0.74），停话后增益自动回落；对滚动峰值归一化，多小的麦都撑满
      peakRef.current = Math.max(peakRef.current * 0.995, ampRef.current, 0.005);
      // 非录音相位（唤醒/识别）目标音量为 0：声纹平滑落底，视觉语义 = 没在收声
      const amp = waveActiveRef.current
        ? clamp01((ampRef.current / peakRef.current) * 0.85)
        : 0;
      const gap = 4;
      const bw = 7; // 2x 画布：屏上 3.5px 条宽
      const total = BARS * bw + (BARS - 1) * gap;
      const x0 = (W - total) / 2;
      const mid = H / 2;
      for (let k = 0; k < BARS; k++) {
        const b = bars[k];
        const env = 0.45 + 0.55 * Math.sin((Math.PI * (k + 0.5)) / BARS); // 中间高两侧低
        const wig = 0.5 + 0.5 * Math.sin(t * b.sp + b.ph);
        const target = clamp01(amp * (0.25 + 0.75 * env * (0.45 + 0.55 * wig)));
        b.cur += (target - b.cur) * Math.min(1, dt * 14); // 平滑起落
        const h = Math.max(2.4, b.cur * H * 0.86);
        const x = x0 + k * (bw + gap);
        ctx.globalAlpha = 0.55 + 0.4 * b.cur;
        ctx.fillStyle = "#c6462f";
        ctx.beginPath();
        if (ctx.roundRect) ctx.roundRect(x, mid - h / 2, bw, h, bw / 2);
        else ctx.rect(x, mid - h / 2, bw, h);
        ctx.fill();
      }
      ctx.globalAlpha = 1;
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [live]);

  // 结果徽标键（纯函数，断言在 panelState.test.ts）：后端 reason → 徽标分支单一事实源
  const badge = state.kind === "result" && !state.error ? badgeKeyOf(state) : null;

  // 内容区：听写期 = 固定布局（声纹 + 状态灯同屏共存）；结果态是唯一一次内容替换
  const content = live ? (
    <div className="live-row">
      <canvas ref={canvasRef} className="wave" width={352} height={88} aria-label="录音声纹" />
      <span className={`live-dot ${lamp.dot}`} aria-label={lamp.label} />
      <span className="live-label">{lamp.label}</span>
    </div>
  ) : state.kind === "result" ? (
      state.error ? (
        <>
          <div className="pill-error">{state.error}</div>
          {state.reason === "no_model" && (
            <button
              className="pill-btn"
              onClick={() => invoke("open_settings", { tab: "model" }).catch(() => {})}
            >
              去模型库下载 →
            </button>
          )}
          {state.reason === "silent_mic" && (
            <button
              className="pill-btn"
              onClick={() => invoke("open_mic_settings").catch(() => {})}
            >
              去系统设置检查麦克风 →
            </button>
          )}
        </>
      ) : (
        <>
          <div className="pill-result-text">{state.text}</div>
          <div className="result-actions">
            {badge === "injected" && <span className="injected-badge">✓ 已插入</span>}
            {badge === "accessibility" && (
              <button
                className="pill-btn warn"
                onClick={() => invoke("open_accessibility_settings").catch(() => {})}
              >
                未插入 · 去开辅助功能权限
              </button>
            )}
            {badge === "clipboard_failed" && (
              <span className="injected-badge miss">未自动插入 · 剪贴板暂不可用，请手动复制</span>
            )}
            {badge === "no_input_focus" && (
              <span className="injected-badge miss">没在输入框 · 文本在剪贴板，可点复制或手动 ⌘V</span>
            )}
            {badge === "focus_unknown" && (
              <span className="injected-badge miss">未确认输入框 · 文本在剪贴板，可点复制或手动 ⌘V</span>
            )}
            {badge === "miss" && <span className="injected-badge miss">未自动插入 · 文本在剪贴板</span>}
            <button
              className="pill-btn"
              onClick={(ev) => {
                invoke("copy_text", { text: state.text ?? "" })
                  .then(() => {
                    ev.currentTarget.textContent = "已复制 ✓";
                  })
                  .catch(() => {});
              }}
            >
              复制
            </button>
          </div>
        </>
      )
    ) : null;

  return (
    <div className="panel-root">
      {/* idle（窗口已隐藏/将隐藏）不渲染胶囊；听写周期内 .pill 恒定不重挂 */}
      {state.kind !== "idle" && <div className="pill">{content}</div>}
    </div>
  );
}
