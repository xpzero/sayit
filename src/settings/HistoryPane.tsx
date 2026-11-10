// 历史页：听写历史列表 + 逐条复制
import { invoke } from "@tauri-apps/api/core";
import type { HistoryEntry } from "../types";
import { fmtHistoryTime } from "./format";

export function HistoryPane(p: {
  history: HistoryEntry[];
  setHistory: (fn: (prev: HistoryEntry[]) => HistoryEntry[]) => void;
}) {
  const history = p.history;
  const setHistory = p.setHistory;
  return (
    <>
          <h2 className="sect">
            听写历史
            <span className="hist-spacer" />
            {history.length > 0 && (
              <button
                className="btn danger"
                onClick={() => {
                  invoke("clear_history").catch(() => {});
                  setHistory(() => []);
                }}
              >
                清空
              </button>
            )}
          </h2>
          <p className="desc">
            最近 100 条听写文案（本地保存）。点「复制」把该条写入剪贴板——应用不主动
            动你的剪贴板，复制永远是显式动作。
          </p>
          {history.length === 0 ? (
            <div className="card hist-empty">还没有听写记录</div>
          ) : (
            <div className="card">
              {history.map((h) => (
                <div key={h.tsMs} className="hist-item">
                  <div className="hist-time">{fmtHistoryTime(h.tsMs)}</div>
                  <div className="hist-text" title={h.text}>
                    {h.text}
                  </div>
                  <div className="hist-actions">
                    <button
                      className="btn"
                      onClick={(ev) => {
                        invoke("copy_text", { text: h.text })
                          .then(() => {
                            ev.currentTarget.textContent = "已复制 ✓";
                          })
                          .catch(() => {
                            ev.currentTarget.textContent = "复制失败 ✗";
                          });
                      }}
                    >
                      复制
                    </button>
                    <button
                      className="btn danger"
                      title="删除这条"
                      onClick={() => {
                        invoke("delete_history", { tsMs: h.tsMs }).catch(() => {});
                        setHistory((prev) => prev.filter((x) => x.tsMs !== h.tsMs));
                      }}
                    >
                      ×
                    </button>
                  </div>
                </div>
              ))}
            </div>
          )}
    </>
  );
}
