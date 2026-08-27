# Nonoka WebUI × matugen

## 配置

复制模板文件`nonoka-theme.css`到合适的位置，然后在 `~/.config/matugen/config.toml` 或者其他 Matugen 相关的配置文件中加入:

```toml
[templates.nonoka]
input_path = "/path/to/nonoka-theme.css"
output_path = "~/.nonoka/config/webui-theme.css"
```

然后正常运行 matugen(例如 `matugen image /path/to/wallpaper.png`)。

