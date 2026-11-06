# Sayit — 本地优先的 macOS 语音输入工具

按快捷键说话，文字自动出现在光标处。免费、离线优先、语音不出本机。

> 灵感来源：**[Typeless](https://typeless.com)** 与 **[TypeAloud](https://typealoud.app)**（开源）。
> 前者定义了"快捷键听写 → 文字注入光标处"的交互范式（含 Fn 键触发、按应用注入规则）；
> 后者验证了 Qwen3-ASR + llama.cpp 端上听写在 Mac 上的可行性。
> Sayit 站在两者的肩膀上，做一个**完全开源、免费、可插拔**的版本。感谢两款优秀的先行者。
>
> 商标声明：Typeless、TypeAloud 等名称与商标归其各自所有者所有。Sayit 为独立项目，
> 与上述项目无隶属、合作或代言关系，文中提及仅为事实性说明与致谢。

## 功能

- **本地语音识别**（默认）：Qwen3-ASR 0.6B（GGUF / llama.cpp 引擎，Apple 芯片 GPU 加速），引擎常驻内存后秒回
- **云端识别**（备选）：任何 OpenAI 兼容接口（OpenAI / 硅基流动 / Groq / 本地 faster-whisper 服务……）
- **模型库**：精选模型一键下载（断点续传 + GGUF 魔数校验），支持登记本地模型文件、多模型切换默认
- **快捷键**：组合键任意定制（录制框直接按出来），支持 **🌐 Fn 键**触发；一个功能可绑多个键
- **文字注入**：识别结果自动粘贴进光标处（TextEdit / 浏览器 / 终端 / 微信……），不抢焦点；复制按钮永远兜底
- **悬浮面板**：录音计时 / 音量条 / 模型名 / 静音警告（"未听到声音"= 麦克风权限或设备的早期预警）
- **形态**：菜单栏托盘常驻，无主窗口

## 架构（四层 + trait 插槽）

```
界面层 React:   Panel.tsx（事件驱动四态面板）/ Settings.tsx（设置页）
桥 Tauri IPC:   invoke 命令 / emit 事件（recording / phase / result / level / download-progress）
编排层 Rust:    commands.rs —— 用例流程（快捷键 → 录音 → 识别 → 注入）
能力层 Rust:    audio.rs（cpal 采集 16kHz）
               stt/{local,openai_compatible}.rs —— Transcriber 双插头
               hotkey/{mod,fn_listener}.rs —— 组合键注册 + Fn/捕获轮询
               models.rs（模型库+下载器） engines.rs（引擎三级探测）
               inject.rs（剪贴板三步舞） config.rs（serde↔JSON 原子读写）
```

识别引擎是 trait 插槽：`Local`（llama-server 子进程，懒启动常驻）与 `OpenAiCompatible`（云端）说同一种
"OpenAI 方言"，加新引擎 = 加一个 impl，编排层零改动。

## 开发

```bash
brew install llama.cpp          # 引擎（开发期用系统检测；打包版内置）
npm install
npm run tauri dev               # 注意：必须从「终端」运行！
```

> **为什么必须从终端跑 dev？** macOS 隐私系统按"责任人 App"授权麦克风。从终端启动时
> 责任人是终端（有户口、能弹授权窗）；从某些后台进程启动的程序是"黑户"，申请会被
> 静默拒绝——macOS 给"全零假数据"而不报错，表现为音量条不动。应用内设置页有
> 「测试麦克风」可诊断此状况。

### 两个必需的系统权限

| 权限 | 用途 | 授权方式 |
|---|---|---|
| 麦克风 | 录音 | 首次录音自动弹窗（dev 挂在终端名下；打包版以 Sayit 自己的名义） |
| 辅助功能 | 模拟 ⌘V 注入文字 | 系统设置 → 隐私与安全性 → 辅助功能 |

启用 Fn 键触发需把 系统设置 → 键盘 →「按下 🌐 键时」改为**无操作**（设置页有直达按钮）。

## 打包

```bash
npm run tauri build
scripts/postprocess-app.sh      # 注入 Info.plist 权限声明（麦克风用途/托盘标志）+ 重签名
```

## 图标

```bash
uv run --with pillow scripts/make_icon.py   # 生成 scripts/icon-source.png
npx tauri icon scripts/icon-source.png      # 重新生成全平台图标
```

## 数据目录

```
~/Library/Application Support/com.sayit.app/
├── config.json        # 全部设置（原子写入）
├── models/            # 下载的模型
├── engines/           # 托管引擎（预留）
├── recordings/        # 每次录音的 WAV
└── llama-server.log   # 引擎日志（排障用）
```

## 踩坑实录（开发中换来的）

1. **全零假数据**：麦克风被拒不会报错，只给静音数据——已内置 2 秒静音警告
2. **旧版系统设置深链**：`com.apple.preference.universalaccess` 在新版 macOS 打开的是辅助功能
   *功能页*而非授权列表，正确格式是 `com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility`
3. **键码 0（A 键）恒读 true**：CGEventSourceKeyState 的系统怪癖，已在键表中除名
4. **捕获模式的边沿残留**：录完 Fn 松手前的"仍按着"会被误判成新触发——边沿在轮询顶部统一消费
5. **dev 二进制随编译变身份**：未签名的 dev 版辅助功能授权可能随重编译失效，重勾即可；打包签名后稳定

## Roadmap

- v1.1：引擎库（whisper.cpp / sherpa-onnx 可选下载）、"粘贴上次结果"独立快捷键、按住说话模式
- v1.2：模型 sha256 校验（等官方公布指纹）、API Key 迁移到钥匙串、按应用注入黑白名单

## 许可

MIT（引擎与模型各自遵循其上游许可：llama.cpp MIT · Qwen3-ASR Apache 2.0）
