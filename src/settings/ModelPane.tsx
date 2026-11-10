// 模型库页：精选下载 + 自带登记
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { Config, DownloadEvent, LibraryItem } from "../types";
import { Ico } from "./brand";
import { fmtMB } from "./format";

export function ModelPane(p: {
  config: Config;
  setConfig: (c: Config) => void;
  library: LibraryItem[];
  dl: Record<string, DownloadEvent>;
  libErr: string | null;
  setLibErr: (e: string | null) => void;
  refreshLibrary: () => void;
}) {
  const { config, setConfig, library, dl, libErr, setLibErr, refreshLibrary } = p;
  const startDownload = (id: string) => {
    setLibErr(null);
    invoke("download_model", { id }).catch((e) => setLibErr(String(e)));
  };
  return (
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
  );
}
