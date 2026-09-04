//! AOE4World MCP stdio server:比赛数据、玩家资料、天梯与胜率分析。
//!
//! 协议:换行分隔的 JSON-RPC 2.0(与 Nonoka 的 MCP 客户端一致)。
//! 数据源:https://aoe4world.com 公开 API(v0)。站点没有公开限流数值,但突发
//! 连发会吃 429,因此出站请求统一走节流 + 指数退避重试 + 磁盘缓存。
//! 知识库:分析结果缓存在 `AOE4WORLD_KB_DIR`(默认 ~/.aoe4world-mcp)。

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BASE: &str = "https://aoe4world.com/api/v0";
/// 站点级端点(不带 /api/v0,如对局摘要)。
const SITE: &str = "https://aoe4world.com";
/// 相邻两个出站请求的最小间隔。站点是社区公益服务,连发即 429。
const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(1200);
/// 429/5xx 最多重试次数(不含首次)。
const MAX_RETRIES: u32 = 3;
/// analyze 结果的新鲜度窗口:窗口内的重复调用直接复用本地知识库。
const ANALYZE_FRESH_SECS: u64 = 30 * 60;

fn kb_dir() -> PathBuf {
    std::env::var_os("AOE4WORLD_KB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".aoe4world-mcp"))
                .unwrap_or_else(|| PathBuf::from("/tmp/.aoe4world-mcp"))
        })
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .user_agent("nonoka-aoe4world-mcp/0.2")
            .timeout(Duration::from_secs(30))
            .build()
            .expect("build http client")
    })
}

fn throttle() {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap();
    if let Some(t) = *last {
        let min = MIN_REQUEST_INTERVAL;
        let elapsed = t.elapsed();
        if elapsed < min {
            std::thread::sleep(min - elapsed);
        }
    }
    *last = Some(Instant::now());
}

/// 429/5xx 重试等待:优先 Retry-After,否则指数退避(2/4/8s)+ 抖动。
/// jitter_nanos 单独成参是为了可测。
fn backoff_delay_ms(attempt: u32, retry_after_secs: Option<u64>, jitter_nanos: u32) -> u64 {
    if let Some(s) = retry_after_secs {
        return s.saturating_mul(1000);
    }
    let base = 2000u64.saturating_mul(1 << attempt.min(4));
    base + (jitter_nanos as u64 % 500)
}

fn cache_ttl(path: &str) -> Duration {
    if path.contains("/search?") {
        Duration::from_secs(60)
    } else if path.contains("/games") {
        Duration::from_secs(120)
    } else {
        Duration::from_secs(300)
    }
}

/// FNV-1a 64bit:路径映射为稳定文件名,跨进程/重启一致。
fn cache_key(path: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let safe: String = path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{}-{hash:016x}.json", &safe[..safe.len().min(40)])
}

fn cache_file(path: &str) -> PathBuf {
    kb_dir().join("cache").join(cache_key(path))
}

fn read_cache(path: &str) -> Option<(u64, Value)> {
    let file = cache_file(path);
    let raw = std::fs::read_to_string(file).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let fetched_at = v.get("fetched_at")?.as_u64()?;
    let body = v.get("body")?.clone();
    Some((now_secs().saturating_sub(fetched_at), body))
}

fn write_cache(path: &str, body: &Value) {
    let file = cache_file(path);
    if file
        .parent()
        .is_some_and(|d| std::fs::create_dir_all(d).is_err())
    {
        return;
    }
    let record = json!({"fetched_at": now_secs(), "body": body});
    let _ = std::fs::write(file, serde_json::to_string(&record).unwrap_or_default());
}

fn http_get_uncached(url: &str, _cache_key_path: &str) -> Result<Value> {
    let client = http_client();
    let mut attempt = 0u32;
    loop {
        throttle();
        let response = client
            .get(url)
            .send()
            .with_context(|| format!("GET {url}"))?;
        let status = response.status();
        if status.is_success() {
            let body = response.text().with_context(|| format!("read {url}"))?;
            return serde_json::from_str(&body).with_context(|| format!("parse JSON from {url}"));
        }
        let retryable = status.as_u16() == 429 || status.is_server_error();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.trim().parse::<u64>().ok());
        if !retryable || attempt >= MAX_RETRIES {
            bail!("GET {url} -> {status}");
        }
        let jitter = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let wait = backoff_delay_ms(attempt, retry_after, jitter);
        std::thread::sleep(Duration::from_millis(wait));
        attempt += 1;
    }
}

fn http_get(path: &str) -> Result<Value> {
    if let Some((age, body)) = read_cache(path) {
        if age < cache_ttl(path).as_secs() {
            return Ok(body);
        }
    }
    match http_get_uncached(&format!("{BASE}{path}"), path) {
        Ok(body) => {
            write_cache(path, &body);
            Ok(body)
        }
        // 限流耗尽/网络故障时降级用过期缓存,好过直接把错误甩给模型。
        Err(e) => match read_cache(path) {
            Some((_, body)) => Ok(body),
            None => Err(e),
        },
    }
}

/// 站点级端点(带 sig 的对局摘要等),不走 /api/v0 前缀。
fn http_get_site(path: &str) -> Result<Value> {
    if let Some((age, body)) = read_cache(path) {
        if age < cache_ttl(path).as_secs() {
            return Ok(body);
        }
    }
    match http_get_uncached(&format!("{SITE}{path}"), path) {
        Ok(body) => {
            write_cache(path, &body);
            Ok(body)
        }
        Err(e) => match read_cache(path) {
            Some((_, body)) => Ok(body),
            None => Err(e),
        },
    }
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
            "name": "analyze_player",
            "description": "玩家历史分析:各模式 rating/排名/胜负,加最近 N 场的胜率、常用与高胜率文明、地图表现、连胜连败。输出结构化报告供 further 分析。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "profile_id": {"type": "integer"},
                    "games_limit": {"type": "integer", "description": "回溯场数,默认 50,最大 200"}
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
            "name": "analyze_game",
            "description": "对局速览:仅阵容(玩家/文明/rating 变化)、地图、时长、版本、胜负。只有用户明确只要\"简单看看阵容/谁跟谁打\"时才用本工具;任何\"分析战报/分析这局/分析对局/战报分析\"及 /分析战报 类请求一律用 analyze_game_full 深度版,不要用本工具凑合。",
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
            "description": "统计各文明胜率、文明对阵胜率、登场率,写入本地知识库供 get_meta 查询。优先使用站点官方聚合数据(2 个请求、全量样本),不可用时降级为抓取天梯玩家近期对局(top_players/games_per_player 仅降级路径生效)。30 分钟内已有结果时直接复用,确需重抓传 force=true;站点限流严格,不要为绕过限流反复调用。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "board": {"type": "string", "enum": ["rm_solo", "rm_team"]},
                    "top_players": {"type": "integer", "minimum": 1, "maximum": 50},
                    "games_per_player": {"type": "integer", "minimum": 1, "maximum": 200},
                    "force": {"type": "boolean", "description": "忽略 30 分钟新鲜度窗口,强制重新统计"}
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
        },
        {
            "name": "civ_stats",
            "description": "文明胜率排行,支持段位维度。rank_level 缺省为全段位,可选 conqueror/diamond/platinum/gold/silver/bronze;board 缺省 rm_solo。中英文文明名混用均可。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "board": {"type": "string", "enum": ["rm_solo", "rm_team", "qm_1v1", "qm_2v2", "qm_3v3", "qm_4v4"]},
                    "rank_level": {"type": "string", "description": "段位过滤,缺省全段位"},
                    "patch": {"type": "string"}
                }
            }
        },
        {
            "name": "kb_lookup",
            "description": "AOE4 知识库查询:文明特性、兵种属性与克制关系、中英文/别名互查,并合并当前版本胜率。用户问\"XX怕什么/XX克制谁/XX文明怎么样/XX是什么\"等兵种或文明问题时必须调用,不要凭记忆回答(版本在变)。query 传中文名(如 皇家骑士/法兰西)或英文 id(如 royal_knight/french)。",
            "inputSchema": {
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }
        },
        {
            "name": "screenshot_stats",
            "description": "截取 aoe4world 统计页面的官方原图并以图片形式发到对话。用户要\"胜率表截图/对阵网格图/热力图/发张图看看\"等任何要图的场景,必须调用本工具。path 支持 stats/rm_solo/matchups、stats/rm_solo/civilizations 等(stats/{board}/matchups|civilizations|maps);board 可换模式;支持查询参数如 rank_level=conqueror。热力网格在页面下方,height 建议 4000+。若截图失败报错,降级调用 civ_stats 以文字+表格给出胜率数据,并向用户说明截图暂时不可用。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "站点相对路径,如 stats/rm_solo/matchups"},
                    "height": {"type": "integer", "description": "画布高度,默认 2400;要完整网格取 4000+"}
                },
                "required": ["path"]
            }
        },
        {
            "name": "analyze_game_full",
            "description": "深度战报(默认战报入口):用户说\"分析战报/分析这局/分析对局/战报分析\"或 /分析战报 时必须调用本工具,严禁凭旧对话记忆回答或用网页抓取凑合。链接带 sig 时输出全维度官方数据:每玩家 APM/击杀/战损/生产、Comparison 四维分数(经济/军事/科技/社会)、资源采集细分、时代升级 timing(封建/城堡/帝王)、Build Order 建造出兵序列、MVP 与战犯候选;无 sig 自动降级为 API 战报。本工具已内置节流/缓存/退避,历史 429 不代表现在失败,必须实际调用。只用用户分享的链接,禁止批量抓取。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "share_url": {"type": "string", "description": "用户分享的对局链接(带 sig 则出深度版)"},
                    "profile_id": {"type": "integer", "description": "或分别传:玩家 id"},
                    "game_id": {"type": "integer"},
                    "sig": {"type": "string"}
                }
            }
        },
        {
            "name": "update_knowledge",
            "description": "更新本地知识数据:重新拉取当前版本官方胜率(全段位+各段位),报告知识库各部分的版本与数据年龄。用户要求更新或查询结果显示数据过期时调用。",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }
    ]})
}

fn summarize_search(body: &Value) -> String {
    let total = body.get("total_count").and_then(Value::as_i64).unwrap_or(0);
    let players = body
        .get("players")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = format!("命中 {total} 名玩家,本页 {} 名:\n", players.len());
    for p in players.iter().take(15) {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
        let id = p
            .get("profile_id")
            .map(Value::to_string)
            .unwrap_or_default();
        let rating = p
            .get("modes")
            .and_then(|m| m.get("rm_solo"))
            .and_then(|m| m.get("rating"))
            .map(Value::to_string)
            .unwrap_or_else(|| "-".into());
        let last = p.get("last_game_at").and_then(Value::as_str).unwrap_or("-");
        out.push_str(&format!(
            "- {name} (id={id}) rating={rating} 最近比赛={last}\n"
        ));
    }
    out
}

fn summarize_player(body: &Value) -> String {
    let name = body.get("name").and_then(Value::as_str).unwrap_or("?");
    let id = body
        .get("profile_id")
        .map(Value::to_string)
        .unwrap_or_default();
    let mut out = format!("玩家 {name} (profile_id={id})\n");
    if let Some(modes) = body.get("modes").and_then(Value::as_object) {
        for (mode, m) in modes {
            let rating = m
                .get("rating")
                .map(Value::to_string)
                .unwrap_or_else(|| "-".into());
            let rank = m
                .get("rank")
                .map(Value::to_string)
                .unwrap_or_else(|| "-".into());
            let level = m.get("rank_level").and_then(Value::as_str).unwrap_or("-");
            let w = m
                .get("wins_count")
                .map(Value::to_string)
                .unwrap_or_else(|| "-".into());
            let l = m
                .get("losses_count")
                .map(Value::to_string)
                .unwrap_or_else(|| "-".into());
            out.push_str(&format!(
                "- {mode}: rating={rating} rank={rank} level={level} 胜{w}/负{l}\n"
            ));
        }
    }
    out
}

fn summarize_games(body: &Value) -> String {
    let total = body.get("total_count").and_then(Value::as_i64).unwrap_or(0);
    let games = body
        .get("games")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
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
        out.push_str(&format!(
            "- game={id} {kind} {map} {started}: {}\n",
            players.join(" vs ")
        ));
    }
    out
}

fn summarize_game(body: &Value) -> String {
    let id = body
        .get("game_id")
        .map(Value::to_string)
        .unwrap_or_default();
    let map = body.get("map").and_then(Value::as_str).unwrap_or("-");
    let kind = body.get("kind").and_then(Value::as_str).unwrap_or("-");
    let duration = body.get("duration").and_then(Value::as_i64);
    let patch = body
        .get("patch")
        .map(Value::to_string)
        .unwrap_or_else(|| "-".into());
    let mut out = format!(
        "对局 {id}: {kind} 地图={map} patch={patch} 时长={:?}秒\n",
        duration
    );
    if let Some(teams) = body.get("teams").and_then(Value::as_array) {
        for (ti, team) in teams.iter().enumerate() {
            for slot in team.as_array().cloned().unwrap_or_default() {
                if let Some(p) = slot.get("player") {
                    out.push_str(&format!(
                        "- 队{} {} (id={}) 文明={} rating={} mmr={} 结果={}\n",
                        ti + 1,
                        p.get("name").and_then(Value::as_str).unwrap_or("?"),
                        p.get("profile_id")
                            .map(Value::to_string)
                            .unwrap_or_default(),
                        p.get("civilization").and_then(Value::as_str).unwrap_or("?"),
                        p.get("rating")
                            .map(Value::to_string)
                            .unwrap_or_else(|| "-".into()),
                        p.get("mmr")
                            .map(Value::to_string)
                            .unwrap_or_else(|| "-".into()),
                        p.get("result").and_then(Value::as_str).unwrap_or("进行中"),
                    ));
                }
            }
        }
    }
    out
}

/// 战报:单局端点的玩家字段直接挂在槽位上(与列表端点的 player 嵌套不同)。
fn analyze_game(game_id: i64) -> Result<String> {
    let g: Value = http_get(&format!("/games/{game_id}"))?;
    let kind = g.get("kind").and_then(Value::as_str).unwrap_or("-");
    let map = g.get("map").and_then(Value::as_str).unwrap_or("-");
    let server = g.get("server").and_then(Value::as_str).unwrap_or("-");
    let patch = g
        .get("patch")
        .map(Value::to_string)
        .unwrap_or_else(|| "-".into());
    let duration = g.get("duration").and_then(Value::as_i64).unwrap_or(0);
    let avg = g.get("average_rating").and_then(Value::as_i64);
    let mut text = format!(
        "战报 对局{game_id}:{kind} 地图={map} patch={patch} 时长={}分{}秒 服务器={server} 平均rating={}\n",
        duration / 60,
        duration % 60,
        avg.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
    );
    let teams = g
        .get("teams")
        .and_then(Value::as_array)
        .context("对局没有队伍数据")?;
    for (ti, team) in teams.iter().enumerate() {
        let result = team
            .as_array()
            .and_then(|arr| {
                arr.iter()
                    .filter_map(|s| s.get("result").and_then(Value::as_str))
                    .find(|r| *r == "win" || *r == "loss")
            })
            .unwrap_or("-");
        let outcome = match result {
            "win" => "胜",
            "loss" => "负",
            _ => "进行中",
        };
        text.push_str(&format!("队{}({outcome}):\n", ti + 1));
        for slot in team.as_array().cloned().unwrap_or_default() {
            let name = slot.get("name").and_then(Value::as_str).unwrap_or("?");
            let civ = slot
                .get("civilization")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let rating = slot.get("rating").and_then(Value::as_i64);
            let diff = slot.get("rating_diff").and_then(Value::as_i64);
            let country = slot.get("country").and_then(Value::as_str).unwrap_or("");
            let rating_txt = match (rating, diff) {
                (Some(r), Some(d)) => format!("{r}({d:+})"),
                (Some(r), None) => r.to_string(),
                _ => "-".into(),
            };
            text.push_str(&format!(
                "- {name}[{civ}] rating={rating_txt} 国别={country}\n"
            ));
        }
    }
    Ok(text)
}

/// 玩家历史分析:模式面来自 profile,近场统计来自 games 列表(按场聚合)。
fn analyze_player(profile_id: i64, games_limit: usize) -> Result<String> {
    let profile: Value = http_get(&format!("/players/{profile_id}"))?;
    let name = profile
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let pages = (games_limit / 50).max(1);
    let mut sample: Vec<Value> = Vec::new();
    for page in 1..=pages {
        let body: Value = http_get(&format!("/players/{profile_id}/games?page={page}"))?;
        let Some(games) = body.get("games").and_then(Value::as_array).cloned() else {
            break;
        };
        let fetched = games.len();
        sample.extend(games);
        if fetched < 50 {
            break;
        }
    }
    sample.truncate(games_limit);

    let mut wins = 0usize;
    let mut losses = 0usize;
    let mut civ_games: HashMap<String, (usize, usize)> = HashMap::new();
    let mut map_games: HashMap<String, (usize, usize)> = HashMap::new();
    let mut mode_games: HashMap<String, (usize, usize)> = HashMap::new();
    let mut streaks: Vec<i64> = Vec::new();
    let mut last_game = String::new();
    // games 列表是最新在前;按时间正序算连胜/连败。
    for g in sample.iter().rev() {
        let map = g
            .get("map")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let kind = g
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        if last_game.is_empty() {
            last_game = g
                .get("started_at")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
        }
        let mut my_win: Option<bool> = None;
        let mut my_civ: Option<String> = None;
        for team in g
            .get("teams")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for slot in team.as_array().cloned().unwrap_or_default() {
                let Some(p) = slot.get("player") else {
                    continue;
                };
                if p.get("profile_id").and_then(Value::as_i64) == Some(profile_id) {
                    my_win = p.get("result").and_then(Value::as_str).map(|r| r == "win");
                    my_civ = p
                        .get("civilization")
                        .and_then(Value::as_str)
                        .map(String::from);
                }
            }
        }
        let Some(win) = my_win else { continue };
        if win {
            wins += 1
        } else {
            losses += 1
        }
        if let Some(civ) = &my_civ {
            let e = civ_games.entry(civ.clone()).or_default();
            e.0 += 1;
            if win {
                e.1 += 1
            }
        }
        let e = map_games.entry(map.clone()).or_default();
        e.0 += 1;
        if win {
            e.1 += 1
        }
        let e = mode_games.entry(kind.clone()).or_default();
        e.0 += 1;
        if win {
            e.1 += 1
        }
        // 连胜记 +1,断连(负)则把未结转的连胜清账记 0
        match streaks.last_mut() {
            Some(last) if *last > 0 && win => *last += 1,
            Some(last) if *last < 0 && !win => *last -= 1,
            _ => streaks.push(if win { 1 } else { -1 }),
        }
    }
    let current = streaks.last().copied().unwrap_or(0);
    let best = streaks.iter().max().copied().unwrap_or(0);
    let worst = streaks.iter().min().copied().unwrap_or(0);
    let total = wins + losses;
    let mut text = format!(
        "玩家历史分析 {} (profile_id={profile_id})\n{}\n",
        name,
        summarize_player(&profile)
    );
    text.push_str(&format!(
        "近 {} 场(截至 {}):胜率 {:.1}% ({}胜/{}负);当前{};最长连胜 {best};最长连败 {}\n",
        total,
        last_game,
        if total > 0 {
            wins as f64 * 100.0 / total as f64
        } else {
            0.0
        },
        wins,
        losses,
        if current > 0 {
            format!("{current} 连胜")
        } else if current < 0 {
            format!("{} 连败", -current)
        } else {
            "无连胜连败".into()
        },
        worst.abs(),
    ));
    let render_pairs = |label: &str, stats: &HashMap<String, (usize, usize)>, min_games: usize| {
        let mut rows: Vec<_> = stats.iter().filter(|(_, (g, _))| *g >= min_games).collect();
        rows.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
        let mut line = String::new();
        for (k, (g, w)) in rows.iter().take(6) {
            line.push_str(&format!(
                "- {label}{k}: {:.0}% ({w}/{g})\n",
                *w as f64 * 100.0 / *g as f64
            ));
        }
        line
    };
    let civs = render_pairs("文明 ", &civ_games, 2);
    if !civs.is_empty() {
        text.push_str(&format!("文明表现(按胜率,≥2 场):\n{civs}"));
    }
    let maps = render_pairs("地图 ", &map_games, 2);
    if !maps.is_empty() {
        text.push_str(&format!("地图表现(按胜率,≥2 场):\n{maps}"));
    }
    let modes = render_pairs("模式 ", &mode_games, 1);
    if !modes.is_empty() {
        text.push_str(&format!("模式分布:\n{modes}"));
    }
    Ok(text)
}

// ---- 知识库:人工策展的文明/兵种/克制/中文名,静态知识随二进制走; ----
// ---- 版本强度等动态部分走官方 stats 实时拉取,更新入口见 update_knowledge。----

const KB_VERSION: &str = "2026-09-05.1";

const KB_JSON: &str = r#"{
"civs": {
 "english": {"cn": "英格兰", "trait": "长弓手+网络化经济,农耕城镇中心,防御反击流"},
 "french": {"cn": "法兰西", "trait": "皇家骑士+重甲冲脸,贸易与采石强化,骑士流核心"},
 "holy_roman_empire": {"cn": "神圣罗马帝国", "trait": "预置强化步兵,宗教增益采金,迈因维克宫快速上城"},
 "chinese": {"cn": "中国", "trait": "朝代加成+火药,官员收税,建造加速,运营流"},
 "zhu_xi_legacy": {"cn": "朱熹遗产", "trait": "朱熹遗惠变体,帝国卫队与火矛,机动火药流"},
 "delhi_sultanate": {"cn": "德里苏丹国", "trait": "免费科技靠学者驻守,战象,宗教步兵"},
 "mongols": {"cn": "蒙古", "trait": "全游牧搬家,石资源翻倍,早期双矿暴兵,可汗号角"},
 "rus": {"cn": "罗斯", "trait": "狩猎赏金+射击军,木教堂金门,骑射与板甲骑士"},
 "abbasid_dynasty": {"cn": "阿拔斯王朝", "trait": "智慧宫黄金时代,骆驼减伤光环,步兵文明"},
 "ayyubids": {"cn": "阿尤布王朝", "trait": "阿拔斯变体,德里式科技雇佣,骆驼冲锋与突厥弓骑"},
 "ottomans": {"cn": "奥斯曼", "trait": "免费军事学校产兵,大教习加成,火炮与耶尼切里"},
 "malians": {"cn": "马里", "trait": "黄金矿牛群经济,步枪兵与多兵营暴兵,袭扰流"},
 "byzantines": {"cn": "拜占庭", "trait": "橄榄 oil 雇佣兵,瓦兰吉卫队与具装,水塔增益,全能变阵"},
 "japanese": {"cn": "日本", "trait": "旗帜要塞+驻扎建造加速,足轻与武士,防御反击流"},
 "order_of_the_dragon": {"cn": "龙骑士团", "trait": "神圣罗马变体,兵贵精不贵多,双倍成本精英部队"},
 "knights_templar": {"cn": "圣殿骑士团", "trait": "法兰西变体,圣殿武士+修道院体系,据点防御流"},
 "house_of_lancaster": {"cn": "兰开斯特家族", "trait": "英格兰变体,契约城堡体系,长弓与贵族骑兵"},
 "sengoku_daimyo": {"cn": "战国大名", "trait": "日本变体,大名旗帜,足轻铁道众暴兵"},
 "macedonian_dynasty": {"cn": "马其顿王朝", "trait": "拜占庭变体,瓦兰吉具装强化, frozen 经济爆发"},
 "golden_horde": {"cn": "金帐汗国", "trait": "蒙古变体,突厥化部队,暴骑兵袭扰"},
 "jeanne_darc": {"cn": "圣女贞德", "trait": "英雄单位养成流,贞德亲率大军,法兰西变体"},
 "tughlaq_dynasty": {"cn": "图格鲁克王朝", "trait": "德里变体,攻城象与塔楼,步象体系"},
 "jin_dynasty": {"cn": "金朝", "trait": "中国变体,铁浮屠重骑与火器,工程器械强化"}
},
"units": {
 "villager": {"cn": "村民", "cls": "经济", "strong": [], "weak": "所有军事单位"},
 "scout": {"cn": "侦察兵", "cls": "骑兵", "strong": ["弓箭手"], "weak": ["长矛兵"]},
 "spearman": {"cn": "长矛兵", "cls": "近战步兵", "strong": ["骑兵"], "weak": ["弓箭手", "弩手"]},
 "man_at_arms": {"cn": "武装士兵", "cls": "重步兵", "strong": ["长矛兵", "骑兵(承受)", "弩手(承受)"], "weak": ["弓箭手", "弩手", "投石机"]},
 "archer": {"cn": "弓箭手", "cls": "远程步兵", "strong": ["长矛兵", "武装士兵", "轻甲单位"], "weak": ["骑马兵", "骑士", "投石机"]},
 "crossbowman": {"cn": "弩手", "cls": "远程步兵", "strong": ["重甲单位", "骑士", "具装骑兵"], "weak": ["骑马兵", "弓箭手对射", "投石机"]},
 "longbowman": {"cn": "长弓手", "cls": "远程步兵", "strong": ["长矛兵", "密集步兵"], "weak": ["骑兵", "投石机"]},
 "horseman": {"cn": "骑马兵", "cls": "轻骑兵", "strong": ["弓箭手", "弩手", "远程单位"], "weak": ["长矛兵", "武装士兵"]},
 "knight": {"cn": "骑士", "cls": "重骑兵", "strong": ["远程单位", "弓箭手", "攻城器"], "weak": ["长矛兵", "弩手", "骆驼"]},
 "royal_knight": {"cn": "皇家骑士", "cls": "重骑兵", "strong": ["远程单位", "步兵阵线冲锋"], "weak": ["长矛兵海", "弩手", "僧侣转化"]},
 "cataphract": {"cn": "具装骑兵", "cls": "重骑兵", "strong": ["远程集火承受", "步兵"], "weak": ["长矛兵", "弩手"]},
 "varangian_guard": {"cn": "瓦兰吉卫队", "cls": "精英步兵", "strong": ["步兵对拼", "建筑"], "weak": ["远程集火", "骑兵拉扯"]},
 "ghulam": {"cn": "古拉姆", "cls": "精英步兵", "strong": ["远程单位", "步兵"], "weak": ["长矛兵", "弩手"]},
 "templar_brother": {"cn": "圣殿武士", "cls": "精英步兵", "strong": ["步兵", "建筑"], "weak": ["远程集火", "骑兵拉扯"]},
 "camel_rider": {"cn": "骆驼骑兵", "cls": "轻骑兵", "strong": ["骑兵(减伤光环+反骑)"], "weak": ["长矛兵", "弓箭手"]},
 "camel_archer": {"cn": "骆驼弓骑兵", "cls": "骑射", "strong": ["步兵", "远程对射"], "weak": ["骑马兵", "长矛兵"]},
 "arbaletre": {"cn": "臂弩手", "cls": "远程步兵", "strong": ["重甲单位", "密集目标"], "weak": ["骑兵", "投石机"]},
 "longbowman_en": {"cn": "英格兰长弓手", "cls": "远程步兵", "strong": ["长矛兵"], "weak": ["骑兵"]},
 "streltsy": {"cn": "射击军", "cls": "远程步兵", "strong": ["步兵", "密集目标"], "weak": ["骑兵", "攻城器"]},
 "fire_lancer": {"cn": "火矛骑兵", "cls": "冲锋骑兵", "strong": ["远程单位", "攻城器", "建筑"], "weak": ["长矛兵"]},
 "musketeer": {"cn": "火枪手", "cls": "远程步兵", "strong": ["步兵", "重甲"], "weak": ["骑兵", "投石机"]},
 "springald": {"cn": "扭力弩", "cls": "攻城器", "strong": ["攻城器", "精英单位"], "weak": ["骑兵近身", "步兵"]},
 "mangonel": {"cn": "投石机", "cls": "攻城器", "strong": ["密集远程", "弓箭手群"], "weak": ["骑兵近身", "扭力弩"]},
 "ribauldequin": {"cn": "管风琴炮", "cls": "攻城器", "strong": ["密集步兵"], "weak": ["骑兵", "扭力弩"]},
 "bombard": {"cn": "火炮", "cls": "攻城器", "strong": ["建筑", "城墙", "密集单位"], "weak": ["骑兵近身", "扭力弩"]},
 "battering_ram": {"cn": "攻城槌", "cls": "攻城器", "strong": ["建筑", "城门"], "weak": ["火炮", "近战包围"]},
 "trebuchet": {"cn": "重型投石机", "cls": "攻城器", "strong": ["建筑", "地标"], "weak": ["骑兵近身", "扭力弩"]},
 "monk": {"cn": "僧侣", "cls": "宗教", "strong": ["转化Relic与单位"], "weak": ["所有输出单位"]}
},
"counter_rules": [
 "长矛系(长矛兵/弩手外的反骑轴)克制一切骑兵;骑兵克制远程与攻城器;远程克制步兵",
 "弩手/臂弩手对重甲(骑士/具装/瓦兰吉)有伤害加成,是对重骑兵的正解",
 "面对骑士海:长矛兵海+弩手混编;面对弓箭手海:骑马兵绕后+投石机清场",
 "攻城器互克:扭力弩克制投石机/火炮;近战单位贴脸即废攻城器",
 "骆驼对骑兵有光环减伤,是反骑副轴;战象/精英单位怕集火与僧侣转化"
]}"#;

fn kb() -> &'static serde_json::Value {
    static KB: OnceLock<Value> = OnceLock::new();
    KB.get_or_init(|| serde_json::from_str(KB_JSON).expect("parse bundled KB"))
}

fn kb_text() -> &'static Value {
    kb()
}

/// 英文 id/中文名/子串模糊匹配,命中文明或兵种。
fn kb_lookup(query: &str) -> Result<String> {
    let kb = kb_text();
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        bail!("query 不能为空");
    }
    let mut out = String::new();
    // 文明
    let civs = kb
        .get("civs")
        .and_then(Value::as_object)
        .context("KB civs")?;
    for (id, v) in civs {
        let cn = v.get("cn").and_then(Value::as_str).unwrap_or("");
        if q == *id || q == cn.to_lowercase() || id.contains(&q) || cn.contains(query) {
            let trait_ = v.get("trait").and_then(Value::as_str).unwrap_or("");
            let wr = meta_winrate_line(id);
            out.push_str(&format!("【文明】{id} / {cn}\n  特性:{trait_}\n{wr}\n"));
        }
    }
    // 兵种
    let units = kb
        .get("units")
        .and_then(Value::as_object)
        .context("KB units")?;
    for (id, v) in units {
        let cn = v.get("cn").and_then(Value::as_str).unwrap_or("");
        if q == *id || q == cn.to_lowercase() || id.contains(&q) || cn.contains(query) {
            let cls = v.get("cls").and_then(Value::as_str).unwrap_or("-");
            let strong = v
                .get("strong")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("、")
                })
                .unwrap_or_default();
            let weak = v
                .get("weak")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("、")
                })
                .unwrap_or_else(|| {
                    v.get("weak")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string()
                });
            out.push_str(&format!(
                "【兵种】{id} / {cn}({cls})\n  强对抗:{strong}\n  被克制:{weak}\n"
            ));
        }
    }
    if out.is_empty() {
        // 兜底:给出克制总则
        let rules = kb
            .get("counter_rules")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|s| format!("  - {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        out.push_str(&format!("未命中「{query}」,以下是通用克制总则:\n{rules}\n"));
    } else {
        let rules = kb
            .get("counter_rules")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|s| format!("  - {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        out.push_str(&format!("\n通用克制总则:\n{rules}\n"));
    }
    out.push_str(&format!(
        "(知识库策展版本 {KB_VERSION};版本强度数据见 knowledge.json 的 updated_at)"
    ));
    Ok(out)
}

/// 从 knowledge.json 读某文明当前版本胜率(如果有)。
fn meta_winrate_line(civ: &str) -> String {
    let path = kb_dir().join("knowledge.json");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return "  版本胜率:先跑 analyze_win_rates".into();
    };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        return "  版本胜率:知识库损坏".into();
    };
    let rows = v
        .get("civilizations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(row) = rows
        .iter()
        .find(|r| r.get("civ").and_then(Value::as_str) == Some(civ))
    {
        let g = row.get("games").and_then(Value::as_i64).unwrap_or(0);
        let w = row.get("wins").and_then(Value::as_i64).unwrap_or(0);
        let wr = if g > 0 {
            w as f64 * 100.0 / g as f64
        } else {
            0.0
        };
        let patch = v.get("patch").and_then(Value::as_str).unwrap_or("-");
        format!("  版本胜率:{wr:.1}% ({w}/{g}) [patch {patch}]")
    } else {
        "  版本胜率:该文明不在当前统计中".into()
    }
}

/// 段位维度的文明胜率。
fn civ_stats(board: &str, rank_level: Option<&str>, patch: Option<&str>) -> Result<String> {
    let mut path = format!("/stats/{board}/civilizations?");
    if let Some(rl) = rank_level {
        path.push_str(&format!("rank_level={rl}&"));
    }
    if let Some(p) = patch {
        path.push_str(&format!("patch={p}&"));
    }
    let body: Value = http_get(&path)?;
    let rows = body
        .get("data")
        .and_then(Value::as_array)
        .context("stats 无数据")?;
    let patch_used = body.get("patch").and_then(Value::as_str).unwrap_or("-");
    let rl = body
        .get("rank_level")
        .and_then(Value::as_str)
        .unwrap_or("all");
    let civs = kb()
        .get("civs")
        .and_then(Value::as_object)
        .context("KB civs")?;
    let mut text = format!("文明胜率 {board} [段位:{rl}] [patch {patch_used}](按胜率排序):\n");
    let mut rows_v: Vec<_> = rows.iter().collect();
    rows_v.sort_by(|a, b| {
        b.get("win_rate")
            .and_then(Value::as_f64)
            .unwrap_or(0.)
            .partial_cmp(&a.get("win_rate").and_then(Value::as_f64).unwrap_or(0.))
            .unwrap()
    });
    for r in rows_v.iter().take(25) {
        let id = r.get("civilization").and_then(Value::as_str).unwrap_or("?");
        let wr = r.get("win_rate").and_then(Value::as_f64).unwrap_or(0.);
        let pick = r.get("pick_rate").and_then(Value::as_f64).unwrap_or(0.);
        let games = r.get("games_count").and_then(Value::as_i64).unwrap_or(0);
        let cn = civs
            .get(id)
            .and_then(|v| v.get("cn"))
            .and_then(Value::as_str)
            .unwrap_or("-");
        text.push_str(&format!(
            "- {id}({cn}): {wr:.1}% 登场{pick:.1}% 样本{games}\n"
        ));
    }
    Ok(text)
}

fn fmt_secs(s: u64) -> String {
    format!("{}:{:02}", s / 60, s % 60)
}

/// 深度战报:解析对局摘要(用户分享链接),出 timing/MVP/战犯候选。
/// 链接缺 sig 时自动降级为 API 层战报,保证任何形式的请求都有产出。
fn analyze_game_full(
    share_url: Option<&str>,
    profile_id: Option<i64>,
    game_id: Option<i64>,
    sig: Option<&str>,
) -> Result<String> {
    let (pid, gid, sig_v) = match share_url {
        Some(url) => {
            let re_pid = url
                .split("/players/")
                .nth(1)
                .and_then(|x| x.split('-').next())
                .and_then(|x| x.parse::<i64>().ok());
            let re_gid = url
                .split("/games/")
                .nth(1)
                .and_then(|x| x.split(&['?', '/'][..]).next())
                .and_then(|x| x.parse::<i64>().ok());
            let re_sig = url
                .split("sig=")
                .nth(1)
                .map(|x| x.split('&').next().unwrap_or(x).to_string())
                .filter(|s| !s.is_empty());
            match (re_pid, re_gid) {
                (Some(p), Some(g)) => (p, g, re_sig),
                _ => bail!(
                    "分享链接格式不对,需要 aoe4world.com/players/<id>-/games/<id>?sig=... 完整链接"
                ),
            }
        }
        None => match (profile_id, game_id) {
            (Some(p), Some(g)) => (p, g, sig.map(String::from)),
            _ => bail!("需要 share_url 或 profile_id+game_id(+sig)"),
        },
    };
    let Some(sig_v) = sig_v else {
        let note =
            "对局链接未带 sig(深度时间线需要游戏内分享出的带 sig 链接),已降级为 API 层战报:\n\n";
        return Ok(format!("{note}{}", analyze_game(gid)?));
    };
    let body: Value = http_get_site(&format!(
        "/players/{pid}-/games/{gid}/summary?camelize=true&sig={sig_v}"
    ))?;
    let civs = kb()
        .get("civs")
        .and_then(Value::as_object)
        .context("KB civs")?;
    let map = body.get("mapName").and_then(Value::as_str).unwrap_or("-");
    let lb = body
        .get("leaderboard")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let dur = body.get("duration").and_then(Value::as_i64).unwrap_or(0);
    let reason = body.get("winReason").and_then(Value::as_str).unwrap_or("-");
    let mut text = format!(
        "深度战报 对局{gid}:{lb} 地图={map} 时长={}分{}秒 结束原因={reason}\n",
        dur / 60,
        dur % 60
    );
    let players = body
        .get("players")
        .and_then(Value::as_array)
        .context("无玩家")?;
    struct Row {
        name: String,
        civ: String,
        team: i64,
        result: String,
        apm: i64,
        sqkill: i64,
        sqlost: i64,
        sqprod: i64,
        gathered: i64,
        score_total: i64,
        score_mil: i64,
        inactive: i64,
        feudal: Option<i64>,
        castle: Option<i64>,
        imperial: Option<i64>,
    }
    let mut rows = Vec::new();
    for p in players {
        let stats = p.get("_stats").cloned().unwrap_or(json!({}));
        let g = |k: &str| stats.get(k).and_then(Value::as_i64).unwrap_or(0);
        let acts = p.get("actions").cloned().unwrap_or(json!({}));
        let ts = |k: &str| {
            acts.get(k)
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(Value::as_i64)
        };
        let gathered = p
            .get("totalResourcesGathered")
            .and_then(|r| r.get("total"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let scores = p.get("scores").cloned().unwrap_or(json!({}));
        let civ_id = p
            .get("civilization")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_string();
        let civ_cn = civs
            .get(&civ_id)
            .and_then(|v| v.get("cn"))
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_string();
        rows.push(Row {
            name: p
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
            civ: civ_cn,
            team: p.get("team").and_then(Value::as_i64).unwrap_or(0),
            result: p
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string(),
            apm: p.get("apm").and_then(Value::as_i64).unwrap_or(0),
            sqkill: g("sqkill"),
            sqlost: g("sqlost"),
            sqprod: g("sqprod"),
            gathered,
            score_total: scores.get("total").and_then(Value::as_i64).unwrap_or(0),
            score_mil: scores.get("military").and_then(Value::as_i64).unwrap_or(0),
            inactive: g("inactperiod"),
            feudal: ts("feudalAge"),
            castle: ts("castleAge"),
            imperial: ts("imperialAge"),
        });
    }
    let age = |t: &Option<i64>| t.map(|v| fmt_secs(v as u64)).unwrap_or("-".into());
    for r in &rows {
        text.push_str(&format!(
            "- {}[{}](队{}) {} | APM{} 击杀{} 战损{} 生产{} | 采集{} 总分{}(军{}) | 挂机{}s | 时代 封建{} 城堡{} 帝王{}\n",
            r.name, r.civ, r.team, r.result, r.apm, r.sqkill, r.sqlost, r.sqprod,
            r.gathered, r.score_total, r.score_mil, r.inactive,
            age(&r.feudal), age(&r.castle), age(&r.imperial),
        ));
    }
    // Comparison 面板:四维分数对比(经济/军事/科技/社会),官网 Comparison 同源
    text.push_str("\n【Comparison 四维分数】\n");
    for p in players {
        let scores = p.get("scores").cloned().unwrap_or(json!({}));
        let s = |k: &str| scores.get(k).and_then(Value::as_i64).unwrap_or(0);
        let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
        let res = p.get("totalResourcesGathered").cloned().unwrap_or(json!({}));
        let rg = |k: &str| res.get(k).and_then(Value::as_i64).unwrap_or(0);
        text.push_str(&format!(
            "- {} 总{} 经济{} 军事{} 科技{} 社会{} | 资源 食{} 木{} 金{} 石{}\n",
            name, s("total"), s("economy"), s("military"), s("technology"), s("society"),
            rg("food"), rg("wood"), rg("gold"), rg("stone"),
        ));
    }
    // Build Order 面板:每玩家前 12 条建造/出兵序列(时间+条目+数量)
    text.push_str("\n【Build Order 关键节点】(前 12 条,完整序列过长省略中后段)\n");
    for p in players {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
        let bo = p.get("buildOrder").and_then(Value::as_array);
        let Some(entries) = bo else { continue };
        // 按 finished 首次时间排序的扁平事件
        let mut events: Vec<(i64, String, i64)> = Vec::new();
        for e in entries {
            let icon = e.get("icon").and_then(Value::as_str).unwrap_or("");
            let label = icon.rsplit('/').next().unwrap_or("?");
            let etype = e.get("type").and_then(Value::as_str).unwrap_or("");
            if matches!(etype, "Animal" | "Herdable") {
                continue;
            }
            let firsts = e.get("finished").and_then(Value::as_array);
            if let Some(f) = firsts {
                if let Some(Some(t0)) = f.first().map(|x| x.as_i64()) {
                    let count = f.len() as i64;
                    let destroyed = e.get("destroyed").and_then(Value::as_array).map(|d| d.len() as i64).unwrap_or(0);
                    let tag = match etype {
                        "Unit" => format!("{label}×{count}(损{destroyed})"),
                        _ => format!("{label}"),
                    };
                    events.push((t0, tag, if etype == "Unit" { 1 } else { 0 }));
                }
            }
        }
        events.sort_by_key(|(t, _, _)| *t);
        text.push_str(&format!("- {name}: "));
        for (t, tag, _) in events.iter().take(12) {
            text.push_str(&format!("{}:{} ", fmt_secs(*t as u64), tag));
        }
        text.push('\n');
    }
    // MVP/战犯只适用于多人局:1v1 没有团队语境,胜负即全部信息。
    let team_size = rows.iter().filter(|r| r.team == rows.first().map(|r| r.team).unwrap_or(0)).count();
    let is_team_game = team_size >= 2 && rows.len() >= 4;
    if is_team_game {
        // 粗判:胜方综合分最高者 = MVP 候选;负方战损最高/击杀最低者 = 战犯候选
        let mvp = rows
            .iter()
            .filter(|r| r.result == "win")
            .max_by_key(|r| r.sqkill * 2 + r.sqprod - r.sqlost + (r.gathered / 2000) as i64);
        let criminal = rows
            .iter()
            .filter(|r| r.result == "loss")
            .min_by_key(|r| r.sqkill * 2 - r.sqlost * 2 + (r.gathered / 2000) as i64);
        if let Some(m) = mvp {
            text.push_str(&format!(
                "\nMVP 候选:{}[{}](击杀{} 战损{} 采集{})\n",
                m.name, m.civ, m.sqkill, m.sqlost, m.gathered
            ));
        }
        if let Some(c) = criminal {
            text.push_str(&format!(
                "战犯候选:{}[{}](击杀{} 战损{} 采集{})\n",
                c.name, c.civ, c.sqkill, c.sqlost, c.gathered
            ));
        }
    }
    text.push_str("\n【输出要求】现在直接输出完整深度复盘,不要反问用户是否需要深挖。回复必须包含以下层次,每层都要引用上面的具体数据:\n");
    text.push_str("1.胜负归因:赢了赢在哪、输了输在哪——结合兵种组合、Comparison 四维分差、关键团战的击杀战损比\n");
    text.push_str("2.timing复盘:对照双方封建/城堡/帝王时间与 Build Order 节点,指出决定胜负的关键 timing 窗口(谁的什么兵在什么时间点成型/被压制)\n");
    if is_team_game {
        text.push_str("3.改进清单:给每位玩家1-2条具体可执行的改进(兵种组合/上城时机/集结习惯),战犯要点名说透\n");
    } else {
        text.push_str("3.改进清单:给双方玩家各1-2条具体可执行的改进(兵种组合/上城时机/运营细节)。1v1 没有 MVP/战犯概念,不要使用这类标签,聚焦对局内容本身\n");
    }
    Ok(text)
}

/// 更新动态知识:重拉官方聚合胜率(两个常用 board),报告各部分版本与年龄。
fn update_knowledge() -> Result<String> {
    let mut out = String::from("知识库更新:\n");
    for board in ["rm_solo", "qm_1v1"] {
        match analyze_via_stats(board) {
            Ok(_) => out.push_str(&format!("- {board} 官方聚合胜率已刷新\n")),
            Err(e) => out.push_str(&format!("- {board} 刷新失败:{e:#}\n")),
        }
    }
    let age = std::fs::metadata(kb_dir().join("knowledge.json"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .map(|d| format!("{:.0} 分钟前", d.as_secs_f64() / 60.0))
        .unwrap_or_else(|| "从未".into());
    out.push_str(&format!(
        "- 策展知识(文明/兵种/克制/中文名):版本 {KB_VERSION},随程序更新\n"
    ));
    out.push_str(&format!("- 动态胜率数据:{age}\n"));
    Ok(out)
}

/// 截图机连接串(AOE4_SHOT_HOST,如 alliance@192.168.64.216);未配置时
/// 用本机 docker。NAS 的 docker 有 PVE 嵌套 sysctl 限制起不了新容器,
/// 部署上必须把截图机指到 minipc。
fn shot_host() -> Vec<String> {
    match std::env::var("AOE4_SHOT_HOST") {
        Ok(host) if !host.trim().is_empty() => {
            vec![
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                "-o".to_string(),
                "ConnectTimeout=8".to_string(),
                "-o".to_string(),
                "StrictHostKeyChecking=accept-new".to_string(),
                host.trim().to_string(),
            ]
        }
        _ => Vec::new(),
    }
}

/// 截图管线:headless chromium 截官网页面,PNG 回传。默认本机 docker,
/// 配置 AOE4_SHOT_HOST 后经 SSH 在远端截图机执行。
fn screenshot_stats(path: &str, height: u32) -> Result<Vec<Value>> {
    let safe_path = path.trim().trim_start_matches('/');
    if safe_path.is_empty()
        || !safe_path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "=&?/_-.".contains(c))
    {
        bail!("path 只允许站点相对路径(字母数字与 =&?/_-.),拒绝: {path}");
    }
    if !safe_path.starts_with("stats/") {
        bail!("只允许截 stats/ 下的统计页,拒绝: {path}");
    }
    let height = height.clamp(800, 6000);
    let url = format!("https://aoe4world.com/{safe_path}");
    let remote_png = "/tmp/aoe4world-shot.png";
    let script = |budget: u32| {
        format!(
            "timeout 90 docker run --rm --shm-size=256m -v /tmp:/data zenika/alpine-chrome:latest --headless --no-sandbox --disable-gpu --disable-dev-shm-usage --hide-scrollbars --window-size=1360,{height} --virtual-time-budget={budget} --screenshot=/data/aoe4world-shot.png '{url}' >/dev/null 2>&1; test -s {remote_png} && stat -c %s {remote_png}"
        )
    };
    let mut ssh_cmd = std::process::Command::new("ssh");
    let ssh_args = shot_host();
    let is_remote = !ssh_args.is_empty();
    if is_remote {
        ssh_cmd.args(&ssh_args);
        ssh_cmd.arg(script(20000));
    } else {
        ssh_cmd.arg("sh").arg("-c").arg(script(20000));
    }
    let mut output = ssh_cmd
        .output()
        .context("截图执行失败(检查 docker 或 AOE4_SHOT_HOST)")?;
    let mut size: u64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    // 空白页(站点挑战/渲染未完成)自检:过小则加倍预算重试一次。
    if size < 100_000 {
        let mut retry_cmd = std::process::Command::new("ssh");
        if is_remote {
            retry_cmd.args(&ssh_args);
            retry_cmd.arg(script(45000));
        } else {
            retry_cmd.arg("sh").arg("-c").arg(script(45000));
        }
        output = retry_cmd.output().context("截图重试失败")?;
        size = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .unwrap_or(0);
    }
    if size == 0 {
        bail!("截图未生成(页面超时或被站点挑战拦截)");
    }
    if size < 100_000 {
        bail!(
            "截图疑似空白({size} bytes,站点挑战或渲染未完成)。降级方案:改用 civ_stats 工具输出文字版胜率数据"
        );
    }
    let png = if is_remote {
        let fetch = std::process::Command::new("ssh")
            .args(&ssh_args)
            .arg("cat /tmp/aoe4world-shot.png")
            .output()
            .context("回传截图失败")?;
        if !fetch.status.success() || fetch.stdout.is_empty() {
            bail!("回传截图为空");
        }
        fetch.stdout
    } else {
        std::fs::read(remote_png).context("读取截图失败")?
    };
    let b64 = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(&png)
    };
    Ok(vec![
        json!({"type": "text", "text": format!("已截取官方统计页 https://aoe4world.com/{safe_path} ({size} bytes)。直接把图发给用户;若用户要的是热力网格但图里只见表格,可用更大的 height(如 4000)重截。")}),
        json!({"type": "image", "data": b64, "mimeType": "image/png"}),
    ])
}

fn summarize_leaderboard(body: &Value) -> String {
    let name = body.get("name").and_then(Value::as_str).unwrap_or("-");
    let players = body
        .get("players")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = format!("天梯 {name},本页 {} 名:\n", players.len());
    for p in players.iter().take(20) {
        let rank = p
            .get("rank")
            .map(Value::to_string)
            .unwrap_or_else(|| "-".into());
        let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
        let rating = p
            .get("rating")
            .map(Value::to_string)
            .unwrap_or_else(|| "-".into());
        let wr = p
            .get("win_rate")
            .map(Value::to_string)
            .unwrap_or_else(|| "-".into());
        out.push_str(&format!("- #{rank} {name} rating={rating} winrate={wr}\n"));
    }
    out
}

#[derive(Debug, Default, Serialize)]
struct CivStat {
    games: usize,
    wins: usize,
}

fn write_kb_and_report(
    board: &str,
    source: &str,
    patch: &str,
    games_seen: usize,
    civ_stats: &HashMap<String, CivStat>,
    matchup_stats: &HashMap<String, CivStat>,
    progress: &str,
) -> Result<String> {
    let mut civ_rows: Vec<_> = civ_stats.iter().collect();
    civ_rows.sort_by(|x, y| y.1.games.cmp(&x.1.games));
    let total_civ_games: usize = civ_rows.iter().map(|(_, s)| s.games).sum::<usize>() / 2;
    let mut text = format!(
        "胜率分析完成({source},patch {patch}):统计 {games_seen} 场(文明样本 {total_civ_games} 场)\n\n文明登场/胜率:\n"
    );
    for (civ, s) in civ_rows.iter().take(20) {
        let wr = if s.games > 0 {
            s.wins as f64 * 100.0 / s.games as f64
        } else {
            0.0
        };
        let pick = if total_civ_games > 0 {
            s.games as f64 * 100.0 / total_civ_games as f64
        } else {
            0.0
        };
        text.push_str(&format!(
            "- {civ}: {wr:.1}% ({}/{}) 登场率 {pick:.1}%\n",
            s.wins, s.games
        ));
    }
    let mut matchup_rows: Vec<_> = matchup_stats.iter().collect();
    matchup_rows.sort_by(|x, y| y.1.games.cmp(&x.1.games));
    text.push_str("\n文明对阵胜率(前 20):\n");
    for (key, s) in matchup_rows.iter().take(20) {
        let wr = if s.games > 0 {
            s.wins as f64 * 100.0 / s.games as f64
        } else {
            0.0
        };
        text.push_str(&format!("- {key}: {wr:.2}% ({}/{})\n", s.wins, s.games));
    }
    let kb = json!({
        "updated_at": now_secs(),
        "board": board,
        "source": source,
        "patch": patch,
        "games": games_seen,
        "civilizations": civ_stats.iter().map(|(c, s)| json!({"civ": c, "games": s.games, "wins": s.wins})).collect::<Vec<_>>(),
        "matchups": matchup_stats.iter().map(|(k, s)| json!({"matchup": k, "games": s.games, "wins": s.wins})).collect::<Vec<_>>(),
    });
    let dir = kb_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("knowledge.json"),
        serde_json::to_string_pretty(&kb)?,
    )?;
    text.push_str(&format!(
        "\n知识库已更新:{}/knowledge.json\n",
        dir.display()
    ));
    text.push_str(progress);
    Ok(text)
}

/// 主路径:官方聚合端点,2 个请求拿全量样本,几乎不触发限流。
fn analyze_via_stats(board: &str) -> Result<String> {
    let civs: Value = http_get(&format!("/stats/{board}/civilizations"))?;
    let matchups: Value = http_get(&format!("/stats/{board}/matchups"))?;
    let patch = civs
        .get("patch")
        .and_then(Value::as_str)
        .unwrap_or("-")
        .to_string();
    let mut civ_stats: HashMap<String, CivStat> = HashMap::new();
    for row in civs
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(civ) = row.get("civilization").and_then(Value::as_str) else {
            continue;
        };
        let e = civ_stats.entry(civ.to_string()).or_default();
        e.games = row.get("games_count").and_then(Value::as_u64).unwrap_or(0) as usize;
        e.wins = row.get("win_count").and_then(Value::as_u64).unwrap_or(0) as usize;
    }
    let mut matchup_stats: HashMap<String, CivStat> = HashMap::new();
    for row in matchups
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(a) = row.get("civilization").and_then(Value::as_str) else {
            continue;
        };
        let Some(b) = row.get("other_civilization").and_then(Value::as_str) else {
            continue;
        };
        let e = matchup_stats.entry(format!("{a}_vs_{b}")).or_default();
        e.games = row.get("games_count").and_then(Value::as_u64).unwrap_or(0) as usize;
        e.wins = row.get("win_count").and_then(Value::as_u64).unwrap_or(0) as usize;
    }
    if civ_stats.is_empty() {
        bail!("官方聚合端点返回空数据");
    }
    let games_seen: usize = matchup_stats.values().map(|s| s.games).sum();
    write_kb_and_report(
        board,
        "官方聚合",
        &patch,
        games_seen,
        &civ_stats,
        &matchup_stats,
        "",
    )
}

/// 降级路径:抓取天梯前列玩家近期对局自行统计(请求多,仅在官方聚合不可用时使用)。
fn analyze_via_crawl(board: &str, top_players: usize, games_per_player: usize) -> Result<String> {
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
        let Some(id) = p.get("profile_id").and_then(Value::as_i64) else {
            continue;
        };
        let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
        let games: Value = http_get(&format!("/players/{id}/games?page=1"))?;
        let list = games
            .get("games")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for g in list.iter().take(games_per_player) {
            let Some(teams) = g.get("teams").and_then(Value::as_array) else {
                continue;
            };
            let mut civs = Vec::new();
            let mut winner_is_a = None;
            let mut any_finished = false;
            for (ti, team) in teams.iter().enumerate() {
                for slot in team.as_array().cloned().unwrap_or_default() {
                    let Some(pl) = slot.get("player") else {
                        continue;
                    };
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
            if civs.len() < 2 || !any_finished {
                continue;
            }
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
            if winner_is_a {
                e.wins += 1;
            }
            games_seen += 1;
        }
        progress.push_str(&format!("已统计 {name} 的比赛\n"));
    }
    write_kb_and_report(
        board,
        "天梯抓取",
        "-",
        games_seen,
        &civ_stats,
        &matchup_stats,
        &progress,
    )
}

fn analyze_win_rates(
    board: &str,
    top_players: usize,
    games_per_player: usize,
    force: bool,
) -> Result<String> {
    let kb_file = kb_dir().join("knowledge.json");
    if !force && kb_file.is_file() {
        if let Ok(age) = std::fs::metadata(&kb_file)?.modified()?.elapsed() {
            if age.as_secs() < ANALYZE_FRESH_SECS {
                return Ok(format!(
                    "知识库 {:.0} 分钟前刚分析过(窗口 {} 分钟),直接复用本地结果;确需重抓请在 force=true 后重试。\n\n{}",
                    age.as_secs() as f64 / 60.0,
                    ANALYZE_FRESH_SECS / 60,
                    get_meta(None).unwrap_or_else(|e| format!("读取知识库失败:{e}")),
                ));
            }
        }
    }
    match analyze_via_stats(board) {
        Ok(text) => Ok(text),
        Err(stats_err) => match analyze_via_crawl(board, top_players, games_per_player) {
            Ok(text) => Ok(format!(
                "官方聚合端点不可用({stats_err:#}),已降级为天梯玩家抓取。\n{text}"
            )),
            Err(crawl_err) => Err(anyhow::anyhow!(
                "官方聚合端点失败:{stats_err:#};天梯抓取降级也失败:{crawl_err:#}"
            )),
        },
    }
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
        let rows = kb
            .get("civilizations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        match rows
            .into_iter()
            .find(|x| x.get("civ").and_then(Value::as_str) == Some(civ))
        {
            Some(v) => {
                let g = v.get("games").and_then(Value::as_i64).unwrap_or(0);
                let w = v.get("wins").and_then(Value::as_i64).unwrap_or(0);
                text.push_str(&format!(
                    "{civ}: {:.1}% ({w}/{g})\n",
                    if g > 0 {
                        w as f64 * 100.0 / g as f64
                    } else {
                        0.0
                    }
                ));
                if let Some(ms) = kb.get("matchups").and_then(Value::as_array) {
                    for m in ms {
                        let key = m.get("matchup").and_then(Value::as_str).unwrap_or("");
                        if key.starts_with(&format!("{civ}_")) || key.ends_with(&format!("_{civ}"))
                        {
                            let g2 = m.get("games").and_then(Value::as_i64).unwrap_or(0);
                            let w2 = m.get("wins").and_then(Value::as_i64).unwrap_or(0);
                            text.push_str(&format!(
                                "- {key}: {:.1}% ({w2}/{g2})\n",
                                if g2 > 0 {
                                    w2 as f64 * 100.0 / g2 as f64
                                } else {
                                    0.0
                                }
                            ));
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
                text.push_str(&format!(
                    "- {}: {:.1}% ({w}/{g})\n",
                    v.get("civ").and_then(Value::as_str).unwrap_or("?"),
                    if g > 0 {
                        w as f64 * 100.0 / g as f64
                    } else {
                        0.0
                    }
                ));
            }
        }
        if let Some(ms) = kb.get("matchups").and_then(Value::as_array) {
            text.push_str("对阵胜率:\n");
            for m in ms.iter().take(30) {
                let g = m.get("games").and_then(Value::as_i64).unwrap_or(0);
                let w = m.get("wins").and_then(Value::as_i64).unwrap_or(0);
                text.push_str(&format!(
                    "- {}: {:.1}% ({w}/{g})\n",
                    m.get("matchup").and_then(Value::as_str).unwrap_or("?"),
                    if g > 0 {
                        w as f64 * 100.0 / g as f64
                    } else {
                        0.0
                    }
                ));
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
            Ok(summarize_search(&http_get(&format!(
                "/players/search?query={q}&page={page}"
            ))?))
        }
        "get_player" => {
            let id = args
                .get("profile_id")
                .and_then(Value::as_i64)
                .context("profile_id 必填")?;
            Ok(summarize_player(&http_get(&format!("/players/{id}"))?))
        }
        "get_player_games" => {
            let id = args
                .get("profile_id")
                .and_then(Value::as_i64)
                .context("profile_id 必填")?;
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
            let id = args
                .get("game_id")
                .and_then(Value::as_i64)
                .context("game_id 必填")?;
            Ok(summarize_game(&http_get(&format!("/games/{id}"))?))
        }
        "analyze_game" => {
            let id = args
                .get("game_id")
                .and_then(Value::as_i64)
                .context("game_id 必填")?;
            Ok(analyze_game(id)?)
        }
        "analyze_player" => {
            let id = args
                .get("profile_id")
                .and_then(Value::as_i64)
                .context("profile_id 必填")?;
            let limit = args
                .get("games_limit")
                .and_then(Value::as_i64)
                .unwrap_or(50)
                .clamp(1, 200) as usize;
            Ok(analyze_player(id, limit)?)
        }
        "get_leaderboard" => {
            let board = args
                .get("board")
                .and_then(Value::as_str)
                .unwrap_or("rm_solo");
            let page = args.get("page").and_then(Value::as_i64).unwrap_or(1);
            Ok(summarize_leaderboard(&http_get(&format!(
                "/leaderboards/{board}?page={page}"
            ))?))
        }
        "analyze_win_rates" => {
            let board = args
                .get("board")
                .and_then(Value::as_str)
                .unwrap_or("rm_solo");
            let top = args
                .get("top_players")
                .and_then(Value::as_i64)
                .unwrap_or(10)
                .clamp(1, 50) as usize;
            let per = args
                .get("games_per_player")
                .and_then(Value::as_i64)
                .unwrap_or(50)
                .clamp(1, 200) as usize;
            let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
            analyze_win_rates(board, top, per, force)
        }
        "get_meta" => get_meta(args.get("civilization").and_then(Value::as_str)),
        "civ_stats" => {
            let board = args
                .get("board")
                .and_then(Value::as_str)
                .unwrap_or("rm_solo");
            let rl = args.get("rank_level").and_then(Value::as_str);
            let patch = args.get("patch").and_then(Value::as_str);
            civ_stats(board, rl, patch)
        }
        "kb_lookup" => kb_lookup(args.get("query").and_then(Value::as_str).unwrap_or("")),
        "analyze_game_full" => analyze_game_full(
            args.get("share_url").and_then(Value::as_str),
            args.get("profile_id").and_then(Value::as_i64),
            args.get("game_id").and_then(Value::as_i64),
            args.get("sig").and_then(Value::as_str),
        ),
        "update_knowledge" => update_knowledge(),
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
                    "serverInfo": {"name": "aoe4world-mcp", "version": "0.2.0"}
                }
            }),
            "tools/list" => json!({
                "jsonrpc": "2.0", "id": id,
                "result": tools_list()
            }),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                // 图片型工具返回 text+image 多内容块;其余工具单文本。
                let image_tool = match name {
                    "screenshot_stats" => Some(screenshot_stats(
                        arguments.get("path").and_then(Value::as_str).unwrap_or(""),
                        arguments
                            .get("height")
                            .and_then(Value::as_u64)
                            .unwrap_or(2400) as u32,
                    )),
                    _ => None,
                };
                if let Some(result) = image_tool {
                    match result {
                        Ok(content) => {
                            json!({"jsonrpc": "2.0", "id": id, "result": {"content": content, "isError": false}})
                        }
                        Err(error) => {
                            json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": format!("{error:#}")}], "isError": true}})
                        }
                    }
                } else {
                    match call_tool(name, &arguments) {
                        Ok(text) => {
                            json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": text}], "isError": false}})
                        }
                        Err(error) => {
                            json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": format!("{error:#}")}], "isError": true}})
                        }
                    }
                }
            }
            "notifications/initialized" | "notifications/cancelled" => continue,
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            _ => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {method}")}})
            }
        };
        let _ = serde_json::to_writer(&mut out, &response);
        let _ = out.write_all(b"\n");
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_respects_retry_after_over_exponential() {
        assert_eq!(backoff_delay_ms(0, Some(7), 999), 7000);
        assert_eq!(backoff_delay_ms(2, Some(1), 0), 1000);
    }

    #[test]
    fn backoff_grows_exponentially_with_jitter() {
        assert_eq!(backoff_delay_ms(0, None, 0), 2000);
        assert_eq!(backoff_delay_ms(1, None, 123), 4123);
        assert_eq!(backoff_delay_ms(2, None, 0), 8000);
        // 抖动 = jitter_nanos % 500,最大不到 500ms
        assert_eq!(backoff_delay_ms(3, None, 499), 16499);
        assert_eq!(backoff_delay_ms(3, None, u32::MAX), 16000 + 295);
        // attempt 再大也封顶在 16 倍档
        assert_eq!(backoff_delay_ms(9, None, 0), 32000);
    }

    #[test]
    fn cache_key_is_stable_and_collision_free_for_known_paths() {
        let a = cache_key("/players/123/games?page=1");
        let b = cache_key("/players/123/games?page=1");
        let c = cache_key("/players/124/games?page=1");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.ends_with(".json"));
    }

    #[test]
    fn cache_ttl_matches_endpoint_kinds() {
        assert_eq!(
            cache_ttl("/players/search?query=x"),
            Duration::from_secs(60)
        );
        assert_eq!(
            cache_ttl("/players/1/games?page=1"),
            Duration::from_secs(120)
        );
        assert_eq!(
            cache_ttl("/leaderboards/rm_solo?page=1"),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn cache_roundtrip_and_expiry() {
        std::env::set_var("AOE4WORLD_KB_DIR", "/tmp/aoe4world-mcp-test-cache");
        let path = "/players/42/test-roundtrip";
        let file = cache_file(path);
        let _ = std::fs::remove_file(&file);
        assert!(read_cache(path).is_none());

        write_cache(path, &json!({"ok": true}));
        let (age, body) = read_cache(path).expect("cache entry after write");
        assert!(age <= 1);
        assert_eq!(body, json!({"ok": true}));

        // fetched_at 回拨到 TTL 之外后仍可读出(降级路径),但不再算新鲜
        let stale = json!({"fetched_at": now_secs() - 9999, "body": json!({"ok": true})});
        std::fs::write(&file, serde_json::to_string(&stale).unwrap()).unwrap();
        let (age, _) = read_cache(path).expect("stale entry readable");
        assert!(age >= 9999);
        assert!(age >= cache_ttl(path).as_secs());
        let _ = std::fs::remove_file(&file);
    }
}
