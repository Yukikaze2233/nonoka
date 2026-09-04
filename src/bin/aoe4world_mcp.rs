//! AOE4World MCP stdio server:比赛数据、玩家资料、天梯与胜率分析。
//!
//! 协议:换行分隔的 JSON-RPC 2.0(与 Nonoka 的 MCP 客户端一致)。
//! 数据源:https://aoe4world.com 公开 API(v0)。
//! 知识库:分析结果缓存在 `AOE4WORLD_KB_DIR`(默认 ~/.aoe4world-mcp)。

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;

const BASE: &str = "https://aoe4world.com/api/v0";

fn kb_dir() -> PathBuf {
    std::env::var_os("AOE4WORLD_KB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".aoe4world-mcp"))
                .unwrap_or_else(|| PathBuf::from("/tmp/.aoe4world-mcp"))
        })
}

fn http_get(path: &str) -> Result<Value> {
    let url = format!("{BASE}{path}");
    let body = reqwest::blocking::Client::builder()
        .user_agent("nonoka-aoe4world-mcp/0.1")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("build http client")?
        .get(&url)
        .send()
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .text()
        .with_context(|| format!("read {url}"))?;
    serde_json::from_str(&body).with_context(|| format!("parse JSON from {url}"))
}

fn tools_list() -> Value {
    json!({"tools": [
        {
            "name": "search_players",
            "description": "在 AOE4World 按昵称搜索玩家，返回 profile_id、天梯分、最近比赛时间等。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "玩家昵称关键词"},
                    "page": {"type": "integer", "minimum": 1}
                },
                "required": ["query"]
            }
        },
        {
            "name": "get_player",
            "description": "获取玩家资料:各模式 rating、排名、历史赛季、胜场等。",
            "inputSchema": {
                "type": "object",
                "properties": {"profile_id": {"type": "integer"}},
                "required": ["profile_id"]
            }
        },
        {
            "name": "get_player_games",
            "description": "获取玩家最近对局列表,含地图、文明、胜负、rating 变化、对手信息。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "profile_id": {"type": "integer"},
                    "page": {"type": "integer", "minimum": 1},
                    "opponent_profile_id": {"type": "integer"},
                    "since": {"type": "string", "description": "ISO8601 时间,只取该时间之后的比赛"}
                },
                "required": ["profile_id"]
            }
        },
        {
            "name": "get_game",
            "description": "获取单场对局详情:双方文明、rating、MMR、时长、版本等。",
            "inputSchema": {
                "type": "object",
                "properties": {"game_id": {"type": "integer"}},
                "required": ["game_id"]
            }
        },
        {
            "name": "get_leaderboard",
            "description": "获取天梯排行。board: rm_solo(1v1)、rm_team(组队)等。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "board": {"type": "string", "enum": ["rm_solo", "rm_team", "qm_1v1", "qm_2v2", "qm_3v3", "qm_4v4"]},
                    "page": {"type": "integer", "minimum": 1}
                }
            }
        },
        {
            "name": "analyze_win_rates",
            "description": "抓取天梯前列玩家最近对局并统计:各文明胜率、文明对阵胜率、登场率。结果写入本地知识库,供 get_meta 查询。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "board": {"type": "string", "enum": ["rm_solo", "rm_team"]},
                    "top_players": {"type": "integer", "minimum": 1, "maximum": 50},
                    "games_per_player": {"type": "integer", "minimum": 1, "maximum": 200}
                }
            }
        },
        {
            "name": "get_meta",
            "description": "读取本地知识库里最新的打法/胜率分析结果。civilization 可传文明英文 id(如 english、french、zhu_xi_legacy),不传返回全部。",
            "inputSchema": {
                "type": "object",
                "properties": {"civilization": {"type": "string"}}
            }
        }
    ]})
}

fn summarize_search(body: &Value) -> String {
    let total = body.get("total_count").and_then(Value::as_i64).unwrap_or(0);
    let players = body.get("players").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out = format!("命中 {total} 名玩家,本页 {} 名:\n", players.len());
    for p in players.iter().take(15) {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
        let id = p.get("profile_id").map(Value::to_string).unwrap_or_default();
        let rating = p
            .get("modes")
            .and_then(|m| m.get("rm_solo"))
            .and_then(|m| m.get("rating"))
            .map(Value::to_string)
            .unwrap_or_else(|| "-".into());
        let last = p.get("last_game_at").and_then(Value::as_str).unwrap_or("-");
        out.push_str(&format!("- {name} (id={id}) rating={rating} 最近比赛={last}\n"));
    }
    out
}

fn summarize_player(body: &Value) -> String {
    let name = body.get("name").and_then(Value::as_str).unwrap_or("?");
    let id = body.get("profile_id").map(Value::to_string).unwrap_or_default();
    let mut out = format!("玩家 {name} (profile_id={id})\n");
    if let Some(modes) = body.get("modes").and_then(Value::as_object) {
        for (mode, m) in modes {
            let rating = m.get("rating").map(Value::to_string).unwrap_or_else(|| "-".into());
            let rank = m.get("rank").map(Value::to_string).unwrap_or_else(|| "-".into());
            let level = m.get("rank_level").and_then(Value::as_str).unwrap_or("-");
            let w = m.get("wins_count").map(Value::to_string).unwrap_or_else(|| "-".into());
            let l = m.get("losses_count").map(Value::to_string).unwrap_or_else(|| "-".into());
            out.push_str(&format!("- {mode}: rating={rating} rank={rank} level={level} 胜{w}/负{l}\n"));
        }
    }
    out
}

fn summarize_games(body: &Value) -> String {
    let total = body.get("total_count").and_then(Value::as_i64).unwrap_or(0);
    let games = body.get("games").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out = format!("共 {total} 场,本页 {} 场:\n", games.len());
    for g in games.iter().take(20) {
        let id = g.get("game_id").map(Value::to_string).unwrap_or_default();
        let map = g.get("map").and_then(Value::as_str).unwrap_or("-");
        let kind = g.get("kind").and_then(Value::as_str).unwrap_or("-");
        let started = g.get("started_at").and_then(Value::as_str).unwrap_or("-");
        let mut players = Vec::new();
        if let Some(teams) = g.get("teams").and_then(Value::as_array) {
            for team in teams {
                for slot in team.as_array().cloned().unwrap_or_default() {
                    if let Some(p) = slot.get("player") {
                        players.push(format!(
                            "{}[{}]{}",
                            p.get("name").and_then(Value::as_str).unwrap_or("?"),
                            p.get("civilization").and_then(Value::as_str).unwrap_or("?"),
                            match p.get("result").and_then(Value::as_str) {
                                Some(r) => format!("({r})"),
                                None => String::new(),
                            }
                        ));
                    }
                }
            }
        }
        out.push_str(&format!("- game={id} {kind} {map} {started}: {}\n", players.join(" vs ")));
    }
    out
}

fn summarize_game(body: &Value) -> String {
    let id = body.get("game_id").map(Value::to_string).unwrap_or_default();
    let map = body.get("map").and_then(Value::as_str).unwrap_or("-");
    let kind = body.get("kind").and_then(Value::as_str).unwrap_or("-");
    let duration = body.get("duration").and_then(Value::as_i64);
    let patch = body.get("patch").map(Value::to_string).unwrap_or_else(|| "-".into());
    let mut out = format!("对局 {id}: {kind} 地图={map} patch={patch} 时长={:?}秒\n", duration);
    if let Some(teams) = body.get("teams").and_then(Value::as_array) {
        for (ti, team) in teams.iter().enumerate() {
            for slot in team.as_array().cloned().unwrap_or_default() {
                if let Some(p) = slot.get("player") {
                    out.push_str(&format!(
                        "- 队{} {} (id={}) 文明={} rating={} mmr={} 结果={}\n",
                        ti + 1,
                        p.get("name").and_then(Value::as_str).unwrap_or("?"),
                        p.get("profile_id").map(Value::to_string).unwrap_or_default(),
                        p.get("civilization").and_then(Value::as_str).unwrap_or("?"),
                        p.get("rating").map(Value::to_string).unwrap_or_else(|| "-".into()),
                        p.get("mmr").map(Value::to_string).unwrap_or_else(|| "-".into()),
                        p.get("result").and_then(Value::as_str).unwrap_or("进行中"),
                    ));
                }
            }
        }
    }
    out
}

fn summarize_leaderboard(body: &Value) -> String {
    let name = body.get("name").and_then(Value::as_str).unwrap_or("-");
    let players = body.get("players").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out = format!("天梯 {name},本页 {} 名:\n", players.len());
    for p in players.iter().take(20) {
        let rank = p.get("rank").map(Value::to_string).unwrap_or_else(|| "-".into());
        let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
        let rating = p.get("rating").map(Value::to_string).unwrap_or_else(|| "-".into());
        let wr = p.get("win_rate").map(Value::to_string).unwrap_or_else(|| "-".into());
        out.push_str(&format!("- #{rank} {name} rating={rating} winrate={wr}\n"));
    }
    out
}

#[derive(Debug, Default, Serialize)]
struct CivStat {
    games: usize,
    wins: usize,
}

fn analyze_win_rates(board: &str, top_players: usize, games_per_player: usize) -> Result<String> {
    let lb: Value = http_get(&format!("/leaderboards/{board}?page=1"))?;
    let players = lb
        .get("players")
        .and_then(Value::as_array)
        .cloned()
        .context("leaderboard has no players")?;
    let mut civ_stats: HashMap<String, CivStat> = HashMap::new();
    let mut matchup_stats: HashMap<String, CivStat> = HashMap::new();
    let mut games_seen = 0usize;
    let mut progress = String::new();
    for p in players.iter().take(top_players) {
        let Some(id) = p.get("profile_id").and_then(Value::as_i64) else { continue };
        let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
        let games: Value = http_get(&format!("/players/{id}/games?page=1"))?;
        let list = games.get("games").and_then(Value::as_array).cloned().unwrap_or_default();
        for g in list.iter().take(games_per_player) {
            let Some(teams) = g.get("teams").and_then(Value::as_array) else { continue };
            let mut civs = Vec::new();
            let mut winner_is_a = None;
            let mut any_finished = false;
            for (ti, team) in teams.iter().enumerate() {
                for slot in team.as_array().cloned().unwrap_or_default() {
                    let Some(pl) = slot.get("player") else { continue };
                    if let Some(c) = pl.get("civilization").and_then(Value::as_str) {
                        civs.push(c.to_string());
                    }
                    if let Some(r) = pl.get("result").and_then(Value::as_str) {
                        any_finished = true;
                        if ti == 0 {
                            winner_is_a = Some(r == "win");
                        }
                    }
                }
            }
            if civs.len() < 2 || !any_finished { continue; }
            let a = civs[0].clone();
            let b = civs[1].clone();
            let winner_is_a = winner_is_a.unwrap_or(false);
            for c in [&a, &b] {
                let e = civ_stats.entry(c.clone()).or_default();
                e.games += 1;
                if (c == &a && winner_is_a) || (c == &b && !winner_is_a) {
                    e.wins += 1;
                }
            }
            let key = format!("{a}_vs_{b}");
            let e = matchup_stats.entry(key).or_default();
            e.games += 1;
            if winner_is_a { e.wins += 1; }
            games_seen += 1;
        }
        progress.push_str(&format!("已统计 {name} 的比赛\n"));
    }
    let mut civ_rows: Vec<_> = civ_stats.iter().collect();
    civ_rows.sort_by(|x, y| y.1.games.cmp(&x.1.games));
    let total_civ_games: usize = civ_rows.iter().map(|(_, s)| s.games).sum::<usize>() / 2;
    let mut text = format!("胜率分析完成:统计 {games_seen} 场(文明样本 {total_civ_games} 场)\n\n文明登场/胜率:\n");
    for (civ, s) in civ_rows.iter().take(20) {
        let wr = if s.games > 0 { s.wins as f64 * 100.0 / s.games as f64 } else { 0.0 };
        let pick = if total_civ_games > 0 { s.games as f64 * 100.0 / total_civ_games as f64 } else { 0.0 };
        text.push_str(&format!("- {civ}: {wr:.1}% ({}/{}) 登场率 {pick:.1}%\n", s.wins, s.games));
    }
    let mut matchup_rows: Vec<_> = matchup_stats.iter().collect();
    matchup_rows.sort_by(|x, y| y.1.games.cmp(&x.1.games));
    text.push_str("\n文明对阵胜率(前 20):\n");
    for (key, s) in matchup_rows.iter().take(20) {
        let wr = if s.games > 0 { s.wins as f64 * 100.0 / s.games as f64 } else { 0.0 };
        text.push_str(&format!("- {key}: {wr:.2}% ({}/{})\n", s.wins, s.games));
    }
    let kb = json!({
        "updated_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        "board": board,
        "games": games_seen,
        "civilizations": civ_stats.iter().map(|(c, s)| json!({"civ": c, "games": s.games, "wins": s.wins})).collect::<Vec<_>>(),
        "matchups": matchup_stats.iter().map(|(k, s)| json!({"matchup": k, "games": s.games, "wins": s.wins})).collect::<Vec<_>>(),
    });
    let dir = kb_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("knowledge.json"), serde_json::to_string_pretty(&kb)?)?;
    text.push_str(&format!("\n知识库已更新:{}/knowledge.json\n", dir.display()));
    text.push_str(&progress);
    Ok(text)
}

fn get_meta(civ: Option<&str>) -> Result<String> {
    let path = kb_dir().join("knowledge.json");
    if !path.is_file() {
        bail!("知识库为空:先调用 analyze_win_rates 生成");
    }
    let kb: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
    let mut text = format!(
        "AOE4World 知识库(更新于 epoch {}):\n",
        kb.get("updated_at").and_then(Value::as_u64).unwrap_or(0)
    );
    if let Some(civ) = civ {
        let rows = kb.get("civilizations").and_then(Value::as_array).cloned().unwrap_or_default();
        match rows.into_iter().find(|x| x.get("civ").and_then(Value::as_str) == Some(civ)) {
            Some(v) => {
                let g = v.get("games").and_then(Value::as_i64).unwrap_or(0);
                let w = v.get("wins").and_then(Value::as_i64).unwrap_or(0);
                text.push_str(&format!("{civ}: {:.1}% ({w}/{g})\n", if g > 0 { w as f64 * 100.0 / g as f64 } else { 0.0 }));
                if let Some(ms) = kb.get("matchups").and_then(Value::as_array) {
                    for m in ms {
                        let key = m.get("matchup").and_then(Value::as_str).unwrap_or("");
                        if key.starts_with(&format!("{civ}_")) || key.ends_with(&format!("_{civ}")) {
                            let g2 = m.get("games").and_then(Value::as_i64).unwrap_or(0);
                            let w2 = m.get("wins").and_then(Value::as_i64).unwrap_or(0);
                            text.push_str(&format!("- {key}: {:.1}% ({w2}/{g2})\n", if g2 > 0 { w2 as f64 * 100.0 / g2 as f64 } else { 0.0 }));
                        }
                    }
                }
            }
            None => text.push_str(&format!("知识库里没有 {civ} 的记录\n")),
        }
    } else {
        if let Some(civs) = kb.get("civilizations").and_then(Value::as_array) {
            text.push_str("文明胜率:\n");
            for v in civs {
                let g = v.get("games").and_then(Value::as_i64).unwrap_or(0);
                let w = v.get("wins").and_then(Value::as_i64).unwrap_or(0);
                text.push_str(&format!("- {}: {:.1}% ({w}/{g})\n", v.get("civ").and_then(Value::as_str).unwrap_or("?"), if g > 0 { w as f64 * 100.0 / g as f64 } else { 0.0 }));
            }
        }
        if let Some(ms) = kb.get("matchups").and_then(Value::as_array) {
            text.push_str("对阵胜率:\n");
            for m in ms.iter().take(30) {
                let g = m.get("games").and_then(Value::as_i64).unwrap_or(0);
                let w = m.get("wins").and_then(Value::as_i64).unwrap_or(0);
                text.push_str(&format!("- {}: {:.1}% ({w}/{g})\n", m.get("matchup").and_then(Value::as_str).unwrap_or("?"), if g > 0 { w as f64 * 100.0 / g as f64 } else { 0.0 }));
            }
        }
    }
    Ok(text)
}

fn call_tool(name: &str, args: &Value) -> Result<String> {
    match name {
        "search_players" => {
            let q = args.get("query").and_then(Value::as_str).unwrap_or("");
            let page = args.get("page").and_then(Value::as_i64).unwrap_or(1);
            Ok(summarize_search(&http_get(&format!("/players/search?query={q}&page={page}"))?))
        }
        "get_player" => {
            let id = args.get("profile_id").and_then(Value::as_i64).context("profile_id 必填")?;
            Ok(summarize_player(&http_get(&format!("/players/{id}"))?))
        }
        "get_player_games" => {
            let id = args.get("profile_id").and_then(Value::as_i64).context("profile_id 必填")?;
            let page = args.get("page").and_then(Value::as_i64).unwrap_or(1);
            let mut path = format!("/players/{id}/games?page={page}");
            if let Some(opp) = args.get("opponent_profile_id").and_then(Value::as_i64) {
                path.push_str(&format!("&opponent_profile_id={opp}"));
            }
            if let Some(since) = args.get("since").and_then(Value::as_str) {
                path.push_str(&format!("&since={since}"));
            }
            Ok(summarize_games(&http_get(&path)?))
        }
        "get_game" => {
            let id = args.get("game_id").and_then(Value::as_i64).context("game_id 必填")?;
            Ok(summarize_game(&http_get(&format!("/games/{id}"))?))
        }
        "get_leaderboard" => {
            let board = args.get("board").and_then(Value::as_str).unwrap_or("rm_solo");
            let page = args.get("page").and_then(Value::as_i64).unwrap_or(1);
            Ok(summarize_leaderboard(&http_get(&format!("/leaderboards/{board}?page={page}"))?))
        }
        "analyze_win_rates" => {
            let board = args.get("board").and_then(Value::as_str).unwrap_or("rm_solo");
            let top = args.get("top_players").and_then(Value::as_i64).unwrap_or(10).clamp(1, 50) as usize;
            let per = args.get("games_per_player").and_then(Value::as_i64).unwrap_or(50).clamp(1, 200) as usize;
            analyze_win_rates(board, top, per)
        }
        "get_meta" => get_meta(args.get("civilization").and_then(Value::as_str)),
        other => bail!("unknown tool: {other}"),
    }
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) if l.trim().is_empty() => continue,
            Ok(l) => l,
            Err(_) => break,
        };
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "aoe4world-mcp", "version": "0.1.0"}
                }
            }),
            "tools/list" => {
                let mut r = tools_list();
                r["jsonrpc"] = json!("2.0");
                r["id"] = id.clone().unwrap_or(Value::Null);
                r
            }
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                match call_tool(name, &arguments) {
                    Ok(text) => json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": text}], "isError": false}}),
                    Err(error) => json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": format!("{error:#}")}], "isError": true}}),
                }
            }
            "notifications/initialized" | "notifications/cancelled" => continue,
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            _ => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {method}")}}),
        };
        let _ = serde_json::to_writer(&mut out, &response);
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }
}
