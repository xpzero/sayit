// 路由器：一个前端，两个窗口——启动时看自己住在哪个窗口（label），渲染对应页面
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./App.css"; // 全局唯一样式表——没有这行，界面就是裸的 HTML
import Panel from "./Panel";
import Settings from "./Settings";

export default function App() {
  const label = getCurrentWindow().label;
  return label === "panel" ? <Panel /> : <Settings />;
}
