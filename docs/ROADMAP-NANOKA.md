# Nanoka 复刻路线图（自用）

基线：Miyu v0.4.5，完整 fork 并全局改名为 `nanoka`。

## 已完成

- [x] 建立仓库，保留上游完整 git 历史
- [x] 包/二进制/数据目录品牌改名：`miyu` → `nanoka`
- [x] 资源改名：logo、壁纸、默认人格文件、表情库、matugen 模板
- [x] 补充 NOTICE.md 与上游署名

## 待办（按建议顺序）

1. [ ] 编译验证：`cargo build --release`，修复改名引入的问题
2. [ ] 首跑验证：`nanoka init` / `nanoka normal` / `nanoka dev` / `nanoka web`
3. [ ] 配置自己的 LLM API（`nanoka config` → 供应商/模型）
4. [ ] 确定人格设定 → 替换 `src/prompts/nanoka.md`、`nanoka.hint.md`、`nanoka-dialogs.md`
5. [ ] 替换品牌资源：`pics/`、`web/assets/`、`src/memes/nanoka/`、内置音频
6. [ ] 选择默认知识库策略（保留上游 ShorinWiki 或换成自己的）
7. [ ] 按需接入 QQ：NapCat/OneBot 反向 WS
8. [ ] 按需发布：GitHub 远程、AUR PKGBUILD、release 资产

## 每次改动后的验收流程（沿用上游 AGENTS.md）

1. `cargo check` 或 `cargo build --release` 通过
2. 跑相关 `cargo test`
3. 至少实际对话一轮，确认工具循环/缓存/持久化正常
4. 确认后再 commit
