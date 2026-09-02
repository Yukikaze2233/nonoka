# Nonoka 是怎么实现的

## 设计者
Nonoka 由 QQ 2786450878 设计。

## 一句话
Nonoka 是 Rust 写的 AI 助手，核心是「平台无关的 Agent + 工具注册表 + 本地状态账本」，终端、WebUI、QQ 都只是接入它的前端。

## 技术栈
- 语言：Rust（edition 2021，rust-version 1.89）
- 异步运行时：Tokio（multi-thread）
- Web/daemon：axum（HTTP + WebSocket）
- HTTP 客户端：reqwest（rustls-tls）
- 终端 UI：crossterm + rustyline + cosmic-text
- 数据存储：SQLite（rusqlite bundled，WAL 模式）
- 渲染：终端 Markdown/ANSI，公式用 RaTeX，长文转图可用独立 renderer worker
- 序列化：serde / serde_json
- LLM 接入：OpenAI Chat/Responses 与 Anthropic 两种协议，统一到 OpenAiCompatibleClient
- 平台接入：QQ 走 OneBot v11 反向 WebSocket（主要适配 NapCat）

## 架构
- CLI 只做入口；真正常驻的是 daemon（`nonoka daemon` → `web::run`）
- 前端（终端 REPL / WebUI / QQ）优先通过 Unix socket IPC 找 daemon；daemon 不在才本地直连兜底
- 平台无关核心：`agent`（消息组装 + 工具循环）、`tools`（80+ 工具注册表）、`llm`（供应商适配）、`state`（SQLite 会话/用量账本）
- 一次对话：用户输入 → 平台层转内部消息 → QQ 场景先过 real_context 等插件链 → Agent 组装提示词/历史/记忆 → 工具循环调 LLM → 结果持久化 → 平台回复
- 部署架构：Nonoka daemon + NapCatQQ（Docker）部署在 NAS，QQ 消息经 NapCat → OneBot 反向 WebSocket → Nonoka；DSH web 提供飞书通道和 MCP 工具桥

## 设计哲学
1. 上下文是只增账本（append-only），相同状态必须产生逐字节相同的前缀，靠前缀缓存省钱
2. 权限由代码承担，不靠提示词；身份/权限在执行层判定
3. 三份内容分离：raw_content（用户原话，供记忆）、display_content（界面显示）、context_messages（工程附加物）
4. 工具目录用 stub 模式：懒工具以「真名 + 摘要 + 宽松参数壳」常驻，完整 schema 按需加载，兼顾缓存命中与工具数量

## 两个模式
- Normal：全功能，日常对话、系统排障、天气、知识库、QQ 等
- Dev：极简编码形态，去掉与人设/娱乐无关的内容，专注开发
