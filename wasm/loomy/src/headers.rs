//! 沿用 Loomy ChatId/MsgId/TurnId/traceparent 及用途头，不转发客户端鉴权头。

use agent2api_wasm_sdk::{json, Result, Value};

use super::{sign, CLIENT_VERSION};

/// fx2/fx4 共享浏览器身份（WASM 无共享客户端默认头，每包自带一份常量）。
/// 聊天与图像 plan 共用这一份；绝不能是 `undici`（核心 egress 默认值，会自曝）。
/// login.rs 的 CLIENT_UA 是登录体里的 `ua` 字段，不是 HTTP UA，不能拿来当 User-Agent。
pub const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/147.0.0.0 Safari/537.36";
/// 浏览器固定会带的语言头；缺失它是一处廉价特征。
pub const ACCEPT_LANGUAGE: &str = "zh-CN,zh;q=0.9,en;q=0.8";

fn header(headers: &Value, name: &str) -> Option<String> {
    let value = headers
        .as_object()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))?
        .1
        .as_str()?
        .trim();
    (!value.is_empty()
        && value.len() <= 256
        && value.is_ascii()
        && !value.bytes().any(|byte| byte.is_ascii_control()))
    .then(|| value.to_string())
}

fn valid_traceparent(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();
    let hex = |value: &str, length: usize| {
        value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    parts.len() == 4
        && parts[0] == "00"
        && hex(parts[1], 32)
        && hex(parts[2], 16)
        && hex(parts[3], 2)
        && parts[1].bytes().any(|byte| byte != b'0')
        && parts[2].bytes().any(|byte| byte != b'0')
}

pub fn build(session: &str, incoming: &Value, image: bool) -> Result<Value> {
    let chat_id = match header(incoming, "chatid").or_else(|| header(incoming, "x-session-id")) {
        Some(value) => value,
        None => sign::uuid()?,
    };
    let msg_id = match header(incoming, "msgid").or_else(|| header(incoming, "x-request-id")) {
        Some(value) => value,
        None => sign::uuid()?,
    };
    let turn_id = header(incoming, "turnid").unwrap_or_else(|| msg_id.clone());
    let traceparent = match header(incoming, "traceparent").filter(|value| valid_traceparent(value))
    {
        Some(value) => value.to_ascii_lowercase(),
        None => format!("00-{}-{}-01", sign::random_hex(16)?, sign::random_hex(8)?),
    };
    let mut headers = json!({
        "Content-Type": "application/json;charset=utf-8", "token": session, "Authorization": format!("Bearer {session}"),
        "User-Agent": BROWSER_USER_AGENT,
        "traceparent": traceparent, "ChatId": chat_id, "MsgId": msg_id, "TurnId": turn_id,
        "InvokeOrigin": "user", "X-Loomy-Request-Purpose": if image { "image.generate" } else { "chat.message" },
        "Accept": if image { "application/json" } else { "text/event-stream" }
    });
    if !image {
        headers["loomy-version"] = json!(CLIENT_VERSION);
    }
    Ok(headers)
}
