//! 官方客户端指纹：keyfrom、UA、traceparent、用量归因头 —— **一处定义、全链路引用**。
//!
//! 由原生插件 `crates/providers/src/providers/lobsterai/fingerprint.rs`（fx1）移植。
//! 指纹值与原生完全一致；唯一改动是随机数与版本缓存的 WASM 适配：
//!
//! * 随机数：原生用 `getrandom`；WASM 实例无 WASI、无系统随机源，改为宿主
//!   `random.bytes`（与 `login::uuid` 同一条路）。宿主调用失败时退回
//!   「宿主时钟 + 进程计数器」的 xorshift64 —— 仍保证每次请求不同值，
//!   绝不发全零 traceId（全零在 W3C 里非法）。
//! * 版本缓存：原生 12 小时进程内缓存；WASM 实例每次调用即销毁，
//!   缓存走 `version::resolve` 的宿主 session（TTL ≤ 15 分钟）+ 账号 state
//!   （12 小时），本文件不持有任何跨调用状态。
//! * TLS：不在此列 —— WASM 插件没有自己的网络，TLS 是宿主核心的事。

use std::sync::atomic::{AtomicU64, Ordering};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::Value;
use sha2::{Digest, Sha256};

use agent2api_wasm_sdk::{self as sdk, json};

use super::upstream;

/// 官方渠道名：官网包构建期固化的默认渠道，个人用户的官方客户端恒为该值。
pub const OFFICIAL_KEYFROM: &str = "official";

/// Electron 主版本对应的精确版本号（官方桌面端 devDependency `electron@43.5.0`）。
pub const ELECTRON_VERSION: &str = "43.5.0";

/// Chromium 主版本串（UA 里的 `Chrome/<ver>`）。
///
/// 推法见原生模块头：Electron 主版本与 Chromium 主版本是「每升一版 +2」的关系，
/// `chromium_major = 2 * electron_major + 64`，43 → 150。这是格式推断，不是逐字符实测。
pub const CHROMIUM_VERSION: &str = "150.0.0.0";

/// 查不到版本时用的兜底值：必须是真实发布过的 CalVer（当前官方最新 `2026.11.16`）。
pub const FALLBACK_CLIENT_VERSION: &str = "2026.11.16";

/// 声明的客户端能力 —— **官方四段，恒定、顺序固定、英文逗号无空格**。
///
/// ```text
///   virtual-models-v1           虚拟模型（yd-fast / yd-ultra）解析
///   kimi-k3-agentic-v1          Kimi K3 agentic 运行时配置（模型目录的准入门）
///   thinking-level-control-v1   思考等级控制（`reasoning_effort:"off"` 的前提，缺它 HTTP 500）
///   upstream-error-details-v1   上游错误详情透出
/// ```
pub const CLIENT_CAPABILITIES: &str = "virtual-models-v1,kimi-k3-agentic-v1,thinking-level-control-v1,upstream-error-details-v1";

/// 平台 token：与官方活动位上报的 `platform=win32` 保持一致。
const PLATFORM_TOKEN: &str = "Windows NT 10.0; Win64; x64";

/// 官方桌面端的 `User-Agent`（Electron/Chromium 默认形态）。
///
/// `version` 传**已解析的客户端版本**（与 `X-LobsterAI-Client-Version` 同一个值），
/// 三处版本必须一致。空串时退回兜底版本，**绝不发空 token**。
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
/// 官方规则：去空白 → 转小写 → 必须匹配 `^[a-z0-9_-]{1,64}$`，否则回退 `official`。
/// 额外一条（本仓历史包袱）：旧版本把登录时刻的毫秒时间戳写进了
/// `firstKeyfrom` / `latestKeyfrom`，≥10 位纯数字即判定为旧时间戳并归一为 `official`。
/// 归一放在**读凭证时**（`account::canonical` / `keyfrom_fields`），存量账号也会被纠正。
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
/// 官方每个 Cowork turn 生成一个 `traceId`、turn 内每个模型请求是子 span。
/// 本网关一次转发 = 一个 turn = 一个 traceId + 一个 spanId。
pub struct RequestTrace {
    /// 32 位小写 hex
    pub trace_id: String,
    /// 16 位小写 hex
    pub span_id: String,
}

impl RequestTrace {
    /// 生成一条新的追踪上下文（随机源见模块头：宿主 `random.bytes`，失败退回时钟种子）。
    pub fn new() -> Self {
        let bytes = random_bytes(24).unwrap_or_else(|_| fallback_bytes(24));
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        Self {
            trace_id: hex[..32].to_string(),
            span_id: hex[32..48].to_string(),
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

/// 宿主随机字节（与 `login::uuid` 同一条 `random.bytes` 宿主调用）。
fn random_bytes(length: usize) -> Result<Vec<u8>, String> {
    let value = sdk::host("random.bytes", json!({ "length": length }))
        .map_err(|error| error.message)?;
    let bytes = STANDARD
        .decode(value.as_str().unwrap_or(""))
        .map_err(|_| "宿主随机数据编码无效".to_string())?;
    if bytes.len() != length {
        return Err("宿主随机数据长度无效".to_string());
    }
    Ok(bytes)
}

/// 宿主随机数不可用时的退路：宿主时钟 + 计数器的 xorshift64，只求「不同请求不同值」。
fn fallback_bytes(length: usize) -> Vec<u8> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = sdk::now().max(1) as u64;
    let mut seed = (nanos as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ counter.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ 1;
    if seed == 0 {
        seed = 1;
    }
    (0..length)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed & 0xff) as u8
        })
        .collect()
}

/// `x-lobster-session-id`：官方口径是**会话 id**（`^[a-zA-Z0-9_.:-]{1,128}$`），
/// 同一段对话内的多个 turn 复用它。
///
/// 本网关协议里没有「会话 id」，所以从**请求内容**派生：`账号 + 对话里第一条 user 消息`。
/// 客户端在同一段对话里每次都会重发完整历史，因此第一条 user 消息在一段会话内
/// 恒定 —— 派生的 id 在一段会话内稳定、换一段对话就变。
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
/// 编码口径与原生 `urlencode` 一致（未保留字符集 `-_.~` + 字母数字，其余 `%XX` 大写）。
pub fn user_message_header(message: &str) -> String {
    let truncated: String = message.trim().chars().take(200).collect();
    upstream::query(&truncated)
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

    /// 能力位恒 4 段、顺序固定、逗号无空格。
    #[test]
    fn capabilities_are_four_segments_in_order() {
        assert_eq!(
            CLIENT_CAPABILITIES,
            "virtual-models-v1,kimi-k3-agentic-v1,thinking-level-control-v1,upstream-error-details-v1"
        );
    }

    /// traceparent 是 W3C 格式：`00-<32hex>-<16hex>-01`，且每次请求都不一样。
    /// 注：单测跑在宿主 target 上，宿主 `random.bytes` 不可用时走时钟种子退路，
    /// 形状断言仍成立（两次调用间计数器保证不同值）。
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
