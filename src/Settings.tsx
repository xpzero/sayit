// 设置页 · 宣纸文人风 · 侧栏六 Tab
// 录音 / 模型库 / 快捷键 / 云端 / 权限 / 关于 —— 单页不滚动，各自独立
import { useEffect, useRef, useState, type ReactElement } from "react";
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

type Tab = "rec" | "model" | "key" | "cloud" | "perm" | "about";

// 字节数 -> MB 文本
const fmtMB = (b?: number) => (b ? `${(b / 1024 / 1024).toFixed(0)} MB` : "0 MB");

// 品牌标识：与应用图标同款 E「墨滴字」—— 米纸底 + 手写墨字 + 朱砂一点
const Logo = ({ size = 32 }: { size?: number }) => (
  <svg width={size} height={size} viewBox="0 0 64 64" aria-hidden="true">
    <rect x="2" y="2" width="60" height="60" rx="14" fill="#FDF6EC" stroke="#E8DDC6" strokeWidth="2" />
    <text
      x="27"
      y="44"
      textAnchor="middle"
      fontFamily="'Bradley Hand', 'Chalkboard SE', cursive"
      fontWeight="700"
      fontSize="28"
      fill="#171A20"
    >
      sa
    </text>
    <circle cx="45" cy="42" r="4.2" fill="#C6462F" />
  </svg>
);

// 图标（16px 线性，stroke=currentColor）
const Ico = {
  mic: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <rect x="9" y="2" width="6" height="12" rx="3" />
      <path d="M5 10v1a7 7 0 0 0 14 0v-1" />
      <path d="M12 18v4" />
    </svg>
  ),
  wave: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <path d="M3 12h2M8 8v8M13 5v14M18 9v6M22 12h-1" />
    </svg>
  ),
  cpu: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <rect x="6" y="6" width="12" height="12" rx="2" />
      <rect x="10" y="10" width="4" height="4" />
      <path d="M12 2v3M12 19v3M2 12h3M19 12h3M5 5l2 2M17 17l2 2M19 5l-2 2M7 17l-2 2" />
    </svg>
  ),
  box: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M21 8l-9-5-9 5v8l9 5 9-5V8z" />
      <path d="M3 8l9 5 9-5M12 13v8" />
    </svg>
  ),
  key: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <rect x="2" y="6" width="20" height="12" rx="2" />
      <path d="M6 10h.01M10 10h.01M14 10h.01M18 10h.01M6 14h12" />
    </svg>
  ),
  type: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M4 7V5h16v2M9 5v14M7 19h4M14 13l3 6 3-6" />
    </svg>
  ),
  cloud: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M17.5 19a4.5 4.5 0 0 0 .4-9A7 7 0 0 0 4.3 12.4 4 4 0 0 0 6 19.9z" />
    </svg>
  ),
  shield: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 22s8-3.5 8-10V5l-8-3-8 3v7c0 6.5 8 10 8 10z" />
      <path d="M9 12l2 2 4-4" />
    </svg>
  ),
  info: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <circle cx="12" cy="12" r="10" />
      <path d="M12 16v-4M12 8h.01" />
    </svg>
  ),
  sliders: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <path d="M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6" />
    </svg>
  ),
};

const TABS: { id: Tab; label: string; icon: ReactElement }[] = [
  { id: "rec", label: "录音", icon: Ico.mic },
  { id: "model", label: "模型库", icon: Ico.box },
  { id: "key", label: "快捷键", icon: Ico.key },
  { id: "cloud", label: "云端", icon: Ico.cloud },
  { id: "perm", label: "权限", icon: Ico.shield },
  { id: "about", label: "关于", icon: Ico.info },
];

export default function Settings() {
  const [tab, setTab] = useState<Tab>("rec");
  const [config, setConfig] = useState<Config | null>(null);
  const [engine, setEngine] = useState<EngineStatus | null>(null);
  const [saveErr, setSaveErr] = useState<string | null>(null);
  const [selftest, setSelftest] = useState<Selftest>({ kind: "idle" });
  const [library, setLibrary] = useState<LibraryItem[]>([]);
  const [dl, setDl] = useState<Record<string, DownloadEvent>>({});
  const [libErr, setLibErr] = useState<string | null>(null);
  const [capturing, setCapturing] = useState(false); // 快捷键输入框聚焦 = 录制态
  const [preview, setPreview] = useState<string | null>(null); // keydown 实时预览的组合
  const [axOk, setAxOk] = useState<boolean | null>(null);
  const cfgRef = useRef<Config | null>(null);
  cfgRef.current = config; // 事件回调里读最新配置用
  const hkInputRef = useRef<HTMLInputElement>(null);
  // 事件监听（挂载时注册一次）→ 组件内最新函数的桥
  const addHotkeyRef = useRef<((k: string) => void) | null>(null);
  const removeHotkeyRef = useRef<((k: string) => void) | null>(null);

  // 所有设置改动都走这里：改内存即落盘（后端增量同步快捷键，无变化时空跑）
  const update = (next: Config) => {
    setConfig(next);
    setSaveErr(null); // 成功路径清旧错；失败会在 catch 里重新亮起
    invoke("save_config", { config: next }).catch((e) => setSaveErr(String(e)));
  };

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
    invoke<boolean>("accessibility_status").then(setAxOk);
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
    // 后端 tap 的捕获事件：keyup 提交组合 / Fn 与 Menu 令牌；cancelled = Esc 退出
    const un3b = listen<{ key?: string; cancelled?: boolean }>("hotkey-captured", (e) => {
      if (e.payload.cancelled) return;
      const key = e.payload.key;
      if (key) addHotkeyRef.current?.(key);
    });
    // keyDown 实时预览（修饰符 + 主键的组合名由后端拼好）
    const un3c = listen<{ key?: string }>("hotkey-preview", (e) => {
      if (e.payload.key) setPreview(e.payload.key);
    });
    // ⌫ 删末尾（后端 tap 在捕获态转发）
    const un3d = listen("hotkey-remove-last", () => {
      const cur = cfgRef.current;
      if (cur && cur.hotkeys.dictation.length > 0)
        removeHotkeyRef.current?.(cur.hotkeys.dictation[cur.hotkeys.dictation.length - 1]);
    });
    // 深链直达某个 tab（如"未配置模型"面板按钮 → 模型库）
    const un4 = listen<{ tab?: string }>("goto-tab", (e) => {
      const t = e.payload.tab as Tab | undefined;
      if (t) setTab(t);
    });
    // 窗口隐藏不销毁（✕=隐藏），所以每次重新聚焦时自动拉新数据，避免旧快照
    const un3 = getCurrentWindow().onFocusChanged(({ payload: focus }) => {
      if (focus) {
        refreshLibrary();
        invoke<Config>("get_config").then(setConfig);
        invoke<EngineStatus>("detect_engine").then(setEngine);
        invoke<boolean>("accessibility_status").then(setAxOk);
      } else {
        // 窗口失焦必停捕获：否则焦点去了别的应用还在打字，每个裸键都会被录进配置
        setCapturing(false);
        setPreview(null);
        invoke("stop_hotkey_capture").catch(() => {});
      }
    });
    // 兜底轮询：窗口一直开着不动焦点时，数据也会每 2 秒自动保鲜
    const timer = setInterval(refreshLibrary, 2000);
    return () => {
      un1.then((f) => f());
      un2.then((f) => f());
      un3.then((f) => f());
      un3b.then((f) => f());
      un3c.then((f) => f());
      un3d.then((f) => f());
      un4.then((f) => f());
      clearInterval(timer);
    };
  }, []);

  if (!config) return <div className="loading">加载中…</div>;

  const setProvider = (p: Config["stt"]["provider"]) =>
    update({ ...config, stt: { ...config.stt, provider: p } });

  const setCloud = (patch: Partial<Config["stt"]["cloud"]>) =>
    update({ ...config, stt: { ...config.stt, cloud: { ...config.stt.cloud, ...patch } } });

  // ---------- 快捷键输入框 ----------
  // 捕获逻辑全在后端（CGEventTap：keydown 预览 / keyup 提交 / Fn、Menu 轮询兜底），
  // 前端只负责焦点开关、Esc 退出和事件展示。

  const addHotkey = (combo: string) => {
    if (combo === "Space") {
      setPreview("Space 需要加修饰键"); // 与后端方言同规则
      setTimeout(() => setPreview(null), 1500);
      return;
    }
    if (config.hotkeys.dictation.some((k) => k.toLowerCase() === combo.toLowerCase())) return;
    update({
      ...config,
      hotkeys: { ...config.hotkeys, dictation: [...config.hotkeys.dictation, combo] },
    });
  };

  const removeHotkey = (key: string) =>
    update({
      ...config,
      hotkeys: {
        ...config.hotkeys,
        dictation: config.hotkeys.dictation.filter((x) => x !== key),
      },
    });
  addHotkeyRef.current = addHotkey;
  removeHotkeyRef.current = removeHotkey;

  // 输入框只做两件事：吞掉一切默认行为 + Esc 退出录制（其余按键全由后端 tap 处理）
  const onHkKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    e.preventDefault(); // IME、Tab 焦点转移都不会发生
    if (e.code === "Escape") hkInputRef.current?.blur();
  };

  const defaultModel = library.find((m) => m.id === config.stt.localModelId);

  return (
    <div className="shell">
      <header className="shell-head">
        <div className="brand">
          <Logo />
          <div>
            <div className="brand-name">Sayit</div>
            <div className="brand-sub">说就写了 · 本地优先的语音输入</div>
          </div>
        </div>
        <div className="head-actions">
          {saveErr && (
            <span className="save-err" title={saveErr}>
              {saveErr}
            </span>
          )}
        </div>
      </header>

      <div className="shell-body">
        <nav className="rail" aria-label="设置分区">
          {TABS.map((t) => (
            <button
              key={t.id}
              className={tab === t.id ? "rail-btn on" : "rail-btn"}
              onClick={() => setTab(t.id)}
            >
              {t.icon}
              <span>{t.label}</span>
            </button>
          ))}
          <span className="rail-spacer" />
          <span className="rail-ver">Sayit v0.1.0</span>
        </nav>

        <main className="pane">
          {/* ============ 录音 ============ */}
          {tab === "rec" && (
            <>
              <h2 className="sect">识别方式</h2>
              <p className="desc">本地识别免费离线；云端需要密钥、按量付费。两者随时可切换。</p>
              <div className="grid2">
                <label className={config.stt.provider === "local" ? "opt on" : "opt"}>
                  <input
                    type="radio"
                    name="provider"
                    checked={config.stt.provider === "local"}
                    onChange={() => setProvider("local")}
                  />
                  <div>
                    <b>本地识别</b>
                    <small>免费 · 离线 · 语音不出本机</small>
                  </div>
                  {config.stt.provider === "local" && <span className="stamp">默认</span>}
                </label>
                <label
                  className={config.stt.provider === "openai_compatible" ? "opt on" : "opt"}
                >
                  <input
                    type="radio"
                    name="provider"
                    checked={config.stt.provider === "openai_compatible"}
                    onChange={() => setProvider("openai_compatible")}
                  />
                  <div>
                    <b>云端识别</b>
                    <small>OpenAI 兼容 · 按量付费 · 在「云端」页配置</small>
                  </div>
                </label>
              </div>

              <div className="grid2 top14">
                <div className="card">
                  <h3>{Ico.cpu} 当前模型</h3>
                  <div className="model-item single">
                    <div className="model-head">
                      <div>
                        <b>{defaultModel?.name ?? "未选择"}</b>
                        <small>
                          {defaultModel
                            ? `${defaultModel.description} · ${defaultModel.sizeHint} · ${defaultModel.status === "ready" ? "已就绪" : "未就绪"}`
                            : "去「模型库」挑选或登记一个模型"}
                        </small>
                      </div>
                      <button className="btn" onClick={() => setTab("model")}>
                        更换
                      </button>
                    </div>
                  </div>
                  <p className="muted small-top">
                    引擎：
                    {engine?.found
                      ? `llama.cpp（${engine.source === "system" ? "系统检测" : engine.source === "settings" ? "手动指定" : "内置"} · ${engine.capable ? "能力检查通过" : "版本过旧"}）`
                      : "未找到 llama-server"}
                  </p>
                </div>
                <div className="card">
                  <h3>{Ico.sliders} 录音质量</h3>
                  <div className="field">
                    <label>采样率（识别模型标准）</label>
                    <select disabled>
                      <option>16,000 Hz · 单声道 · 16bit</option>
                    </select>
                  </div>
                  <div className="field top10">
                    <label>面板自动收起（秒）</label>
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
                </div>
              </div>

              <div className="card top14">
                <h3>{Ico.type} 文字注入方式</h3>
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
                      <small>兼容性最好 · 用后自动还原原剪贴板</small>
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
                      <small>不碰剪贴板 · 少数应用可能不认</small>
                    </div>
                  </label>
                </div>
                <p className="muted small-top">
                  两种方式都需要「辅助功能」权限。
                  <button
                    className="link"
                    onClick={() => invoke("open_accessibility_settings").catch(() => {})}
                  >
                    去授权
                  </button>
                </p>
              </div>
            </>
          )}

          {/* ============ 模型库 ============ */}
          {tab === "model" && (
            <>
              <h2 className="sect">模型库</h2>
              <p className="desc">
                精选模型点击下载（断点续传）；也可以登记自己已有的 GGUF 文件。
              </p>
              <div className="card">
                <h3>
                  {Ico.box} 精选 · 下载即用
                  <button
                    className="btn"
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
                    ＋ 登记本地模型
                  </button>
                </h3>
                {library.map((m) => {
                  const p = dl[m.id];
                  const downloading = p?.stage === "progress";
                  const isDefault = config.stt.localModelId === m.id;
                  return (
                    <div
                      key={m.id}
                      className={isDefault && m.status === "ready" ? "model-item on" : "model-item"}
                    >
                      <div className="model-head">
                        <div>
                          <b>
                            {m.name}
                            {m.recommended && <span className="badge rec">推荐</span>}
                            {isDefault && <span className="badge def">默认</span>}
                            {m.source === "user" && <span className="badge user">自带</span>}
                          </b>
                          <small>
                            {m.description} · {m.sizeHint} · {m.license}
                          </small>
                        </div>
                        <div className="model-actions">
                          {m.status === "ready" && !isDefault && (
                            <button
                              className="btn"
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
                            <button className="btn red" onClick={() => startDownload(m.id)}>
                              下载
                            </button>
                          )}
                          {m.status === "partial" && !downloading && (
                            <button className="btn green" onClick={() => startDownload(m.id)}>
                              续传
                            </button>
                          )}
                          {downloading && (
                            <button
                              className="btn"
                              onClick={() => invoke("cancel_download", { id: m.id })}
                            >
                              取消
                            </button>
                          )}
                          {m.status !== "absent" && !downloading && (
                            <button
                              className="btn danger"
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
              </div>
              <p className="muted">
                登记不复制文件：主模型和 mmproj 可一次多选，文件名含 "mmproj" 的会自动配对。
              </p>
            </>
          )}

          {/* ============ 快捷键 ============ */}
          {tab === "key" && (
            <>
              <h2 className="sect">快捷键</h2>
              <p className="desc">开始/结束录音共用一个键；一个功能可绑多个快捷键。</p>
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
                  修饰符（⌘⌃⌥⇧）+ 任意键：字母数字、F1-F20、Home / End / PgUp / PgDn、Ins /
                  Del、小键盘、方向键、反引号；🌐 Fn 与 PC 键盘的 Menu
                  键单独点按录入。系统保留组合（⌘Tab 等）无法捕获。
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
            </>
          )}

          {/* ============ 云端 ============ */}
          {tab === "cloud" && (
            <>
              <h2 className="sect">云端识别</h2>
              <p className="desc">
                任何 OpenAI 兼容接口：OpenAI / 硅基流动 / Groq / 本地 faster-whisper 服务。
                修改即保存。
              </p>
              <div className="grid2">
                <div className="card">
                  <div className="field">
                    <label>接口地址（Base URL）</label>
                    <input
                      value={config.stt.cloud.endpoint}
                      onChange={(e) => setCloud({ endpoint: e.target.value })}
                      placeholder="https://api.siliconflow.cn/v1"
                    />
                  </div>
                  <div className="field top10">
                    <label>模型名</label>
                    <input
                      value={config.stt.cloud.model}
                      onChange={(e) => setCloud({ model: e.target.value })}
                      placeholder="FunAudioLLM/SenseVoiceSmall"
                    />
                  </div>
                </div>
                <div className="card">
                  <div className="field">
                    <label>API Key</label>
                    <input
                      type="password"
                      value={config.stt.cloud.apiKey}
                      onChange={(e) => setCloud({ apiKey: e.target.value })}
                      placeholder="sk-…"
                    />
                  </div>
                  <div className="field top10">
                    <label>语言提示</label>
                    <input
                      value={config.stt.cloud.language}
                      onChange={(e) => setCloud({ language: e.target.value })}
                      placeholder="zh / en / 留空自动"
                    />
                  </div>
                </div>
              </div>
              <p className="muted top10">
                密钥仅保存在本机（v1.2 迁移至钥匙串）；切回「本地识别」随时可用离线引擎。
              </p>
            </>
          )}

          {/* ============ 权限 ============ */}
          {tab === "perm" && (
            <>
              <h2 className="sect">权限体检</h2>
              <p className="desc">macOS 的两道门：录音要麦克风，自动打字要辅助功能。</p>
              <div className="card">
                <div className="perm">
                  <span className={selftest.kind === "done" && selftest.ok ? "st ok" : "st no"}>
                    {selftest.kind === "done" && selftest.ok ? "✓" : "!"}
                  </span>
                  <div>
                    <b>麦克风</b>
                    <small>
                      {selftest.kind === "done"
                        ? selftest.ok
                          ? "工作正常"
                          : "自检未通过"
                        : "点右侧按钮录 1.5 秒自检"}
                    </small>
                  </div>
                  <button
                    className="btn"
                    onClick={() => {
                      setSelftest({ kind: "recording" });
                      invoke("mic_selftest").catch(() => {});
                    }}
                  >
                    {selftest.kind === "recording" ? "录音中…" : "测试麦克风"}
                  </button>
                  <button
                    className="btn"
                    onClick={() => invoke("open_mic_settings").catch(() => {})}
                  >
                    去设置
                  </button>
                </div>
                {selftest.kind === "done" && (
                  <p className={selftest.ok ? "ok-text" : "warn-text"}>{selftest.verdict}</p>
                )}
                {selftest.kind === "error" && <p className="warn-text">{selftest.message}</p>}

                <div className="perm">
                  <span className={axOk ? "st ok" : "st no"}>{axOk ? "✓" : "!"}</span>
                  <div>
                    <b>辅助功能</b>
                    <small>
                      {axOk === null
                        ? "查询中…"
                        : axOk
                          ? "已授权 · 文字可自动注入光标处"
                          : "未授权 · 注入会被系统静默丢弃"}
                    </small>
                  </div>
                  <button
                    className={axOk ? "btn" : "btn red"}
                    onClick={() => invoke("open_accessibility_settings").catch(() => {})}
                  >
                    去授权
                  </button>
                </div>

                <div className="perm">
                  <span
                    className={
                      config.hotkeys.dictation.some((k) => k === "Fn") ? "st ok" : "st no"
                    }
                  >
                    {config.hotkeys.dictation.some((k) => k === "Fn") ? "✓" : "!"}
                  </span>
                  <div>
                    <b>Fn 键</b>
                    <small>
                      {config.hotkeys.dictation.some((k) => k === "Fn")
                        ? "已启用 · 需将「按下 🌐 键时」设为无操作"
                        : "未启用 · 在「快捷键」页录制 🌐 Fn"}
                    </small>
                  </div>
                  <button
                    className="btn"
                    onClick={() => invoke("open_keyboard_settings").catch(() => {})}
                  >
                    去设置
                  </button>
                </div>
              </div>
              <p className="muted">
                开发模式下权限弹窗可能挂在启动应用的工具（终端）名下，打包版以 Sayit
                自己的名义申请。
              </p>
            </>
          )}

          {/* ============ 关于 ============ */}
          {tab === "about" && (
            <>
              <h2 className="sect">关于</h2>
              <div className="card about">
                <Logo size={56} />
                <div className="about-name">Sayit</div>
                <p className="about-slogan">说就写了 · 本地优先的 macOS 语音输入</p>
                <p className="muted">v0.1.0 · MIT License</p>
                <p className="muted">github.com/xpzero/sayit</p>
                <p className="muted">
                  灵感致谢 Typeless 与 TypeAloud · 引擎 llama.cpp（MIT）· 模型 Qwen3-ASR（Apache
                  2.0）
                </p>
              </div>
            </>
          )}
        </main>
      </div>
    </div>
  );
}
