//! `nonoka ask --backend dsh` 的一次性聊天入口。
//!
//! 这里刻意不复用旧 Agent 的渲染与会话状态：DSH 负责完整 Agent 回合，
//! 本层只负责输入、后端选择和最终文本输出。后续接入 REPL/WebUI 时，
//! 事件映射会下沉到独立的 backend/application 层。

use super::NonokaPaths;
use crate::backend::DshBackend;
use crate::i18n::text as t;
use anyhow::Result;

pub(in crate::cli) async fn run_dsh_ask(
    paths: &NonokaPaths,
    message: String,
    agent_preset: Option<String>,
    plain: bool,
) -> Result<()> {
    let backend = DshBackend::from_env(agent_preset)?;
    if !plain {
        println!(
            "{}",
            t("Waiting for DSH agent...", "正在等待 DSH Agent 回复……")
        );
    }
    let reply = backend.chat(paths, message).await?;
    println!("{reply}");
    Ok(())
}
