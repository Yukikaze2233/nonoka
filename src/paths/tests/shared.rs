//! 路径测试共用的 fixture。

use crate::paths::*;

pub(super) fn test_layouts(root: &Path) -> (LegacyLayout, Layout) {
    (
        LegacyLayout {
            config_dir: root.join("legacy/config"),
            data_dir: root.join("legacy/data"),
            cache_dir: root.join("legacy/cache"),
            state_dir: root.join("legacy/state"),
            documents_dir: root.join("Documents/Nonoka"),
            pictures_dirs: vec![root.join("Pictures/nonoka"), root.join("Pictures/Nonoka")],
        },
        Layout {
            root_dir: root.join(".nonoka"),
            config_dir: root.join(".nonoka/config"),
            data_dir: root.join(".nonoka/data"),
            cache_dir: root.join(".nonoka/cache"),
            state_dir: root.join(".nonoka/state"),
        },
    )
}
