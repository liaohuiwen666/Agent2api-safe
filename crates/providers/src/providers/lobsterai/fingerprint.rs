//! 官方客户端指纹：keyfrom、UA、traceparent、用量归因头 —— **一处定义、全链路引用**。
//!
//! ── 这个文件为什么存在 ──────────────────────────────────────
//! 本家的风控判的是「这条请求是不是官方桌面端发出的」。官方客户端的行为真值
//! （官方源码 + 官方设计文档逐条对拍；对拍依据在 free6589 参考实现的 lobsterai
//! 模块里，来源见 README —— 本仓没有单独的基线文档）
//! 里有四项是**恒定值**，也正是反代最容易写错、写错了还不会报错的地方：
//!
//! ```text
//!   ① keyfrom        官方包恒为 `official`（构建期固化；^[a-z0-9_-]{1,64}$，小写）
//!   ② Capabilities   恒 4 段、英文逗号连接、无空格、顺序固定
//!   ③ 版本同源       body.version / X-LobsterAI-Client-Version / UA 里的
//!                    LobsterAI/<ver> 取自同一份已解析版本；各请求按需携带
//!                    （对话：版本头＋UA；鉴权：body.version＋UA；
//!                    目录：URL＋版本头＋UA），没有一条请求同时带三处
//!   ④ UA            Electron(Chromium) 默认 UA，同时含 LobsterAI/<ver> 与 Electron/<ver>
//! ```
//!
//! 旧实现与此的差异（本次修复的对象）：
//!   · `keyfrom` 写成了**登录时刻的毫秒时间戳**（`mod.rs::login_callback`），
//!     纯数字串虽然能过 `^[a-z0-9_-]{1,64}$`，但「渠道名是 13 位数字」这件事
//!     本身就是一眼可辨的异常；
//!   · Capabilities 只发 2 段（子集），官方恒 4 段；
//!   · UA 写死 `LobsterAI/0.1.0` —— 既不是 Electron/Chromium 形态，也与
//!     `X-LobsterAI-Client-Version` 的 CalVer 对不上；
//!   · 转发路径完全没有 `traceparent` 与用量归因头，而官方每个 turn 都发。
//!
//! ── 证据分级（避免把推断写成实测）──────────────────────────
//! ```text
//!   实测/官方源码可证：keyfrom 取值与语义、Capabilities 四段字面量与顺序、
//!                     版本头名与 CalVer 形态、traceparent 格式与三个用量头名、
//!                     「auth 接口不带 Capabilities 头」
//!   格式推断：        UA 的 Chromium/Electron token 具体版本号 —— 官方客户端
//!                     不覆盖 UA（走 Electron net.fetch 默认值），所以能确定的是
//!                     **形态与 token 组成**，不是逐字符快照。这里的版本号按
//!                     Electron 主版本推出（见 ELECTRON_VERSION 的注释），
//!                     上游若改版只需改这两个常量。
//! ```
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic = "unwind"`（workspace Cargo.toml）：panic 会被请求边界的
//! `catch_unwind` 兜住，不会带走整个插件进程；本文件仍保持零 unwrap/expect/panic。

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::endpoints::{FALLBACK_CLIENT_VERSION, urlencode};

/// 官方渠道名：构建期未传 `KEYFROM` 时固化的默认渠道，官网包恒为该值。
///
/// `firstKeyfrom` / `latestKeyfrom` 都发它 —— 个人用户的官方客户端就是
/// `official`，不存在「用户自己的渠道名」这回事（渠道名是打包方的事）。
pub const OFFICIAL_KEYFROM: &str = "official";

/// Electron 主版本对应的精确版本号（官方桌面端 devDependency `electron@43.5.0`）。
pub const ELECTRON_VERSION: &str = "43.5.0";

/// Chromium 主版本串（UA 里的 `Chrome/<ver>`）。
///
/// 推法：Electron 主版本与 Chromium 主版本是「每升一版 +2」的关系
/// （Electron 28 → Chromium 120、33 → 130、38 → 140 均按此规则），故
/// `chromium_major = 2 * electron_major + 64`，43 → 150。
/// 这是**格式推断**（见模块头的证据分级），不是逐字符实测；官方改版时改这一行。
pub const CHROMIUM_VERSION: &str = "150.0.0.0";

/// 平台 token：与官方活动位上报的 `platform=win32`（`endpoints::SLOT_PLATFORM`）保持一致 ——
/// 同一份客户端不会「UA 说自己是 Linux、活动位说自己是 win32」。
const PLATFORM_TOKEN: &str = "Windows NT 10.0; Win64; x64";

/// 官方桌面端的 `User-Agent`。
///
/// 官方转发路径**不设置**自定义 UA，上游看到的是 Electron(Chromium) 默认 UA：
/// `Mozilla/5.0 (<OS>) AppleWebKit/537.36 (KHTML, like Gecko) LobsterAI/<appVersion>
/// Chrome/<chromium> Electron/<electron> Safari/537.36`。
///
/// `version` 传**已解析的客户端版本**（与 `X-LobsterAI-Client-Version` 同一个值），
/// 三处版本必须一致 —— 这是官方指纹里最容易被反代写崩的一处（旧实现写死
/// `LobsterAI/0.1.0`，与版本头永远对不上）。空串时退回兜底版本，**绝不发空 token**。
pub fn user_agent(client_version: &str) -> String {
    let raw = client_version.trim();
    let version = if raw.is_empty() {
        FALLBACK_CLIENT_VERSION
    } else {
        raw
    };
    format!(
        "Mozilla/5.0 ({PLATFORM_TOKEN}) AppleWebKit/537.36 (KHTML, like Gecko) \
         LobsterAI/{version} Chrome/{CHROMIUM_VERSION} Electron/{ELECTRON_VERSION} Safari/537.36"
    )
}

/// keyfrom 归一：不合官方口径的一律回退 [`OFFICIAL_KEYFROM`]。
///
/// 官方规则（`keyfromAttribution.ts:14-24`）：去空白 → 转小写 → 必须匹配
/// `^[a-z0-9_-]{1,64}$`，否则回退默认渠道 `official`。
///
/// **额外一条（本仓历史包袱）**：旧版本把登录时刻的毫秒时间戳写进了
/// `firstKeyfrom` / `latestKeyfrom`。13 位纯数字能过上面那个正则，但它不是渠道名，
/// 于是这里判定「≥10 位纯数字 = 旧实现的时间戳」并归一为 `official`。
/// 归一放在**读凭证时**（`credentials::from_record`），所以存量账号也会被纠正，
/// 不必等用户重新登录。
pub fn normalize_keyfrom(value: &str) -> String {
    let trimmed = value.trim().to_ascii_lowercase();
    if trimmed.is_empty() || !is_channel_name(&trimmed) || looks_like_legacy_timestamp(&trimmed) {
        return OFFICIAL_KEYFROM.to_string();
    }
    trimmed
}

/// `^[a-z0-9_-]{1,64}$`（入参必须已是小写：调用方先归一）
fn is_channel_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes
            .iter()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

/// 旧实现写进 keyfrom 的时间戳形态：≥10 位纯数字（毫秒或秒级 epoch）。
fn looks_like_legacy_timestamp(value: &str) -> bool {
    value.len() >= 10 && value.bytes().all(|byte| byte.is_ascii_digit())
}

/// 一次请求的 W3C 追踪上下文。
///
/// 官方每个 Cowork turn 生成一个 `traceId`、turn 内每个模型请求是子 span
/// （`llmTrace.ts:9-13` 的注释原文）。本网关是**无状态**的（一次 HTTP 请求 =
/// 一次回答），所以「一次转发 = 一个 turn = 一个 traceId + 一个 spanId」，
/// 这既符合「每 turn 新 traceId」，也不会出现对照表里那条
/// 「同一 traceId 跨 turn 复用」的破绽。
pub struct RequestTrace {
    /// 32 位小写 hex
    pub trace_id: String,
    /// 16 位小写 hex
    pub span_id: String,
}

impl RequestTrace {
    /// 生成一条新的追踪上下文。
    pub fn new() -> Self {
        Self {
            trace_id: random_hex(16),
            span_id: random_hex(8),
        }
    }

    /// `traceparent` 头值：`00-<traceId>-<spanId>-01`（W3C 格式，采样位置 01）。
    pub fn traceparent(&self) -> String {
        format!("00-{}-{}-01", self.trace_id, self.span_id)
    }
}

impl Default for RequestTrace {
    fn default() -> Self {
        Self::new()
    }
}

/// `x-lobster-session-id`：官方口径是**会话 id**（`^[a-zA-Z0-9_.:-]{1,128}$`），
/// 同一段对话内的多个 turn 复用它。
///
/// ── 反代怎么给这个值 ────────────────────────────────────────
/// 本网关的协议里没有「会话 id」（`ChatRequest` 只有一次请求的 body / headers），
/// 所以这里从**请求内容**派生而不是写死常量：`账号 + 对话里第一条 user 消息`。
/// 客户端在同一段对话里每次都会重发完整历史，因此第一条 user 消息在一段会话内
/// 恒定 —— 派生的 id 在一段会话内稳定、换一段对话就变。
///
/// 为什么不直接写死一个常量：对照表第 10 条明确写着「三头静态写死 = 破绽」。
/// 为什么不随机生成：每请求一个新 session 与「session 内多 turn 复用」的官方
/// 语义相反，同样能被看出来。
pub fn session_id(account: &str, first_user_message: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(account.trim().as_bytes());
    hasher.update(b"\x1f");
    hasher.update(first_user_message.trim().as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    // 32 位 hex，满足 `^[a-zA-Z0-9_.:-]{1,128}$`
    hex.chars().take(32).collect()
}

/// `x-lobster-turn-id`：官方口径是**该 turn 的 traceId**（32 hex）。
///
/// 无状态转发里「本次请求」就是「这个 turn」，所以直接复用 [`RequestTrace::trace_id`]。
pub fn turn_id(trace: &RequestTrace) -> String {
    trace.trace_id.clone()
}

/// 请求体里**第一条** user 消息的文本（派生 session id 用；取不到给空串）。
pub fn first_user_message(body: &Value) -> String {
    user_messages(body).into_iter().next().unwrap_or_default()
}

/// 请求体里**最后一条** user 消息的文本（`x-lobster-user-message` 用）。
///
/// 官方口径是「本次 turn 的用户消息」，无状态转发里就是最后一条 user 消息。
pub fn last_user_message(body: &Value) -> Option<String> {
    user_messages(body).pop()
}

/// `x-lobster-user-message` 的头值：用户消息**前 200 个码点**，URL 编码。
///
/// 超长消息按码点截断（不是字节），避免把一个 UTF-8 字符切成半个。
pub fn user_message_header(message: &str) -> String {
    let truncated: String = message.trim().chars().take(200).collect();
    urlencode(&truncated)
}

/// 按顺序取出 body 里所有 user 消息的纯文本（`content` 支持字符串与分段数组）。
fn user_messages(body: &Value) -> Vec<String> {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut collected: Vec<String> = Vec::new();
    for message in messages {
        let Some(entry) = message.as_object() else {
            continue;
        };
        let is_user = entry
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| role.eq_ignore_ascii_case("user"));
        if !is_user {
            continue;
        }
        if let Some(text) = content_text(entry.get("content")) {
            if !text.trim().is_empty() {
                collected.push(text);
            }
        }
    }
    collected
}

/// 把 `content` 折成纯文本：字符串直接用；分段数组只取 `text` 段。
fn content_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let mut joined = String::new();
            for part in parts {
                let text = match part {
                    Value::String(text) => Some(text.clone()),
                    Value::Object(entry) => entry
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    _ => None,
                };
                if let Some(text) = text {
                    joined.push_str(&text);
                }
            }
            if joined.is_empty() { None } else { Some(joined) }
        }
        _ => None,
    }
}

/// 取 `len` 个随机字节的小写 hex（`len` 即字节数，输出长度是它的两倍）。
///
/// 随机源与 `crate::ids` 同一条（`getrandom`）；取不到时退回「时间戳 + 计数器」，
/// 仍然是不可预测度合理的唯一串 —— 绝不发全零 traceId（全零在 W3C 里非法）。
fn random_hex(len: usize) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut bytes = vec![0u8; len];
    if getrandom::getrandom(&mut bytes).is_err() {
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(1);
        let mut seed = nanos ^ counter.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 1;
        for slot in bytes.iter_mut() {
            // xorshift64：只为「不同请求不同值」，不承担密码学强度
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *slot = (seed & 0xff) as u8;
        }
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 官方渠道名就是 `official`；合法渠道名原样保留（小写化）。
    #[test]
    fn keyfrom_follows_official_rules() {
        assert_eq!(normalize_keyfrom(""), OFFICIAL_KEYFROM);
        assert_eq!(normalize_keyfrom("   "), OFFICIAL_KEYFROM);
        assert_eq!(normalize_keyfrom("Official"), "official");
        assert_eq!(normalize_keyfrom("bilibili"), "bilibili");
        assert_eq!(normalize_keyfrom("partner_a"), "partner_a");
        assert_eq!(normalize_keyfrom("中文渠道"), OFFICIAL_KEYFROM);
        assert_eq!(normalize_keyfrom("has space"), OFFICIAL_KEYFROM);
    }

    /// 旧实现的时间戳形态必须被纠正成 `official`（存量账号靠这条生效）。
    #[test]
    fn legacy_timestamp_keyfrom_is_corrected() {
        assert_eq!(normalize_keyfrom("1793869387181"), OFFICIAL_KEYFROM);
        assert_eq!(normalize_keyfrom("1793869387"), OFFICIAL_KEYFROM);
        // 短数字是合法渠道名（如 `q3`），不该被误伤
        assert_eq!(normalize_keyfrom("2026"), "2026");
    }

    /// UA 必须是 Electron/Chromium 形态，且版本 token 跟随传入版本（三处一致）。
    #[test]
    fn user_agent_carries_client_version_and_electron_tokens() {
        let ua = user_agent("2026.11.6");
        assert!(ua.starts_with("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36"));
        assert!(ua.contains("LobsterAI/2026.11.6"), "{ua}");
        assert!(ua.contains("Electron/43.5.0"), "{ua}");
        assert!(ua.contains("Chrome/150.0.0.0"), "{ua}");
        assert!(ua.ends_with("Safari/537.36"), "{ua}");
        // 旧实现那个写死的 token 必须彻底消失
        assert!(!ua.contains("LobsterAI/0.1.0"), "{ua}");
        // 空版本退回兜底常量，绝不发空 token
        assert!(user_agent("").contains(&format!("LobsterAI/{FALLBACK_CLIENT_VERSION}")));
    }

    /// traceparent 是 W3C 格式：`00-<32hex>-<16hex>-01`，且每次请求都不一样。
    #[test]
    fn traceparent_matches_w3c_shape() {
        let trace = RequestTrace::new();
        let value = trace.traceparent();
        let parts: Vec<&str> = value.split('-').collect();
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], "00");
        assert_eq!(parts[1].len(), 32);
        assert_eq!(parts[2].len(), 16);
        assert_eq!(parts[3], "01");
        assert!(parts[1].bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(parts[2].bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(parts[1], "0".repeat(32), "全零 traceId 非法");
        assert_ne!(RequestTrace::new().trace_id, trace.trace_id);
        assert_eq!(turn_id(&trace), trace.trace_id);
    }

    /// session id 由「账号 + 第一条 user 消息」派生：会话内稳定、换会话就变。
    #[test]
    fn session_id_is_content_derived_and_stable() {
        let body = json!({
            "messages": [
                {"role": "system", "content": "you are helpful"},
                {"role": "user", "content": "第一句"},
                {"role": "assistant", "content": "回一句"},
                {"role": "user", "content": "第二句"}
            ]
        });
        let first = first_user_message(&body);
        assert_eq!(first, "第一句");
        let a = session_id("acct-1", &first);
        let b = session_id("acct-1", &first);
        assert_eq!(a, b, "同一段会话必须稳定");
        assert_eq!(a.len(), 32);
        assert_ne!(a, session_id("acct-1", "另一段会话"));
        assert_ne!(a, session_id("acct-2", &first));
    }

    /// `x-lobster-user-message` 取最后一条 user 消息、码点截断 200、URL 编码。
    #[test]
    fn user_message_header_is_last_message_and_percent_encoded() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "旧问题"},
                {"role": "user", "content": [{"type": "text", "text": "新问题 带空格"}]}
            ]
        });
        let last = last_user_message(&body).unwrap_or_default();
        assert_eq!(last, "新问题 带空格");
        assert_eq!(user_message_header(&last), "%E6%96%B0%E9%97%AE%E9%A2%98%20%E5%B8%A6%E7%A9%BA%E6%A0%BC");
        let long = "a".repeat(300);
        assert_eq!(user_message_header(&long).len(), 200);
    }
}
