// 悬浮面板：纯"事件驱动"组件——自己没有任何业务逻辑，
// 全部状态由 Rust 广播的事件推进（第 7 讲的设计）
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

type PanelState =
  | { kind: "idle" }
  | { kind: "recording"; model: string; startedAt: number }
  | { kind: "transcribing"; note?: string }
  | {
      kind: "result";
      text?: string;
      error?: string;
      injected?: boolean | null;
      reason?: string | null;
    };

// 声波包络：24 根条纹各自的放大系数（中间高、两侧低的伪随机节奏）
const WAVE_SHAPE = [
  0.4, 0.7, 0.5, 0.9, 0.6, 1, 0.7, 0.85, 0.5, 0.95, 0.65, 1, 0.8, 0.55, 0.9, 0.7, 1, 0.6,
  0.85, 0.5, 0.75, 0.45, 0.65, 0.4,
];

export default function Panel() {
  const [state, setState] = useState<PanelState>({ kind: "idle" });
  const [level, setLevel] = useState(0);
  const [elapsed, setElapsed] = useState(0);
  const [silent, setSilent] = useState(false);
  const hideTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const autoHide = useRef(5); // 面板自动收起秒数（启动时从配置读）

  useEffect(() => {
    invoke<{ panel: { autoHideSeconds: number } }>("get_config").then((c) => {
      autoHide.current = c.panel.autoHideSeconds;
    });

    // 四条广播频道，全部注册，组件卸载时退订
    const unsubs: Promise<UnlistenFn>[] = [
      listen<{ active: boolean; model: string }>("recording", (e) => {
        if (e.payload.active) {
          if (hideTimer.current) clearTimeout(hideTimer.current); // 新录音取消自动收起
          setState({ kind: "recording", model: e.payload.model, startedAt: Date.now() });
          setElapsed(0);
          setSilent(false);
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
        // 注入成功：文字已落进用户光标处，面板不再打扰，即刻收起
        if (!e.payload.error && e.payload.injected === true) {
          setState({ kind: "idle" });
          invoke("hide_panel").catch(() => {});
          return;
        }
        // 失败/未注入/报错：面板保留（复制按钮兜底），到点自动收起
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
      listen<{ rms: number }>("level", (e) => setLevel(e.payload.rms)),
      listen<{ active: boolean }>("silence", (e) => setSilent(e.payload.active)),
    ];
    return () => unsubs.forEach((u) => u.then((f) => f()));
  }, []);

  // 计时归前端管：录音态每秒走表，其他态不动
  useEffect(() => {
    if (state.kind !== "recording") return;
    const t = setInterval(() => {
      setElapsed(Math.floor((Date.now() - state.startedAt) / 1000));
    }, 1000);
    return () => clearInterval(t);
  }, [state]);

  const mm = String(Math.floor(elapsed / 60)).padStart(2, "0");
  const ss = String(elapsed % 60).padStart(2, "0");

  return (
    <div className="panel-root">
      {state.kind === "recording" && (
        <div className="capsule">
          <div className="capsule-main">
            <span className="rec-dot" />
            <span className="timer">
              {mm}:{ss}
            </span>
            <span className={silent ? "model silent-warn" : "model"}>
              {silent ? "未听到声音 · 检查麦克风" : state.model}
            </span>
          </div>
          <div className="wave" aria-hidden="true">
            {WAVE_SHAPE.map((f, i) => (
              <i
                key={i}
                style={{ transform: `scaleY(${Math.max(0.06, Math.min(1, level * 1.6 * f))})` }}
              />
            ))}
          </div>
        </div>
      )}

      {state.kind === "transcribing" && (
        <div className="capsule">
          <div className="capsule-main">
            <span className="wave eq" aria-hidden="true">
              <i />
              <i />
              <i />
              <i />
              <i />
            </span>
            <span className="hint">{state.note ?? "识别中…"}</span>
          </div>
        </div>
      )}

      {state.kind === "result" && (
        <div className="capsule result">
          {state.error ? (
            <div className="error-text">{state.error}</div>
          ) : (
            <>
              <div className="result-text">{state.text}</div>
              <div className="result-actions">
                {state.injected ? (
                  <span className="injected-badge">✓ 已插入</span>
                ) : state.reason === "accessibility" ? (
                  <button
                    className="panel-btn warn"
                    onClick={() => invoke("open_accessibility_settings").catch(() => {})}
                  >
                    未插入 · 去开辅助功能权限
                  </button>
                ) : (
                  <span className="injected-badge miss">未自动插入</span>
                )}
                <button
                  className="panel-btn"
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
