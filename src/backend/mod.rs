//! Agent 后端边界。
//!
//! 当前先放 DSH 一次性回合实现。Direct Agent 仍由既有 CLI/daemon 路径驱动，
//! 等 DSH ask 验收后再把两者统一到完整的 backend trait。

mod dsh;

pub(crate) use dsh::DshBackend;
