// 面板窗口入口：只装 Panel（多入口改造前由 App.tsx 按窗口 label 分流，2026-11-11 起各窗口独立入口）
import React from "react";
import ReactDOM from "react-dom/client";
import "./App.css"; // 全局唯一样式表——没有这行，界面就是裸的 HTML
import Panel from "./Panel";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <Panel />
  </React.StrictMode>,
);
