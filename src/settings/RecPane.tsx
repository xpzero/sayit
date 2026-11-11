// 识别页：权限体检条 + 识别方式（云端内联展开）+ 当前模型/引擎
import { invoke } from "@tauri-apps/api/core";
import type { Config, EngineStatus, LibraryItem } from "../types";
import { Ico } from "./brand";
import type { Selftest } from "./Selftest";

export function RecPane(p: {
  config: Config;
  setProvider: (v: "local" | "openai_compatible") => void;
  setCloud: (patch: Partial<Config["stt"]["cloud"]>) => void;
  engine: EngineStatus | null;
  defaultModel?: LibraryItem;
  setTab: (t: "model") => void;
  selftest: Selftest;
  setSelftest: (s: Selftest) => void;
  axOk: boolean | null;
  micPerm: number | null; // 3 已授权 / 0 未询问 / 1、2 拒绝·受限 / null 查询失败
}) {
  const { config, engine, defaultModel, setTab, selftest, setSelftest, axOk, micPerm,
          setProvider, setCloud } = p;
  return (
    <>
          {/* 权限体检条：识别页顶部（默认落地页——进设置第一眼即见，权限是主功能的闸门）。
              长诊断放 title 悬停，一格一行不占版面 */}
          <div className="perm-strip">
            <button
              className={
                "perm-cell" +
                (micPerm === null
                  ? ""
                  : micPerm === 3
                    ? selftest.kind === "done"
                      ? selftest.level === "ok"
                        ? " ok"
                        : selftest.level === "quiet"
                          ? " warn"
                          : " bad"
                      : selftest.kind === "error"
                        ? " bad"
                        : ""
                    : " bad") // 未授权/被拒绝：权限是自检的前置，先亮红
              }
              title={
                micPerm === 0
                  ? "首次使用需要授权：点击弹出系统授权窗"
                  : micPerm === 1 || micPerm === 2
                    ? "系统设置里拒绝了 Sayit 的麦克风——点击前往开启"
                    : selftest.kind === "done"
                      ? selftest.verdict
                      : "录 1.5 秒自检——测试期间要对着麦克风说话；全零=权限被拒"
              }
              onClick={() => {
                // 三态分流：未询问→系统授权窗；被拒绝→带路系统设置；已授权→自检
                if (micPerm === 0) {
                  invoke("request_mic_permission").catch(() => {});
                } else if (micPerm === 1 || micPerm === 2) {
                  invoke("open_mic_settings").catch(() => {});
                } else {
                  setSelftest({ kind: "recording" });
                  invoke("mic_selftest").catch(() => {});
                }
              }}
            >
              {Ico.mic}
              <b>麦克风</b>
              <small>
                {micPerm === null
                  ? "查询中…"
                  : micPerm === 0
                    ? "未授权 · 点击开启"
                    : micPerm === 1 || micPerm === 2
                      ? "被拒绝 · 点击去开启"
                      : selftest.kind === "recording"
                        ? "请说句话…"
                        : selftest.kind === "done"
                          ? selftest.level === "ok"
                            ? "正常 ✓"
                            : selftest.level === "quiet"
                              ? "声音轻 · 凑近说"
                              : selftest.level === "silent"
                                ? "全零 · 查权限"
                                : "无数据"
                          : selftest.kind === "error"
                            ? "失败 ✗"
                            : "点击测试"}
              </small>
            </button>
            <button
              className={"perm-cell" + (axOk === false ? " bad" : axOk ? " ok" : "")}
              title="模拟按键注入文字的前提；未授权时注入会被系统静默丢弃"
              onClick={() => invoke("open_accessibility_settings").catch(() => {})}
            >
              {Ico.shield}
              <b>辅助功能</b>
              <small>{axOk === null ? "查询中…" : axOk ? "已授权 ✓" : "未授权 · 点击开启"}</small>
            </button>
          </div>

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
                <small>OpenAI 兼容 · 按量付费 · 选中后下方配置</small>
              </div>
            </label>
          </div>

          <div className="card top14">
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

          {config.stt.provider === "openai_compatible" && (
            <div className="card top14">
              <h3>{Ico.cloud} 云端接口配置</h3>
              <div className="grid2">
                <div className="field">
                  <label>接口地址（Base URL）</label>
                  <input
                    value={config.stt.cloud.endpoint}
                    onChange={(e) => setCloud({ endpoint: e.target.value })}
                    placeholder="https://api.siliconflow.cn/v1"
                  />
                </div>
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
                  <label>模型名</label>
                  <input
                    value={config.stt.cloud.model}
                    onChange={(e) => setCloud({ model: e.target.value })}
                    placeholder="FunAudioLLM/SenseVoiceSmall"
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
              <p className="muted top10">
                任何 OpenAI 兼容接口：OpenAI / 硅基流动 / Groq / 本地
                faster-whisper 服务。密钥仅存本机（v1.2 迁移钥匙串），修改即保存。
              </p>
            </div>
          )}
    </>
  );
}
