// 关于页
import { Logo } from "./brand";

export function AboutPane() {
  return (
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
  );
}
