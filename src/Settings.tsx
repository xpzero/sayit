// 设置页 · 宣纸文人风 · 壳：状态提升 + 事件监听 + Tab 路由；各 Tab 的 UI 在 src/settings/
import { useEffect, useRef, useState, type ReactElement } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { Config, DownloadEvent, EngineStatus, HistoryEntry, LibraryItem } from "./types";
import { Logo, Ico } from "./settings/brand";
import type { Selftest } from "./settings/Selftest";
import { RecPane } from "./settings/RecPane";
import { ModelPane } from "./settings/ModelPane";
import { UsePane } from "./settings/UsePane";
import { HistoryPane } from "./settings/HistoryPane";
import { AboutPane } from "./settings/AboutPane";

type Tab = "rec" | "model" | "key" | "history" | "about";





const TABS: { id: Tab; label: string; icon: ReactElement }[] = [
  { id: "rec", label: "识别", icon: Ico.mic },
  { id: "model", label: "模型库", icon: Ico.box },
  { id: "key", label: "使用", icon: Ico.sliders },
  { id: "history", label: "历史", icon: Ico.wave },
  { id: "about", label: "关于", icon: Ico.info },
];

export default function Settings() {
  const [tab, setTab] = useState<Tab>("rec");
  const [config, setConfig] = useState<Config | null>(null);
  const [engine, setEngine] = useState<EngineStatus | null>(null);
  const [saveErr, setSaveErr] = useState<string | null>(null);
  const [selftest, setSelftest] = useState<Selftest>({ kind: "idle" });
  const [history, setHistory] = useState<HistoryEntry[]>([]);
  const [library, setLibrary] = useState<LibraryItem[]>([]);
  const [dl, setDl] = useState<Record<string, DownloadEvent>>({});
  const [libErr, setLibErr] = useState<string | null>(null);
  const [capturing, setCapturing] = useState(false); // 快捷键输入框聚焦 = 录制态
  const [preview, setPreview] = useState<string | null>(null); // keydown 实时预览的组合
  const [axOk, setAxOk] = useState<boolean | null>(null);
  // 麦克风授权原始状态（后端 mic_status：3 已授权/0 未询问/1、2 拒绝·受限；null 查询失败）
  const [micPerm, setMicPerm] = useState<number | null>(null);
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

  const refreshHistory = () =>
    invoke<HistoryEntry[]>("get_history")
      .then(setHistory)
      .catch(() => {});


  useEffect(() => {
    invoke<Config>("get_config").then(setConfig);
    invoke<EngineStatus>("detect_engine").then(setEngine);
    invoke<boolean>("accessibility_status").then(setAxOk);
    invoke<number | null>("mic_status").then(setMicPerm);
    refreshLibrary();
    refreshHistory();

    const unMicPerm = listen<{ granted: boolean }>("mic-perm", (e) => {
      setMicPerm(e.payload.granted ? 3 : 2);
    });
    const un1 = listen<{
      stage: string;
      verdict?: string;
      ok?: boolean;
      kind?: "ok" | "quiet" | "silent" | "nodata";
      message?: string;
    }>("selftest", (e) => {
      const p = e.payload;
      if (p.stage === "recording") setSelftest({ kind: "recording" });
      else if (p.stage === "done")
        setSelftest({
          kind: "done",
          ok: !!p.ok,
          verdict: p.verdict ?? "",
          level: p.kind ?? (p.ok ? "ok" : "silent"),
        });
      else if (p.stage === "error") setSelftest({ kind: "error", message: p.message ?? "" });
    });
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
    // 后端 tap 的捕获事件：keyup 提交组合 / Fn 与 Menu 令牌；cancelled = Esc 退出。
    // 提交即清预览——组合已变成 chip，输入框里那份实时预览不清就会和新 chip 并排重复
    // （2026-11-11 用户实测发现）；Esc 退出同理清掉残留的半截预览
    const un3b = listen<{ key?: string; cancelled?: boolean }>("hotkey-captured", (e) => {
      setPreview(null);
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
    // 键码表外的键（如 A）：后端拒绝录入，预览位亮原因——不再是“按了没反应”
    const un3e = listen("hotkey-rejected", () => {
      setPreview("✗ 此键不可用作快捷键主键（如 A 键）");
    });
    // 深链直达某个 tab（如"未配置模型"面板按钮 → 模型库）。
    // 旧 id 兼容：「云端」已并入识别页、「权限」已是常驻体检条（无需跳转）
    const un4 = listen<{ tab?: string }>("goto-tab", (e) => {
      const t = e.payload.tab;
      if (!t) return;
      const mapped: Tab | null =
        t === "cloud" || t === "rec" ? "rec" : t === "perm" ? null : (t as Tab);
      if (mapped) setTab(mapped);
    });
    // 窗口隐藏不销毁（✕=隐藏），所以每次重新聚焦时自动拉新数据，避免旧快照
    const un3 = getCurrentWindow().onFocusChanged(({ payload: focus }) => {
      if (focus) {
        refreshLibrary();
        refreshHistory();
        invoke<Config>("get_config").then(setConfig);
        invoke<EngineStatus>("detect_engine").then(setEngine);
        invoke<boolean>("accessibility_status").then(setAxOk);
        invoke<number | null>("mic_status").then(setMicPerm);
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
      unMicPerm.then((f) => f());
      un2.then((f) => f());
      un3.then((f) => f());
      un3b.then((f) => f());
      un3c.then((f) => f());
      un3d.then((f) => f());
      un3e.then((f) => f());
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
            <RecPane
              config={config}
              setProvider={setProvider}
              setCloud={setCloud}
              engine={engine}
              defaultModel={library.find((m) => m.id === config.stt.localModelId)}
              setTab={setTab}
              selftest={selftest}
              setSelftest={setSelftest}
              axOk={axOk}
              micPerm={micPerm}
            />
          )}

          {tab === "model" && (
            <ModelPane
              config={config}
              setConfig={setConfig}
              library={library}
              dl={dl}
              libErr={libErr}
              setLibErr={setLibErr}
              refreshLibrary={refreshLibrary}
            />
          )}

          {tab === "key" && (
            <UsePane
              config={config}
              update={update}
              capturing={capturing}
              setCapturing={setCapturing}
              preview={preview}
              setPreview={setPreview}
              hkInputRef={hkInputRef}
              onHkKeyDown={onHkKeyDown}
              removeHotkey={removeHotkey}
            />
          )}

          {tab === "history" && <HistoryPane history={history} setHistory={setHistory} />}

          {tab === "about" && <AboutPane />}
        </main>
      </div>
    </div>
  );
}
