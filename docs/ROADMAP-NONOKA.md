# Nonoka 复刻路线图（自用）

基线：Miyu v0.4.5，完整 fork 并全局改名为 `nonoka`。

## 已完成

- [x] 建立仓库，保留上游完整 git 历史
- [x] 包/二进制/数据目录品牌改名：`miyu` → `nonoka`
- [x] 资源改名：logo、壁纸、默认人格文件、表情库、matugen 模板
- [x] 补充 NOTICE.md 与上游署名
- [x] Rust 原生 DSH 协议客户端：RPC + `events.mux` WebSocket
- [x] `nonoka dsh-test` 真实回合验收
- [x] `nonoka ask --backend dsh` 一次性聊天验收

## 待办（按建议顺序）

1. [x] 编译验证：`cargo build`，修复改名引入的问题
2. [x] 首跑验证：`nonoka init` / `nonoka paths` / `nonoka config validate`
3. [ ] 配置自己的 LLM API（`nonoka config` → 供应商/模型）
4. [x] 确定基础人格与外观 → `src/prompts/nonoka.md`、`nonoka.hint.md`、`nonoka-dialogs.md`
5. [x] 替换基础品牌资源：`pics/`、`web/assets/`、`src/memes/nonoka/`
6. [ ] 选择默认知识库策略（保留上游 ShorinWiki 或换成自己的）
7. [x] 接入 DSH MCP 工具桥，保留 Nonoka 全部工具能力
8. [ ] 接入 QQ：NapCat/OneBot 反向 WS
9. [x] GitHub 私有仓库：`Yukikaze2233/nonoka`

## 每次改动后的验收流程（沿用上游 AGENTS.md）

1. `cargo check` 或 `cargo build --release` 通过
2. 跑相关 `cargo test`
3. 至少实际对话一轮，确认工具循环/缓存/持久化正常
4. 确认后再 commit
