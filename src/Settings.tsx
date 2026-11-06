// 设置页：浅色清新主题
// 区块：麦克风与权限 -> 识别方式 -> 识别引擎 -> 模型库 ->（占位）快捷键/注入
import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import type { Config, DownloadEvent, EngineStatus, LibraryItem } from "./types";

type Selftest =
  | { kind: "idle" }
  | { kind: "recording" }
  | { kind: "done"; ok: boolean; verdict: string }
  | { kind: "error"; message: string };

// 字节数 -> MB 文本
const fmtMB = (b?: number) => (b ? `${(b / 1024 / 1024).toFixed(0)} MB` : "0 MB");

export default function Settings() {
  const [config, setConfig] = useState<Config | null>(null);
  const [engine, setEngine] = useState<EngineStatus | null>(null);
  const [saved, setSaved] = useState(false);
  const [saveErr, setSaveErr] = useState<string | null>(null);
  const [selftest, setSelftest] = useState<Selftest>({ kind: "idle" });
  const [library, setLibrary] = useState<LibraryItem[]>([]);
  const [dl, setDl] = useState<Record<string, DownloadEvent>>({});
  const [libErr, setLibErr] = useState<string | null>(null);
  const [capturing, setCapturing] = useState(false);
  const cfgRef = useRef<Config | null>(null);
  cfgRef.current = config; // 事件回调里读最新配置用

  const refreshLibrary = () =>
    invoke<LibraryItem[]>("get_model_library")
      .then(setLibrary)
      .catch((e) => setLibErr(String(e)));

  // 点击下载：立即给出可见反馈，出错也亮出来（绝不静默）
  const startDownload = (id: string) => {
    setLibErr(null);
    setDl((prev) => ({ ...prev, [id]: { id, stage: "progress", pct: 0 } }));
    invoke("download_model", { id }).catch((e) => setLibErr(String(e)));
  };

  useEffect(() => {
    invoke<Config>("get_config").then(setConfig);
    invoke<EngineStatus>("detect_engine").then(setEngine);
    refreshLibrary();

    const un1 = listen<{ stage: string; verdict?: string; ok?: boolean; message?: string }>(
      "selftest",
      (e) => {
        const p = e.payload;
        if (p.stage === "recording") setSelftest({ kind: "recording" });
        else if (p.stage === "done")
          setSelftest({ kind: "done", ok: !!p.ok, verdict: p.verdict ?? "" });
        else if (p.stage === "error") setSelftest({ kind: "error", message: p.message ?? "" });
      },
    );
    // 下载进度频道
    const un2 = listen<DownloadEvent>("download-progress", (e) => {
      const p = e.payload;
      setDl((prev) => ({ ...prev, [p.id]: p }));
      if (p.stage === "done" || p.stage === "error" || p.stage === "cancelled") {
        refreshLibrary();
        if (p.stage !== "done") {
          setLibErr(p.message ?? (p.stage === "cancelled" ? "已取消下载" : ""));
        }
      }
    });
    // 快捷键捕获频道：录到的组合直接入库并即时生效
    const un3b = listen<{ key?: string; cancelled?: boolean }>("hotkey-captured", (e) => {
      setCapturing(false);
      const p = e.payload;
      if (p.cancelled || !p.key || p.key === "Space") return;
      const cur = cfgRef.current;
      if (!cur || cur.hotkeys.dictation.includes(p.key)) return;
      const next: Config = {
        ...cur,
        hotkeys: { ...cur.hotkeys, dictation: [...cur.hotkeys.dictation, p.key] },
      };
      setConfig(next);
      invoke("save_config", { config: next }).catch((err) => setSaveErr(String(err)));
    });
    // 窗口隐藏不销毁（✕=隐藏），所以每次重新聚焦时自动拉新数据，避免旧快照
    const un3 = getCurrentWindow().onFocusChanged(({ payload: focus }) => {
      if (focus) {
        refreshLibrary();
        invoke<Config>("get_config").then(setConfig);
        invoke<EngineStatus>("detect_engine").then(setEngine);
      }
    });
    // 兜底轮询：窗口一直开着不动焦点时，数据也会每 2 秒自动保鲜
    const timer = setInterval(refreshLibrary, 2000);
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
      un3.then((f) => f());
      un3b.then((f) => f());
      clearInterval(timer);
    };
  }, []);

  if (!config) return <div className="loading">加载中…</div>;

  const setProvider = (p: Config["stt"]["provider"]) =>
    setConfig({ ...config, stt: { ...config.stt, provider: p } });

  const setCloud = (patch: Partial<Config["stt"]["cloud"]>) =>
    setConfig({ ...config, stt: { ...config.stt, cloud: { ...config.stt.cloud, ...patch } } });

  const save = async () => {
    setSaveErr(null);
    try {
      await invoke("save_config", { config });
      setSaved(true);
      setTimeout(() => setSaved(false), 1500);
      setEngine(await invoke<EngineStatus>("detect_engine"));
    } catch (e) {
      setSaveErr(String(e));
    }
  };

  return (
    <div className="settings">
      <h1>Sayit 设置</h1>

      {/* 麦克风与权限 */}
      <section>
        <h2>麦克风与权限</h2>
        <div className="mic-row">
          <button
            className="primary"
            onClick={() => {
              setSelftest({ kind: "recording" });
              invoke("mic_selftest").catch(() => {});
            }}
          >
            {selftest.kind === "recording" ? "录音中…（说点什么）" : "测试麦克风"}
          </button>
          <button className="ghost" onClick={() => invoke("open_mic_settings").catch(() => {})}>
            打开系统麦克风设置
          </button>
        </div>
        {selftest.kind === "recording" && <p className="muted">录制 1.5 秒分析中…</p>}
        {selftest.kind === "done" && (
          <p className={selftest.ok ? "ok-text" : "warn-text"}>{selftest.verdict}</p>
        )}
        {selftest.kind === "error" && <p className="warn-text">{selftest.message}</p>}
        {selftest.kind === "idle" && (
          <p className="muted">开发模式下，权限弹窗可能挂在启动本应用的工具（终端/ZCode）名下</p>
        )}
      </section>

      {/* 识别方式 */}
      <section>
        <h2>识别方式</h2>
        <div className="radio-cards">
          <label className={config.stt.provider === "local" ? "card selected" : "card"}>
            <input
              type="radio"
              name="provider"
              checked={config.stt.provider === "local"}
              onChange={() => setProvider("local")}
            />
            <div>
              <strong>本地识别</strong>
              <small>免费 · 离线 · 语音不出本机</small>
            </div>
          </label>
          <label
            className={config.stt.provider === "openai_compatible" ? "card selected" : "card"}
          >
            <input
              type="radio"
              name="provider"
              checked={config.stt.provider === "openai_compatible"}
              onChange={() => setProvider("openai_compatible")}
            />
            <div>
              <strong>云端识别</strong>
              <small>OpenAI 兼容接口 · 按量付费</small>
            </div>
          </label>
        </div>

        {config.stt.provider === "openai_compatible" && (
          <div className="cloud-form">
            <label>
              接口地址
              <input
                value={config.stt.cloud.endpoint}
                onChange={(e) => setCloud({ endpoint: e.target.value })}
                placeholder="https://api.openai.com/v1"
              />
            </label>
            <label>
              模型名
              <input
                value={config.stt.cloud.model}
                onChange={(e) => setCloud({ model: e.target.value })}
                placeholder="whisper-1"
              />
            </label>
            <label>
              API Key
              <input
                type="password"
                value={config.stt.cloud.apiKey}
                onChange={(e) => setCloud({ apiKey: e.target.value })}
                placeholder="sk-…"
              />
            </label>
            <label>
              语言
              <input
                value={config.stt.cloud.language}
                onChange={(e) => setCloud({ language: e.target.value })}
                placeholder="zh"
              />
            </label>
          </div>
        )}
      </section>

      {/* 识别引擎 */}
      <section>
        <h2>
          识别引擎
          <button
            className="ghost"
            onClick={() => invoke<EngineStatus>("detect_engine").then(setEngine)}
          >
            重新检测
          </button>
        </h2>
        {engine === null ? (
          <p className="muted">检测中…</p>
        ) : engine.found ? (
          <div className={engine.capable ? "engine-ok" : "engine-warn"}>
            <div>
              来源：
              {engine.source === "settings" && "手动指定"}
              {engine.source === "system" && "系统检测"}
              {engine.source === "managed" && "内置下载"}
            </div>
            <div className="mono">{engine.path}</div>
            <div>
              音频支持检查：
              {engine.capable ? "✓ 通过" : "✗ 版本过旧，建议下载内置版本"}
            </div>
            <div className="muted">{engine.detail}</div>
          </div>
        ) : (
          <div className="engine-warn">
            未找到 llama-server 引擎
            <div className="muted">首次使用时可在引导页一键下载（步骤 3 提供）</div>
          </div>
        )}
      </section>

      {/* 模型库 */}
      <section>
        <h2>
          模型库
          <button
            className="ghost"
            onClick={async () => {
              setLibErr(null);
              try {
                const picked = await open({
                  multiple: true,
                  filters: [{ name: "GGUF 模型文件", extensions: ["gguf"] }],
                });
                if (!picked || picked.length === 0) return;
                const paths = Array.isArray(picked) ? picked : [picked];
                await invoke("add_user_model", { paths });
                refreshLibrary();
              } catch (e) {
                setLibErr(String(e));
              }
            }}
          >
            + 登记本地模型
          </button>
        </h2>
        <p className="muted">
          主模型和 mmproj 文件可一次多选；文件名含 "mmproj" 的会自动配对。登记不复制文件。
        </p>

        {library.map((m) => {
          const p = dl[m.id];
          const downloading = p?.stage === "progress";
          const isDefault = config.stt.localModelId === m.id;
          return (
            <div key={m.id} className="model-item">
              <div className="model-head">
                <div>
                  <strong>
                    {m.name}
                    {m.recommended && <span className="badge rec">推荐</span>}
                    {isDefault && <span className="badge def">默认</span>}
                    {m.source === "user" && <span className="badge user">自带</span>}
                  </strong>
                  <small>
                    {m.description} · {m.sizeHint} · {m.license}
                  </small>
                </div>
                <div className="model-actions">
                  {m.status === "ready" && !isDefault && (
                    <button
                      className="ghost"
                      onClick={async () => {
                        await invoke("set_default_model", { id: m.id });
                        setConfig(await invoke<Config>("get_config"));
                        refreshLibrary();
                      }}
                    >
                      设为默认
                    </button>
                  )}
                  {m.status === "absent" && !downloading && (
                    <button className="primary sm" onClick={() => startDownload(m.id)}>
                      下载
                    </button>
                  )}
                  {m.status === "partial" && !downloading && (
                    <button className="primary sm" onClick={() => startDownload(m.id)}>
                      续传
                    </button>
                  )}
                  {downloading && (
                    <button className="ghost" onClick={() => invoke("cancel_download", { id: m.id })}>
                      取消
                    </button>
                  )}
                  {m.status !== "absent" && !downloading && (
                    <button
                      className="ghost danger"
                      onClick={async () => {
                        await invoke("delete_model", { id: m.id });
                        setConfig(await invoke<Config>("get_config"));
                        refreshLibrary();
                      }}
                    >
                      删除
                    </button>
                  )}
                </div>
              </div>

              {downloading && (
                <div className="dl-progress">
                  <div className="level-track">
                    <div className="level-fill" style={{ width: `${p.pct ?? 0}%` }} />
                  </div>
                  <small className="muted">
                    {p.file}（{p.fileIndex! + 1}/{p.fileCount}）· {fmtMB(p.received)} /{" "}
                    {p.total ? fmtMB(p.total) : "?"} · {p.pct ?? 0}%
                  </small>
                </div>
              )}
              {p?.stage === "error" && <p className="warn-text">{p.message}</p>}
            </div>
          );
        })}
        {libErr && <p className="warn-text">{libErr}</p>}
      </section>
      {/* 快捷键 */}
      <section>
        <h2>
          录音快捷键
          <button
            className={capturing ? "primary sm" : "ghost"}
            onClick={() => {
              setCapturing(!capturing);
              invoke(capturing ? "stop_hotkey_capture" : "start_hotkey_capture").catch(() => {});
            }}
          >
            {capturing ? "录制中…按 Esc 取消" : "+ 录制新快捷键"}
          </button>
        </h2>
        <div className="chips">
          {config.hotkeys.dictation.map((k) => (
            <span key={k} className="chip">
              {k === "Fn" ? "🌐 Fn" : k}
              <button
                className="x"
                onClick={() => {
                  const next: Config = {
                    ...config,
                    hotkeys: {
                      ...config.hotkeys,
                      dictation: config.hotkeys.dictation.filter((x) => x !== k),
                    },
                  };
                  setConfig(next);
                  invoke("save_config", { config: next }).catch((e) => setSaveErr(String(e)));
                }}
              >
                ×
              </button>
            </span>
          ))}
        </div>
        <p className="muted">
          点击「录制」后直接按想要的组合键（支持 ⌘⌃⌥⇧ 修饰符、F1-F12、字母数字，含 🌐
          Fn 键）。多个快捷键可同时生效。
        </p>
        {config.hotkeys.dictation.some((k) => k === "Fn") && (
          <p className="muted">
            启用 Fn 键需把系统设置里「按下 🌐 键时」的行为改为
            <b>无操作</b>
            <button className="ghost" onClick={() => invoke("open_keyboard_settings").catch(() => {})}>
              去设置
            </button>
          </p>
        )}
      </section>
      {/* 文字注入 */}
      <section>
        <h2>文字注入方式</h2>
        <div className="radio-cards">
          <label className={config.injection.mode === "paste" ? "card selected" : "card"}>
            <input
              type="radio"
              name="injection"
              checked={config.injection.mode === "paste"}
              onChange={() =>
                setConfig({ ...config, injection: { ...config.injection, mode: "paste" } })
              }
            />
            <div>
              <strong>剪贴板粘贴</strong>
              <small>兼容性最好 · 用后自动还原原剪贴板</small>
            </div>
          </label>
          <label className={config.injection.mode === "type" ? "card selected" : "card"}>
            <input
              type="radio"
              name="injection"
              checked={config.injection.mode === "type"}
              onChange={() =>
                setConfig({ ...config, injection: { ...config.injection, mode: "type" } })
              }
            />
            <div>
              <strong>直接键入</strong>
              <small>不碰剪贴板 · 少数应用可能不认</small>
            </div>
          </label>
        </div>
        <p className="muted">
          两种方式都需要「辅助功能」权限（模拟键盘事件的前提）。
          <button
            className="ghost"
            onClick={() => invoke("open_accessibility_settings").catch(() => {})}
          >
            去授权
          </button>
        </p>
      </section>

      <footer>
        {saveErr && <span className="warn-text">{saveErr}</span>}
        <button className="primary" onClick={save}>
          {saved ? "已保存 ✓" : "保存设置"}
        </button>
      </footer>
    </div>
  );
}
