// 使用页：快捷键录制 + 文字落位与面板
import { invoke } from "@tauri-apps/api/core";
import type { Config } from "../types";
import { Ico } from "./brand";

export function UsePane(p: {
  config: Config;
  update: (c: Config) => void;
  capturing: boolean;
  setCapturing: (v: boolean) => void;
  preview: string | null;
  setPreview: (v: string | null) => void;
  hkInputRef: React.RefObject<HTMLInputElement | null>;
  onHkKeyDown: (e: React.KeyboardEvent<HTMLInputElement>) => void;
  removeHotkey: (k: string) => void;
}) {
  const { config, update, capturing, setCapturing, preview, setPreview, hkInputRef, onHkKeyDown, removeHotkey } = p;
  return (
    <>
          <h2 className="sect">使用</h2>
          <p className="desc">怎么触发（快捷键）、文字怎么落（注入）、过程看见什么（面板）。</p>
          <div className="card">
            <h3>{Ico.key} 录音开关</h3>
            <div
              className={capturing ? "hk-capture on" : "hk-capture"}
              onClick={() => hkInputRef.current?.focus()}
              onContextMenu={(e) => {
                // 录制态吞掉右键菜单弹出（Menu 键的录入由后端键码轮询负责，不走 DOM 事件）
                if (capturing) e.preventDefault();
              }}
            >
              {config.hotkeys.dictation.map((k) => (
                <span key={k} className="chip">
                  {k === "Fn" ? "🌐 Fn" : k}
                  <button className="x" onClick={() => removeHotkey(k)}>
                    ×
                  </button>
                </span>
              ))}
              <input
                ref={hkInputRef}
                className="hk-input"
                readOnly
                value={preview ?? ""}
                placeholder={
                  capturing ? "按下组合、抬起即录入 · Esc 退出 · ⌫ 删末尾" : "点击此处录制快捷键"
                }
                onFocus={() => {
                  setCapturing(true);
                  invoke("start_hotkey_capture").catch(() => {});
                }}
                onBlur={() => {
                  setCapturing(false);
                  setPreview(null);
                  invoke("stop_hotkey_capture").catch(() => {});
                }}
                onKeyDown={onHkKeyDown}
              />
            </div>
            <p className="muted small-top">
              修饰符（⌘⌃⌥⇧）+ 任意键：字母数字（A 除外——系统键码怪癖，录不进去）、F1-F20、Home / End / PgUp / PgDn、Ins /
              Del、小键盘、方向键、反引号；🌐 Fn 与 PC 键盘的 Menu
              键单独点按录入。系统保留组合（⌘Tab 等）无法捕获。
              注册的快捷键为<b>全局独占</b>：按下不会再传递给其他应用。
            </p>
          </div>

          {config.hotkeys.dictation.some((k) => k === "Fn") && (
            <div className="card">
              <h3>🌐 Fn 键使用说明</h3>
              <p className="muted">
                需把 系统设置 → 键盘 →「按下 🌐 键时」改为
                <b>无操作</b>
                <button
                  className="link"
                  onClick={() => invoke("open_keyboard_settings").catch(() => {})}
                >
                  去设置
                </button>
                ；之后 Fn 就是纯粹的录音开关，拇指一按即说。
              </p>
            </div>
          )}

          <div className="card top14">
            <h3>{Ico.type} 文字落位与面板</h3>
            <div className="grid2">
              <label className={config.injection.mode === "paste" ? "opt on" : "opt"}>
                <input
                  type="radio"
                  name="injection"
                  checked={config.injection.mode === "paste"}
                  onChange={() =>
                    update({ ...config, injection: { ...config.injection, mode: "paste" } })
                  }
                />
                <div>
                  <b>剪贴板粘贴</b>
                  <small>兼容性最好 · 注入时占用剪贴板（⌘V 机制）</small>
                </div>
              </label>
              <label className={config.injection.mode === "type" ? "opt on" : "opt"}>
                <input
                  type="radio"
                  name="injection"
                  checked={config.injection.mode === "type"}
                  onChange={() =>
                    update({ ...config, injection: { ...config.injection, mode: "type" } })
                  }
                />
                <div>
                  <b>直接键入</b>
                  <small>逐字键入 · 不碰剪贴板，落空在历史里取回</small>
                </div>
              </label>
            </div>
            <div className="field top10">
              <label>结果面板自动收起（秒）</label>
              <input
                type="number"
                min={1}
                max={60}
                value={config.panel.autoHideSeconds}
                onChange={(e) =>
                  update({
                    ...config,
                    panel: {
                      ...config.panel,
                      autoHideSeconds: Math.max(1, Number(e.target.value) || 5),
                    },
                  })
                }
              />
            </div>
            <p className="muted small-top">
              两种注入都需要「辅助功能」权限（顶部体检条可查）。
            </p>
          </div>
    </>
  );
}
