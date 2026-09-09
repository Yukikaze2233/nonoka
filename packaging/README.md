# packaging

各发行版/平台的打包适配，每个平台一个子目录：

- `arch/nonoka-git/` — AUR VCS 包（从最新 main 源码构建），AUR `nonoka-git` 的真相源
- `arch/nonoka-release/` — 发布资产构建器：从 `v<版本>` 标签构建预编译
  `nonoka-<版本>-<rel>-x86_64.pkg.tar.zst`，上传到 GitHub Release
- `arch/nonoka/` — AUR 二进制包装包（下载上述 Release 资产），AUR `nonoka` 的真相源
- `arch/nonoka-voice/` — AUR 二进制包装包（下载 `nonoka-release` 拆出的 `nonoka-voice` 资产），
  AUR `nonoka-voice` 的真相源；依赖 `nonoka`

## 发布流程（Arch）

1. `Cargo.toml` 升版本 → 提交 `release: vX.Y.Z` → 打标签 `vX.Y.Z` → push（含标签）
2. 在干净目录用 `arch/nonoka-release/PKGBUILD` 构建：
   `PACKAGER='Nonoka Release <noreply@example.com>' makepkg -Cf`
3. `gh release create vX.Y.Z nonoka-X.Y.Z-1-x86_64.pkg.tar.zst nonoka-voice-X.Y.Z-1-x86_64.pkg.tar.zst --title "Nonoka X.Y.Z"`
   （两个资产都要传，AUR `nonoka-voice` 靠第二个）
4. 更新 `arch/nonoka/PKGBUILD`、`arch/nonoka-voice/PKGBUILD` 的 `pkgver` 与资产 sha256，
   `arch/nonoka-git/PKGBUILD` 刷新 `pkgver` 快照
5. 复制三份 PKGBUILD 到 AUR 检出目录（`~/Documents/aur/nonoka`、`nonoka-voice`、`nonoka-git`），
   `makepkg -Cf` 本地实测，`makepkg --printsrcinfo > .SRCINFO`，提交 `upd: X.Y.Z` 并 push

## 系统资产约定

除 `/usr/bin/nonoka` 外，Nonoka 运行时按固定路径查找以下系统资产，
打包时需要一并安装：

| 路径 | 内容 | 来源 | 缺失时的行为 |
|---|---|---|---|
| `/usr/share/nonoka/fonts/` | 长回复转图片的渲染字体 | Noto 上游（AUR 包装包下载；发布资产不含字体） | 长文转图静默退化为纯文本 |
| `/usr/share/nonoka/memes/nonoka/` | 内置表情库 | `src/memes/nonoka/` | 默认人格无内置表情 |
| `/usr/share/nonoka/default-kb/` | 默认知识库 | 本仓库 `kb/` + Shorin Wiki 仓库，运行时 `nonoka update-default-kb` 更新 | 默认知识库为空 |
| `/usr/share/nonoka/scripts/personas/default/` | 内置脚本（属默认人格 Nonoka） | `src/scripts/personas/default/` | 无内置脚本 |
| `/usr/share/nonoka/models/<id>/` | 内置本地 embedding 模型（语义检索辅助，默认 `bge-small-zh-v1.5-int8`） | `assets/models/` | 语义检索静默退回关键词；运行库 `libonnxruntime` 来自 `onnxruntime-cpu` 或 `onnxruntime-cuda`（都 provides `onnxruntime`） |
