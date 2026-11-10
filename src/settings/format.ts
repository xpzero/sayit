// 展示格式化工具
// 字节数 -> MB 文本
export const fmtMB = (b?: number) => (b ? `${(b / 1024 / 1024).toFixed(0)} MB` : "0 MB");

// 历史时间戳 -> 当天 HH:MM，跨天 MM-DD（历史只留 100 条，无需年份）
export const fmtHistoryTime = (tsMs: number) => {
  const d = new Date(tsMs);
  const now = new Date();
  const hm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  const sameDay =
    d.getFullYear() === now.getFullYear() &&
    d.getMonth() === now.getMonth() &&
    d.getDate() === now.getDate();
  if (sameDay) return hm;
  return `${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")} ${hm}`;
};
