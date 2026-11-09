// 悬浮面板 · 宣纸文人风
// 录音态 = 纯声纹（无文字）；文字只出现在唤醒中 / 警告 / 识别中 / 结果
import { useEffect, useRef, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

type PanelState =
  | { kind: "idle" }
  | { kind: "opening" } // 开麦进行中（设备唤醒可到秒级）：加载动效，正式开录才亮声纹
  | { kind: "recording" }
  | { kind: "transcribing"; note?: string }
  | {
      kind: "result";
      text?: string;
      error?: string;
      injected?: boolean | null;
      reason?: string | null;
    };

const BARS = 30;

export default function Panel() {
  const [state, setState] = useState<PanelState>({ kind: "idle" });
  const hideTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const autoHide = useRef(5); // 面板自动收起秒数（启动时从配置读）
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const ampRef = useRef(0); // Rust 每 50ms 推来的 RMS，动画只读它
  const rootRef = useRef<HTMLDivElement | null>(null);

  // 窗口贴合内容：胶囊尺寸一变就把窗口缩放到同尺寸（Rust 侧保持底部居中锚定）。
  // 空态（无胶囊）不上报——保持上一个内容尺寸，窗口不至于缩没。
  useEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    let last = "";
    const ro = new ResizeObserver(() => {
      if (!el.querySelector(".pill")) return;
      const w = Math.ceil(el.offsetWidth);
      const h = Math.ceil(el.offsetHeight);
      const key = `${w}x${h}`;
      if (key === last) return;
      last = key;
      invoke("resize_panel", { w, h }).catch(() => {});
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useEffect(() => {
    invoke<{ panel: { autoHideSeconds: number } }>("get_config").then((c) => {
      autoHide.current = c.panel.autoHideSeconds;
    });

    const unsubs: Promise<UnlistenFn>[] = [
      listen<{ active: boolean; opening?: boolean }>("recording", (e) => {
        if (e.payload.active) {
          if (hideTimer.current) clearTimeout(hideTimer.current); // 新录音取消自动收起
          ampRef.current = 0;
          // opening = 设备唤醒中，先给加载态；正式开录事件（不带 opening）再来时切成声纹
          setState(e.payload.opening ? { kind: "opening" } : { kind: "recording" });
        } else {
          setState({ kind: "idle" }); // Esc 中止等场景：复位，避免声纹冻结在屏幕上
        }
      }),
      listen<{ stage: string; note?: string }>("phase", (e) => {
        if (e.payload.stage === "transcribing")
          setState({ kind: "transcribing", note: e.payload.note });
      }),
      listen<{
        text?: string;
        error?: string;
        injected?: boolean;
        reason?: string | null;
      }>("result", (e) => {
        // 注入成功：文字已落进用户光标处，面板即刻收起
        if (!e.payload.error && e.payload.injected === true) {
          setState({ kind: "idle" });
          invoke("hide_panel").catch(() => {});
          return;
        }
        setState({
          kind: "result",
          text: e.payload.text,
          error: e.payload.error,
          injected: e.payload.injected,
          reason: e.payload.reason,
        });
        hideTimer.current = setTimeout(() => {
          invoke("hide_panel").catch(() => {});
        }, autoHide.current * 1000);
      }),
      listen<{ rms: number }>("level", (e) => {
        ampRef.current = e.payload.rms;
      }),
    ];
    return () => unsubs.forEach((u) => u.then((f) => f()));
  }, []);

  const waveActive = state.kind === "recording";

  // 状态切换的残影：旧胶囊保留 150ms 绝对定位淡出（不参与布局与尺寸上报，窗口只认
  // flow 里的新胶囊），与新胶囊的 pop-in 叠成柔和的交叉溶解——旧节点直接卸载会
  // "啪"地消失，这正是状态切换生硬感的来源
  const [ghost, setGhost] = useState<ReactNode>(null);
  const prevPillRef = useRef<{ kind: PanelState["kind"]; node: ReactNode } | null>(null);

  // 声纹动画：canvas 挂载期间运行（录音态才渲染 canvas，故以 waveActive 为依赖）
  useEffect(() => {
    if (!waveActive) return;
    const cv = canvasRef.current;
    const ctx = cv?.getContext("2d");
    if (!cv || !ctx) return;

    const bars = Array.from({ length: BARS }, () => ({
      cur: 0.04,
      ph: Math.random() * Math.PI * 2,
      sp: 1.6 + Math.random() * 2.2,
    }));
    const clamp01 = (v: number) => Math.max(0, Math.min(1, v));
    let raf = 0;
    let t = 0;
    let last = performance.now();
    let peak = 0.01; // AGC 滚动峰值：麦克风电平差 50 倍（耳机 0.1 vs 笔记本 0.007），固定放大系数治不了

    const tick = (now: number) => {
      const dt = Math.min(0.06, (now - last) / 1000);
      last = now;
      t += dt;
      const W = cv.width;
      const H = cv.height;
      ctx.clearRect(0, 0, W, H);
      // 峰值缓慢衰减（≈每秒 ×0.74），停话后增益自动回落；对滚动峰值归一化，多小的麦都撑满
      peak = Math.max(peak * 0.995, ampRef.current, 0.005);
      const amp = clamp01((ampRef.current / peak) * 0.85);
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
  }, [waveActive]);

  const pill: ReactNode =
    (state.kind === "opening" && (
      <div className="pill">
        <span className="thinking" aria-label="正在唤醒麦克风">
          <i />
          <i />
          <i />
        </span>
        <span className="pill-text">正在唤醒麦克风…</span>
      </div>
    )) ||
    (waveActive && (
      <div className="pill">
        <canvas ref={canvasRef} className="wave" width={352} height={88} aria-label="录音声纹" />
      </div>
    )) ||
    (state.kind === "transcribing" && (
      <div className="pill">
        <span className="thinking" aria-label="识别中">
          <i />
          <i />
          <i />
        </span>
        {/* 冷启动等罕见场景仍保留说明文字（平时只有动效） */}
        {state.note && <span className="pill-text">{state.note}</span>}
      </div>
    )) ||
    (state.kind === "result" && (
        <div className="pill result">
          {state.error ? (
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
                {state.injected ? (
                  <span className="injected-badge">✓ 已插入</span>
                ) : state.reason === "accessibility" ? (
                  <button
                    className="pill-btn warn"
                    onClick={() => invoke("open_accessibility_settings").catch(() => {})}
                  >
                    未插入 · 去开辅助功能权限
                  </button>
                ) : (
                  <span className="injected-badge miss">未自动插入 · 文本在剪贴板</span>
                )}
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
          )}
        </div>
    ));

  // 状态一变就把上一帧的胶囊立为残影（150ms 后自动撤除）；同态的内容更新则清掉旧残影
  useEffect(() => {
    const prev = prevPillRef.current;
    prevPillRef.current = { kind: state.kind, node: pill };
    if (prev && prev.kind !== state.kind && prev.node) {
      setGhost(prev.node);
      const t = setTimeout(() => setGhost(null), 150);
      return () => clearTimeout(t);
    }
    setGhost(null);
  }, [state]);

  return (
    <div className="panel-root" ref={rootRef}>
      {/* 残影先渲染垫底，新胶囊盖上——交叉溶解由此而来 */}
      {ghost !== null && <div className="pill-ghost">{ghost}</div>}
      {pill}
    </div>
  );
}
