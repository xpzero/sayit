// 麦克风自检状态（RecPane 与 Settings 壳共享）
export type Selftest =
  | { kind: "idle" }
  | { kind: "recording" }
  | { kind: "done"; ok: boolean; verdict: string; level?: "ok" | "quiet" | "silent" | "nodata" }
  | { kind: "error"; message: string };
