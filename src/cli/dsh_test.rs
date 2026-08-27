//! `nonoka dsh-test`：验证 DSH 的 HTTP RPC 与事件 WebSocket 链路。
//!
//! 这是 DSH Backend 的第一条垂直验收路径，不依赖现有 Agent/REPL。

use super::{DshTestArgs, NonokaPaths};
use crate::dsh::{DshClient, DshContentBlock, DshPrompt, DshTurnCollector};
use crate::i18n::text as t;
use anyhow::{bail, Context, Result};
use std::time::Duration;
use tokio::time::timeout;

pub(in crate::cli) async fn run_dsh_test(paths: &NonokaPaths, args: DshTestArgs) -> Result<()> {
    let base_url = args
        .base_url
        .or_else(|| std::env::var("NONOKA_DSH_BASE_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:3080".to_string());
    let client = DshClient::new(&base_url)?;

    println!("{} {base_url}", t("Connecting to DSH:", "正在连接 DSH："));
    let host = client.host_describe().await?;
    let version = host
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    println!(
        "{} {version}",
        t(
            "DSH connection established, version:",
            "DSH 连接成功，版本："
        )
    );

    let cwd = paths.state_dir.join("dsh-test");
    std::fs::create_dir_all(&cwd).context("creating DSH test workspace")?;
    let session = client
        .session_create(
            Some(&cwd.to_string_lossy()),
            None,
            args.agent_preset.as_deref(),
        )
        .await?;
    println!(
        "{} {}",
        t("DSH session created:", "DSH 会话已创建："),
        session.session_id
    );

    // 事件流必须先建立，再发送 prompt；否则首个 turn/start 可能在连接打开前产生。
    let mut events = client.events_mux().await?;
    timeout(
        Duration::from_secs(15),
        events.wait_for_session_subscription(&session.session_id),
    )
    .await
    .map_err(|_| anyhow::anyhow!("timed out waiting for DSH session subscription (15s)"))??;
    let mut collector = DshTurnCollector::new(&session.session_id);
    client
        .session_prompt(&DshPrompt {
            session_id: session.session_id.clone(),
            mode: "queue".to_string(),
            content: vec![DshContentBlock::Text { text: args.message }],
        })
        .await?;
    println!("{}", t("Prompt accepted.", "Prompt 已接受。"));

    let result = timeout(Duration::from_secs(120), async {
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
    .map_err(|_| anyhow::anyhow!("timed out waiting for DSH turn/end (120s)"))??;

    println!("{} {}", t("Turn finished:", "回合结束："), result.reason);
    println!("{}", t("Agent reply:", "Agent 回复："));
    println!(
        "{}",
        if result.text.is_empty() {
            "（无文本）"
        } else {
            &result.text
        }
    );
    Ok(())
}

#[allow(dead_code)]
fn _ensure_result_is_not_empty(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        bail!("DSH returned an empty reply");
    }
    Ok(())
}
