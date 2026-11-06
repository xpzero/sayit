// 悬浮面板 · 宣纸文人风
// 录音态 = 纯声纹（无文字）；文字只出现在警告 / 识别中 / 结果
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

type PanelState =
  | { kind: "idle" }
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
  const [silent, setSilent] = useState(false);
  const hideTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const autoHide = useRef(5); // 面板自动收起秒数（启动时从配置读）
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const ampRef = useRef(0); // Rust 每 50ms 推来的 RMS，动画只读它

  useEffect(() => {
    invoke<{ panel: { autoHideSeconds: number } }>("get_config").then((c) => {
      autoHide.current = c.panel.autoHideSeconds;
    });

    const unsubs: Promise<UnlistenFn>[] = [
      listen<{ active: boolean }>("recording", (e) => {
        if (e.payload.active) {
          if (hideTimer.current) clearTimeout(hideTimer.current); // 新录音取消自动收起
          setState({ kind: "recording" });
          setSilent(false);
          ampRef.current = 0;
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
      listen<{ active: boolean }>("silence", (e) => setSilent(e.payload.active)),
    ];
    return () => unsubs.forEach((u) => u.then((f) => f()));
  }, []);

  const waveActive = state.kind === "recording" && !silent;

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

    const tick = (now: number) => {
      const dt = Math.min(0.06, (now - last) / 1000);
      last = now;
      t += dt;
      const W = cv.width;
      const H = cv.height;
      ctx.clearRect(0, 0, W, H);
      const amp = clamp01(ampRef.current * 1.6);
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

  return (
    <div className="panel-root">
      {waveActive && (
        <div className="pill">
          <canvas ref={canvasRef} className="wave" width={352} height={88} aria-label="录音声纹" />
        </div>
      )}

      {state.kind === "recording" && silent && (
        <div className="pill">
          <span className="pill-warn">未听到声音 · 检查麦克风</span>
        </div>
      )}

      {state.kind === "transcribing" && (
        <div className="pill">
          <span className="pill-text">{state.note ?? "识别中…"}</span>
        </div>
      )}

      {state.kind === "result" && (
        <div className="pill result">
          {state.error ? (
            <div className="pill-error">{state.error}</div>
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
                  <span className="injected-badge miss">未自动插入</span>
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
      )}
    </div>
  );
}
