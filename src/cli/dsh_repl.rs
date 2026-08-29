//! `nonoka dsh-repl`：一个基于 DSH 会话的多轮对话窗口。
//!
//! 轻量终端界面：彩色提示符、启动 banner、等待提示和 Markdown 回复渲染。
//! 不经过旧 Agent/daemon/REPL 渲染管线，只维护一个 DSH session。

use super::{DshReplArgs, NonokaPaths};
use crate::backend::DshBackend;
use crate::i18n::text as t;
use anyhow::Result;
use std::io::{self, IsTerminal, Write};

use crossterm::cursor::MoveToColumn;
use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{execute, queue};

pub(in crate::cli) async fn run_dsh_repl(paths: &NonokaPaths, args: DshReplArgs) -> Result<()> {
    let backend = DshBackend::from_env(args.agent_preset)?;
    let mut session = backend.open_repl_session(paths).await?;

    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    print_banner(&backend, &session.session_id())?;

    let stdin = io::stdin();
    loop {
        print_prompt()?;
        let mut line = String::new();
        let read = stdin.read_line(&mut line)?;
        if read == 0 {
            println!();
            break;
        }
        let line = line.trim();
        if line.eq_ignore_ascii_case("/exit")
            || line.eq_ignore_ascii_case("exit")
            || line.eq_ignore_ascii_case("quit")
        {
            break;
        }
        if line.eq_ignore_ascii_case("/help") || line.eq_ignore_ascii_case("help") {
            print_help();
            continue;
        }
        if line.is_empty() {
            continue;
        }

        if interactive {
            print_waiting()?;
        }

        let reply = session.prompt(line.to_string()).await;

        if interactive {
            clear_waiting()?;
        }

        match reply {
            Ok(text) => {
                crate::render::print_markdown(&text);
            }
            Err(error) => {
                print_error(&format!("{error:#}"))?;
            }
        }
    }

    session.archive().await;
    if interactive {
        println!(
            "\n{}",
            t("DSH session archived. Bye.", "DSH 会话已归档，再见。")
        );
    }
    Ok(())
}

fn print_banner(backend: &DshBackend, session_id: &str) -> Result<()> {
    let base_url = backend.base_url();
    queue!(
        io::stdout(),
        SetForegroundColor(Color::Cyan),
        Print("╭────────────────────────────────────────╮\n"),
        Print("│  Nonoka DSH REPL                      │\n"),
        Print("╰────────────────────────────────────────╯\n"),
        ResetColor,
        Print(format!("{base_url}\n")),
        Print(format!("session: {session_id}\n")),
        Print(t(
            "Type a message and press Enter. /help for commands. /exit quits.",
            "输入消息后回车发送；/help 查看命令；/exit 退出。",
        )),
        Print("\n"),
    )?;
    io::stdout().flush()?;
    Ok(())
}

fn print_prompt() -> Result<()> {
    queue!(
        io::stdout(),
        SetForegroundColor(Color::Magenta),
        Print("nonoka"),
        SetForegroundColor(Color::DarkCyan),
        Print(" ❯ "),
        ResetColor,
    )?;
    io::stdout().flush()?;
    Ok(())
}

fn print_help() {
    println!(
        "{}",
        t(
            "/help   show this help\n/exit   quit and archive the session",
            "/help   显示帮助\n/exit   退出并归档会话"
        )
    );
}

fn print_waiting() -> Result<()> {
    execute!(io::stdout(), MoveToColumn(0), Clear(ClearType::CurrentLine))?;
    queue!(
        io::stdout(),
        SetForegroundColor(Color::DarkGrey),
        Print("⏳ waiting for DSH…"),
        ResetColor,
    )?;
    io::stdout().flush()?;
    Ok(())
}

fn clear_waiting() -> Result<()> {
    execute!(io::stdout(), MoveToColumn(0), Clear(ClearType::CurrentLine))?;
    io::stdout().flush()?;
    Ok(())
}

fn print_error(message: &str) -> Result<()> {
    queue!(
        io::stdout(),
        SetForegroundColor(Color::Red),
        Print(format!("error: {message}\n")),
        ResetColor,
    )?;
    io::stdout().flush()?;
    Ok(())
}
