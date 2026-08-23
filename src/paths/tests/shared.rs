//! 路径测试共用的 fixture。

use crate::paths::*;

pub(super) fn test_layouts(root: &Path) -> (LegacyLayout, Layout) {
    (
        LegacyLayout {
            config_dir: root.join("legacy/config"),
            data_dir: root.join("legacy/data"),
            cache_dir: root.join("legacy/cache"),
            state_dir: root.join("legacy/state"),
            documents_dir: root.join("Documents/Nanoka"),
            pictures_dirs: vec![root.join("Pictures/nanoka"), root.join("Pictures/Nanoka")],
        },
        Layout {
            root_dir: root.join(".nanoka"),
            config_dir: root.join(".nanoka/config"),
            data_dir: root.join(".nanoka/data"),
            cache_dir: root.join(".nanoka/cache"),
            state_dir: root.join(".nanoka/state"),
        },
    )
}
