//! LobsterAI 的转发：**同方言透传 + 四处上游强制的改写**。
//!
//! ── 为什么这一层也很薄 ──────────────────────────────────────
//! 对话端点是 OpenAI 兼容的原生协议（`POST /api/proxy/v1/chat/completions`），
//! 网关本身就是 OpenAI Chat Completions，所以 body 几乎逐字节上行、上游的 SSE
//! 帧逐字节下发（`executor.go:16-27` 的模块头：语言不同，但「原样透传 + 只改
//! 必须改的」这条口径一样）。剩下的只有四件格式本身表达不了的事。
//!
//! ── 只改四处，每处都有它非改不可的理由（全部实测）────────────────
//!   1. **`role: "developer"` → `"system"`**：上游不认 OpenAI 新版角色，
//!      带它的请求被网关以 **HTTP 502「角色信息不正确」**拒绝（system / user
//!      均 200）。两者在 OpenAI 规范里语义相同，按 `system` 下发
//!      （`executor.go:83-86` 的实测注 + `demoteDeveloperMessages`）。
//!      ⚠️ 本仓的通用转发层在「开发者角色兼容」开关打开时也会做同一件事
//!      （`upstream::payload::send_body` 的 ⓪ 角色归一层，默认开）—— 这里再写
//!      一遍不是冗余：开关关掉时它就是唯一的一道，且对已归一的体是**幂等**的。
//!   2. **`tool_choice` 在 `""` / `"none"` / `null` 时删掉**：客户端可能原样
//!      转发一个上游拒绝的取值（`executor.go:114-123`）。
//!   3. **绝不下发 `prompt_cache_key`**：那是腾讯后端的机制，本家不知道认不认
//!      （`executor.go:63-64` 的原文）；存在就删。
//!   4. **`model` 走一次名字解析**（展示名 / 规范化别名 → 线上 id）：
//!      与 `atomcode/chat.rs` 同一手法，认不出时**原样发**，不猜。
//!      注意通用转发层已经按目录把 body 里的模型名改写成了线上名
//!      （`catalog::wire_target_for_provider`），所以这一步通常是一次幂等的
//!      确认；留着它是为了「客户端直接点名一个规范化别名」那条路。
//!
//! ── 两处**刻意不做**的改写（都是实测结论）──────────────────────
//!   · **不动 `stream`**：本家 `stream:false` 上游直接回 HTTP 500
//!     （`executor.go:18-21`），而本网关的转发入口「无论客户端要不要流式，上游
//!     恒以 `stream:true` 请求」（`core/upstream/mod.rs` 的注释原文）——
//!     这条要求天然满足，**碰它反而会把它弄坏**。
//!   · **不转换图片**：上游只收 `{type:"image_url",image_url:{url}}`，收到
//!     `{type:"image"}` 会回 HTTP 500。我们**不做**形状修补 —— 让上游把它的
//!     真实错误如实报给客户端，好过我们替它猜一种可能不对的形状
//!     （与本仓「广告能力要有实测依据、不编造」的口径一致）。
//!
//! ── 客户端版本头为什么要一个进程级快照 ──────────────────────────
//! `X-LobsterAI-Client-Version` 是功能性头（缺它 / 给空串会被按「不认识的
//! 客户端」对待，见 `endpoints.rs` 的模块头），而版本号要**异步**从上游更新
//! 接口取（`version::resolve_client_version()`）。但 `ProviderAdapter` 的
//! `build_chat_request` 是**同步**的（trait 契约，不能改成 async），所以这里
//! 用一份进程级快照：异步侧（[`prime_client_version`]，由目录刷新 / 签到 /
//! 余额这些本来就要取版本的异步路径调用）写、同步侧（[`client_version_header`]）
//! 读；快照还没填时退回已知可用的兜底常量，**绝不发空版本头**（与 Go 参考
//! `headers.go::resolveHeaderVersion` 同一口径）。
//!
//! ── 转发头集合按官方 `buildUpstreamRequestHeaders` 对齐 ──────────────
//! 官方本地代理是**重建**上游头的（入站头除 `Content-Type` / `Accept` 外一个都
//! 不透传），全集是（`openclawTokenProxy.ts:1237-1261`）：
//!
//! ```text
//!   Authorization: Bearer <accessToken>
//!   Content-Type: <入站值或 application/json>
//!   X-LobsterAI-Client-Capabilities: <官方四段常量>
//!   X-LobsterAI-Client-Version: <app.getVersion() 的 CalVer>
//!   traceparent: 00-<32hex>-<16hex>-01
//!   x-lobster-session-id / x-lobster-turn-id / x-lobster-user-message
//!     （+ 命中该 turn 模型时的 x-lobster-model-preset）
//!   Accept: <入站有才转发>
//!   User-Agent: Electron/Chromium 默认 UA（由 net.fetch 补）
//! ```
//!
//! 旧实现只发前两条 + 2 段能力位 + 写死的 `LobsterAI/0.1.0`，缺 tracking 与
//! 用量归因头 —— 与官方流量对比是「一眼可见」的差异。现在按上表补齐；
//! `x-lobster-model-preset` 仍是**故意不发**（见下文 `build_chat_request` 的注释）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::sync::{Mutex, OnceLock};

use crate::headers::Headers;
use serde_json::Value;

use agent2api_provider_sdk::ChatPlan;

use crate::errors::GatewayError;

use super::endpoints::{CLIENT_CAPABILITIES, chat_url};
use super::fingerprint::{self, RequestTrace};
use super::models;
use super::version;

/// 进程级版本快照（由 [`prime_client_version`] 写、[`client_version_header`] 读）
static CLIENT_VERSION: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn version_slot() -> &'static Mutex<Option<String>> {
    CLIENT_VERSION.get_or_init(|| Mutex::new(None))
}

/// 解析一次客户端版本并写进快照，返回解析结果。
///
/// **异步入口**：目录刷新 / 签到 / 余额查询这些异步路径都调它 —— 一次调用既
/// 拿到自己要用的版本，也顺手把 [`client_version_header`] 的快照填上（幂等，
/// 底层 `version::resolve_client_version` 自带 12 小时缓存）。
pub async fn prime_client_version() -> String {
    let resolved = version::resolve_client_version().await;
    store_client_version(&resolved);
    resolved
}

/// 写入快照（空串不覆盖：宁可留着上一次的好值，也不要为一次瞬时失败清空）
fn store_client_version(value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    let mut guard = match version_slot().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = Some(trimmed.to_string());
}

/// 同步读取 `X-LobsterAI-Client-Version` 的值。
///
/// 快照没填过（进程刚起、还没走过任何异步取版本的路径）时退回兜底常量 ——
/// **绝不发空版本头**（空串会被服务端按「不认识的客户端」对待）。
pub fn client_version_header() -> String {
    let cached = match version_slot().lock() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    match cached {
        Some(value) if !value.trim().is_empty() => value,
        _ => version::fallback_version().to_string(),
    }
}

/// 上游不认的 OpenAI 角色名（带它会被网关以 HTTP 502「角色信息不正确」拒绝）
const DEVELOPER_ROLE: &str = "developer";
/// 归一后的角色名（两者在 OpenAI 规范里语义相同）
const DEMOTED_ROLE: &str = "system";

/// 把 `role: "developer"` 的消息改成 `"system"`（原地改写，非对象条目原样放过）。
///
/// 见模块头的实测表：这是上游的硬要求，不是风格偏好。
fn demote_developer_messages(object: &mut serde_json::Map<String, Value>) {
    let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for message in messages.iter_mut() {
        let Some(entry) = message.as_object_mut() else {
            continue;
        };
        let is_developer = entry
            .get("role")
            .and_then(Value::as_str)
            .is_some_and(|role| role == DEVELOPER_ROLE);
        if is_developer {
            entry.insert("role".to_string(), Value::String(DEMOTED_ROLE.to_string()));
        }
    }
}

/// `tool_choice` 是否属于「上游会拒绝、必须删掉」的取值（`""` / `"none"` / `null`）。
fn tool_choice_must_go(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty() || text.trim().eq_ignore_ascii_case("none"),
        _ => false,
    }
}

/// 准备一次上游调用。
///
/// `account_id` 只参与 `x-lobster-session-id` 的派生（见
/// [`fingerprint::session_id`]），不进请求体、不落任何日志。
pub fn build_chat_request(
    account: &Value,
    account_id: &str,
    body: &Value,
    client_headers: &Headers,
) -> Result<ChatPlan, GatewayError> {
    // 令牌读取口径与 `atomcode/chat.rs` 一致（会话形态的 `auth.accessToken`）
    let access_token = account
        .get("auth")
        .and_then(|auth| auth.get("accessToken"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if access_token.is_empty() {
        return Err(GatewayError::with_status(
            401,
            "LobsterAI 账号缺少访问令牌，无法转发（请重新登录）",
        ));
    }

    let requested = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if requested.is_empty() {
        return Err(GatewayError::with_status(400, "请求缺少 model 字段"));
    }

    let mut payload = body.clone();
    let Some(object) = payload.as_object_mut() else {
        return Err(GatewayError::with_status(400, "请求体不是 JSON 对象"));
    };

    // ① developer → system（上游 502「角色信息不正确」的唯一成因，见模块头）
    demote_developer_messages(object);
    // ② tool_choice：`""` / `"none"` / `null` 一律删掉
    if object.get("tool_choice").is_some_and(tool_choice_must_go) {
        object.remove("tool_choice");
    }
    // ③ 绝不下发 prompt_cache_key（腾讯后端机制，本家不认）
    object.remove("prompt_cache_key");
    // ④ 模型名解析：展示名 / 规范化别名 → 线上 id；认不出原样发
    let wire_model =
        models::upstream_id_for(&requested).unwrap_or_else(|| requested.clone());
    object.insert("model".to_string(), Value::String(wire_model));

    // `stream` **不动**：本网关的转发入口恒以 `stream:true` 请求上游（见模块头），
    // 本家 `stream:false` 会回 HTTP 500 —— 碰它反而会把它弄坏。

    // 版本一处取值、三处复用（版本头 / UA 里的 `LobsterAI/<ver>` / 与本家
    // `body.version` 同源）：官方客户端这三处的值必须一致，写岔一处就是指纹差异。
    let client_version = client_version_header();

    // 官方每 turn 一条 traceparent + 三个用量归因头（见模块头的头集合表）。
    let trace = RequestTrace::new();
    let first_message = fingerprint::first_user_message(body);

    let mut headers: Vec<(String, String)> = vec![
        (
            "authorization".to_string(),
            format!("Bearer {access_token}"),
        ),
        // ⚠️ `Content-Type` **必须显式发**：本仓的转发层用 `client.post(url).body(bytes)`
        // （`core/upstream/request.rs` 的发送点），reqwest 的 `.body()` **不会**自动补
        // 这个头 —— 只有 `.json()` 才会。而剥掉它之后上游回的是**鉴权之后的**
        // `{"code":500,"message":"服务器内部错误"}`（HTTP 500，约 46ms 即返），
        // 跨模型稳定复现，看上去像「网关坏了」而不是「少了个头」。
        // 判据：同一账号打同一个域名的其它 POST（换码 `/api/auth/exchange`、
        // 签到）都带了它且成功；伪令牌探针证明上游**先鉴权**（401 code 40100），
        // 所以 500 出在鉴权之后。两个参考实现也都把它列在四个基础头里
        // （TS `lobsteraiAuthHeaders`、Go `authHeaders`）。
        //
        // 取值口径按官方：**入站给了就原样转发，没给才用 `application/json`**
        // （官方 `buildUpstreamRequestHeaders` 的第三条）。
        (
            "content-type".to_string(),
            incoming_content_type(client_headers),
        ),
        // 两个客户端头：能力位恒官方四段（恒定量，不按请求协商），版本是 CalVer。
        (
            "x-lobsterai-client-capabilities".to_string(),
            CLIENT_CAPABILITIES.to_string(),
        ),
        (
            "x-lobsterai-client-version".to_string(),
            client_version.clone(),
        ),
        // W3C 追踪：`00-<traceId>-<spanId>-01`（官方 `formatLlmTraceparent`）。
        (
            "traceparent".to_string(),
            trace.traceparent(),
        ),
        // 用量归因：会话 id（内容派生、会话内稳定）+ turn id（= 本次 traceId）
        (
            "x-lobster-session-id".to_string(),
            fingerprint::session_id(account_id, &first_message),
        ),
        (
            "x-lobster-turn-id".to_string(),
            fingerprint::turn_id(&trace),
        ),
        // UA：Electron/Chromium 默认形态，版本 token 与版本头同一个值。
        (
            "user-agent".to_string(),
            fingerprint::user_agent(&client_version),
        ),
    ];

    // 用户消息原文（前 200 码点、URL 编码）：官方只在「这条请求能映射到一个
    // 用户 turn」时追加 —— 本网关一次请求就是一次 turn，所以恒有。
    if let Some(message) = fingerprint::last_user_message(body) {
        let value = fingerprint::user_message_header(&message);
        if !value.is_empty() {
            headers.push(("x-lobster-user-message".to_string(), value));
        }
    }

    // `Accept`：官方口径是「入站带了才转发」，没带就退回本家必需的那条
    // （本家只收 SSE，两个类型都报 —— 照 Go `chatHeaders`）。
    headers.push((
        "accept".to_string(),
        match client_headers.get("accept") {
            Some(value) if !value.trim().is_empty() => value.trim().to_string(),
            _ => "text/event-stream, application/json".to_string(),
        },
    ));

    // ⚠️ **不发 `x-lobster-model-preset`**：官方只在「请求的 model 与本 turn 在
    // 界面里选定的 preset 一致」时附加，而网关是 OpenHub/OpenAI 入口，没有
    // 「界面 preset」这个概念。硬编一个等于自报「我每次都命中 preset」，
    // 比不发更可疑 —— 对照表第 10 条把「与请求体无关的静态头」列为破绽。

    Ok(ChatPlan::chat(chat_url(), headers, payload))
}

/// 入站的 `Content-Type`（没有或为空时退回 `application/json`）。
fn incoming_content_type(client_headers: &Headers) -> String {
    match client_headers.get("content-type") {
        Some(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => "application/json".to_string(),
    }
}
