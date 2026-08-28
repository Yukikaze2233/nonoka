//! DSH 一次性聊天后端。
//!
//! 它只负责把 Nonoka 的文本消息投递给 DSH，并把 DSH 的一个完整 turn
//! 映射成终端可打印的文本。工具循环、审批、Agent 上下文由 DSH 负责。

use crate::dsh::{DshClient, DshContentBlock, DshPrompt, DshTurnCollector};
use crate::paths::NonokaPaths;
use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;
use tokio::time::timeout;

const TURN_TIMEOUT: Duration = Duration::from_secs(300);
const SUBSCRIPTION_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
pub(crate) struct DshBackend {
    client: DshClient,
    agent_preset: Option<String>,
}

impl DshBackend {
    pub(crate) fn from_env(agent_preset: Option<String>) -> Result<Self> {
        Ok(Self {
            client: DshClient::from_env()?,
            agent_preset,
        })
    }

    pub(crate) async fn chat(&self, paths: &NonokaPaths, message: String) -> Result<String> {
        self.chat_in_directory(&paths.state_dir.join("dsh-ask"), message)
            .await
    }

    pub(crate) async fn chat_in_directory(
        &self,
        directory: &Path,
        message: String,
    ) -> Result<String> {
        tokio::fs::create_dir_all(directory)
            .await
            .context("creating DSH backend workspace")?;
        let cwd = directory.to_string_lossy().to_string();
        let session = self
            .client
            .session_create(Some(&cwd), None, self.agent_preset.as_deref())
            .await?;
        let turn_result = async {
            let mut events = self.client.events_mux().await?;
            timeout(
                SUBSCRIPTION_TIMEOUT,
                events.wait_for_session_subscription(&session.session_id),
            )
            .await
            .context("timed out waiting for DSH session subscription")??;
            let mut collector = DshTurnCollector::new(&session.session_id);
            self.client
                .session_prompt(&DshPrompt {
                    session_id: session.session_id.clone(),
                    mode: "queue".to_string(),
                    content: vec![DshContentBlock::Text { text: message }],
                })
                .await?;
            let result = timeout(TURN_TIMEOUT, async {
                loop {
                    let frame = events
                        .next_session_frame(&session.session_id)
                        .await?
                        .context("DSH event stream closed before turn/end")?;
                    if let Some(result) = collector.push(&frame)? {
                        return Ok::<_, anyhow::Error>(result);
                    }
                }
            })
            .await
            .context("timed out waiting for DSH turn")??;
            Ok::<_, anyhow::Error>(result)
        }
        .await;

        // 测试、取消和模型失败都不能把临时 session 留在 DSH 列表里。
        if let Err(error) = self
            .client
            .workspace_archive_session(&session.session_id)
            .await
        {
            tracing::warn!(session_id = %session.session_id, %error, "failed to archive one-shot DSH session");
        }
        Ok(turn_result?.text)
    }
}
