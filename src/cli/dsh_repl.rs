//! `nonoka dsh-repl`：一个基于 DSH 会话的多轮对话窗口。
//!
//! 这是最小实现：不经过旧 Agent/daemon/REPL 渲染管线。它只维护一个
//! DSH session，把 stdin 行投递为 prompt，并把 `turn/end` 的文本打印
//! 回来。`/exit` 或 EOF 时归档会话。

use super::{DshReplArgs, NonokaPaths};
use crate::backend::DshBackend;
use crate::i18n::text as t;
use anyhow::Result;
use std::io::{self, Write};

pub(in crate::cli) async fn run_dsh_repl(paths: &NonokaPaths, args: DshReplArgs) -> Result<()> {
    let backend = DshBackend::from_env(args.agent_preset)?;
    let mut session = backend.open_repl_session(paths).await?;
    println!(
        "{} {}",
        t("DSH session:", "DSH 会话："),
        session.session_id()
    );
    println!(
        "{}",
        t(
            "Type a message and press Enter. /exit quits.",
            "输入消息后回车发送；/exit 退出。"
        )
    );

    let stdin = io::stdin();
    loop {
        print!("{} ", t("nonoka>", "nonoka>"));
        io::stdout().flush()?;
        let mut line = String::new();
        let read = stdin.read_line(&mut line)?;
        if read == 0 {
            break;
        }
        let line = line.trim();
        if line.eq_ignore_ascii_case("/exit")
            || line.eq_ignore_ascii_case("exit")
            || line.eq_ignore_ascii_case("quit")
        {
            break;
        }
        if line.is_empty() {
            continue;
        }
        let reply = session.prompt(line.to_string()).await?;
        println!("{reply}");
    }

    session.archive().await;
    Ok(())
}

#[allow(dead_code)]
fn _read_line() -> std::result::Result<Option<String>, io::Error> {
    let mut line = String::new();
    let read = io::stdin().read_line(&mut line)?;
    if read == 0 {
        Ok(None)
    } else {
        Ok(Some(line))
    }
}
