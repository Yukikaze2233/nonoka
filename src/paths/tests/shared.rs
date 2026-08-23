//! 路径测试共用的 fixture。

use crate::paths::*;

pub(super) fn test_layouts(root: &Path) -> (LegacyLayout, Layout) {
    (
        LegacyLayout {
            config_dir: root.join("legacy/config"),
            data_dir: root.join("legacy/data"),
            cache_dir: root.join("legacy/cache"),
            state_dir: root.join("legacy/state"),
            documents_dir: root.join("Documents/Hotaru"),
            pictures_dirs: vec![root.join("Pictures/hotaru"), root.join("Pictures/Hotaru")],
        },
        Layout {
            root_dir: root.join(".hotaru"),
            config_dir: root.join(".hotaru/config"),
            data_dir: root.join(".hotaru/data"),
            cache_dir: root.join(".hotaru/cache"),
            state_dir: root.join(".hotaru/state"),
        },
    )
}
