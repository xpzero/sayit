// 与 Rust 侧结构体一一对应的 TypeScript 类型（serde camelCase 序列化）
// Rust 是源头，改结构时两边同步改

export interface CloudStt {
  endpoint: string;
  apiKey: string;
  model: string;
  language: string;
}

export interface SttConfig {
  provider: "local" | "openai_compatible";
  localModelId: string | null;
  cloud: CloudStt;
}

export interface EngineConfig {
  path: string | null; // null = 自动探测
}

export interface ModelEntry {
  id: string;
  name: string;
  source: "curated" | "user";
  requires: string;
  modelPath: string | null;
  mmprojPath: string | null;
}

export interface Hotkeys {
  dictation: string[];
  pasteLastResult: string[];
}

export interface Injection {
  mode: "paste" | "type";
}

export interface PanelCfg {
  autoHideSeconds: number;
}

export interface Config {
  stt: SttConfig;
  engine: EngineConfig;
  modelLibrary: ModelEntry[];
  hotkeys: Hotkeys;
  injection: Injection;
  panel: PanelCfg;
}

export interface EngineStatus {
  found: boolean;
  source: "settings" | "system" | "managed" | "none";
  path: string | null;
  capable: boolean;
  detail: string;
}

export interface LibraryItem {
  id: string;
  name: string;
  description: string;
  license: string;
  sizeHint: string;
  recommended: boolean;
  source: "curated" | "user";
  status: "ready" | "partial" | "absent";
  modelPath: string | null;
  mmprojPath: string | null;
}

export interface DownloadEvent {
  id: string;
  stage: "progress" | "done" | "cancelled" | "error";
  fileIndex?: number;
  fileCount?: number;
  file?: string;
  received?: number;
  total?: number;
  pct?: number;
  note?: string;
  message?: string;
}
