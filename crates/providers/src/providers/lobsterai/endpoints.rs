//! LobsterAI（有道龙虾）上游地址与身份常量。
//!
//! ── 上游长什么样 ────────────────────────────────────────────
//! 网易有道系的桌面 Agent，两个域都在有道名下（CNAME 都指向网易广州 ALB）：
//!
//! ```text
//!   登录门户   https://lobsterai.youdao.com        浏览器授权页（手机号 / 微信）
//!   API        https://lobsterai-server.youdao.com  换码 / 续期 / 目录 / 对话 / 积分
//! ```
//!
//! 对话端点是 **OpenAI 兼容的原生协议**（`POST /api/proxy/v1/chat/completions`），
//! 但**只收 SSE**：`stream:false` 上游直接回 HTTP 500（Go 参考 `executor.go:18-21`
//! 实测）。本网关的转发入口「无论客户端要不要流式，上游恒以 `stream:true` 请求」
//! （`core/upstream/mod.rs` 的注释原文），所以这条要求天然满足，适配器**不要**去动
//! `stream` 字段。
//!
//! ── 两个 `X-LobsterAI-Client-*` 头是功能性的，不是装饰 ──────────
//! `ClientCapabilities` 声明客户端能力，服务端**按它过滤**：
//!
//! ```text
//!   不带该头        -> 目录里 25 个模型（没有 kimi-k3）
//!   带 kimi-k3-agentic-v1 -> 26 个（kimi-k3 才出现）
//! ```
//!
//! 另一个能力位 `thinking-level-control-v1` 是「把思考关掉」这条参数的前提 ——
//! 缺它时发 `reasoning_effort: "off"` 会得到 HTTP 500。两个都实测过
//! （见参考实现的 `lobsterai-product.ts` 与 `headers.go` 的注释）。
//!
//! 版本头走的是**日期式版本**（CalVer，形如 `2026.11.6`），由上游的更新接口给出。
//!
//! ⚠️ **能力位与 `User-Agent` 已按官方客户端行为纠正**（证据分级见
//! `fingerprint.rs` 的模块头）：
//!
//! ```text
//!   旧（参考实现的形态 —— 每一条都是「这不是官方端」的凭据）
//!     X-LobsterAI-Client-Capabilities: kimi-k3-agentic-v1,thinking-level-control-v1   （2 段子集）
//!     User-Agent: LobsterAI/0.1.0                                    （非 Chromium 形态，且与版本头对不上）
//!   新（官方客户端形态）
//!     X-LobsterAI-Client-Capabilities: virtual-models-v1,kimi-k3-agentic-v1,
//!                                      thinking-level-control-v1,upstream-error-details-v1  （恒 4 段、顺序固定）
//!     User-Agent: Mozilla/5.0 (…) … LobsterAI/<CalVer> Chrome/<chromium> Electron/<electron> …
//! ```
//!
//! UA 由 [`super::fingerprint::user_agent`] 统一生成：它的版本 token 用**同一个**
//! 已解析版本，三处取自同一份值（各请求按需携带：对话是版本头＋UA，鉴权是
//! body.version＋UA，目录是 URL＋版本头＋UA）。
//!
//! ── 没有任何签名 ────────────────────────────────────────────
//! 纯 `Bearer`。没有 HMAC / DPoP / PKCE，也没有腾讯系的 `X-Domain` /
//! `X-Product` / `X-IDE-*` 那批归属头 —— 那些发过去轻则被忽略、重则把请求归到
//! 错误的客户端形态上（参考实现的模块头原话）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic = "unwind"`（workspace Cargo.toml）：panic 会被请求边界的
//! `catch_unwind` 兜住，不会带走整个插件进程；本文件仍保持零 unwrap/expect/panic。

/// 上游 API 主机（换码 / 续期 / 目录 / 对话 / 积分都在它下面）
pub const API_BASE: &str = "https://lobsterai-server.youdao.com";

/// 浏览器登录门户主机
pub const PORTAL_BASE: &str = "https://lobsterai.youdao.com";

/// 客户端版本查询接口。**这是第三方主机，响应不是业务信封**：版本号在
/// `data.value`（参考实现 `lobsterai-product.ts:443` 的说明）。
pub const CLIENT_VERSION_API: &str =
    "https://api-overmind.youdao.com/openapi/get/luna/hardware/lobsterai/prod/update";

/// 查不到版本时用的兜底值。
///
/// 必须是一个**真实发布过的 CalVer**（当前官方最新发布 `2026.11.16`，兜底跟着
/// 它走）：版本头是功能性头，空串或 `0.0.0` 会被按「不认识的客户端」
/// 对待；`2026.9.4`（旧兜底）已经明显落后于官方当前发布版，也一并纠正。
pub const FALLBACK_CLIENT_VERSION: &str = "2026.11.16";

/// 声明的客户端能力 —— **官方四段，恒定、顺序固定、英文逗号无空格**
/// （`src/shared/providers/modelRuntimeProfiles.ts:17-27` 的常量原文）。
///
/// ```text
///   virtual-models-v1           虚拟模型（yd-fast / yd-ultra）解析
///   kimi-k3-agentic-v1          Kimi K3 agentic 运行时配置（模型目录的准入门）
///   thinking-level-control-v1   思考等级控制（`reasoning_effort:"off"` 的前提，缺它 HTTP 500）
///   upstream-error-details-v1   上游错误详情透出
/// ```
///
/// 为什么不能只发「自己用得到的那两段」：官方客户端发的是**全客户端常量**，
/// 不是按请求协商出来的子集（`endpoints.rs` 模块头那句「服务端按它过滤」是
/// 功能面的事实，但指纹面的事实是「官方恒 4 段」）。发子集等于自报家门。
pub const CLIENT_CAPABILITIES: &str = "virtual-models-v1,kimi-k3-agentic-v1,thinking-level-control-v1,upstream-error-details-v1";

/// 用授权码换令牌（**不带 Authorization**）
pub const EXCHANGE_PATH: &str = "/api/auth/exchange";
/// 续期（**不带 Authorization**）
pub const REFRESH_PATH: &str = "/api/auth/refresh";
/// 模型目录
pub const MODELS_PATH: &str = "/api/models/available";
/// 对话（OpenAI 兼容，SSE only）
pub const CHAT_PATH: &str = "/api/proxy/v1/chat/completions";
/// loopback 回调路径
pub const CALLBACK_PATH: &str = "/auth/callback";
/// 积分余额
pub const PROFILE_SUMMARY_PATH: &str = "/api/user/profile-summary";

/// 每日签到三步：查活动位 → 取该活动上下文 → 执行 check_in
pub const ACTIVITY_SLOT_PATH: &str = "/api/client-activities/slot";
/// 见 [`ACTIVITY_SLOT_PATH`]（活动码拼在路径后面）
pub const ACTIVITY_CONTEXT_PATH: &str = "/api/client-activities";

/// 活动位查询的三个固定参数（照抄真实客户端的形态）。
///
/// `platform=win32` 是**按桌面客户端的形态标识**，与运行本网关的宿主系统无关 ——
/// 参考实现特意注明这一点。
pub const SLOT_PLACEMENT: &str = "desktop_sidebar";
/// 见 [`SLOT_PLACEMENT`]
pub const SLOT_CONTAINER_API_VERSION: &str = "2";
/// 见 [`SLOT_PLACEMENT`]
pub const SLOT_PLATFORM: &str = "win32";

/// 控制面请求超时（毫秒）
pub const REQUEST_TIMEOUT_MS: u64 = 30_000;
/// 目录请求超时（毫秒）
pub const CATALOG_TIMEOUT_MS: u64 = 20_000;
/// 已解析的客户端版本缓存时长（毫秒）——12 小时，版本不常变
pub const VERSION_CACHE_TTL_MS: i64 = 12 * 60 * 60 * 1000;

/// 读环境变量覆盖（空值视为未设置），去掉尾部斜杠
fn env_override(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

/// API 基址：`LOBSTERAI_API_BASE` 可覆盖
pub fn api_base() -> String {
    env_override("LOBSTERAI_API_BASE").unwrap_or_else(|| API_BASE.to_string())
}

/// 门户基址：`LOBSTERAI_PORTAL_BASE` 可覆盖
pub fn portal_base() -> String {
    env_override("LOBSTERAI_PORTAL_BASE").unwrap_or_else(|| PORTAL_BASE.to_string())
}

/// 对话端点完整 URL
pub fn chat_url() -> String {
    format!("{}{}", api_base(), CHAT_PATH)
}

/// 目录端点 URL（`keyfrom` 作为查询串，与参考实现一致）
pub fn models_url(keyfrom: &str) -> String {
    format!("{}{}?{}", api_base(), MODELS_PATH, keyfrom.trim())
}

/// 余额端点 URL
pub fn profile_summary_url() -> String {
    format!("{}{}", api_base(), PROFILE_SUMMARY_PATH)
}

/// 活动位 URL（三个固定参数）
pub fn activity_slot_url(client_version: &str) -> String {
    format!(
        "{}{}?placement={}&clientVersion={}&containerApiVersion={}&platform={}",
        api_base(),
        ACTIVITY_SLOT_PATH,
        SLOT_PLACEMENT,
        urlencode(client_version),
        SLOT_CONTAINER_API_VERSION,
        SLOT_PLATFORM
    )
}

/// 某个活动的上下文 URL
///
/// ⚠️ 末尾那个 `/context` 段不能省：上游的路径是三段式
/// `{code}/context?configRevision=…`，少了它整条请求落到 `/api/client-activities/{code}`
/// 上、直接被回 **HTTP 404**（签到因此稳定失败，报「活动上下文查询返回 HTTP 404」）。
/// 参考实现（Go `fetchActivityContext`、TS `lobsterai-credits.ts`）都是三段式。
pub fn activity_context_url(code: &str, config_revision: &str) -> String {
    format!(
        "{}{}/{}/context?configRevision={}",
        api_base(),
        ACTIVITY_CONTEXT_PATH,
        urlencode(code),
        urlencode(config_revision)
    )
}

/// 某个活动的 check_in URL
pub fn activity_checkin_url(code: &str) -> String {
    format!(
        "{}{}/{}/actions/check_in",
        api_base(),
        ACTIVITY_CONTEXT_PATH,
        urlencode(code)
    )
}

/// 极小的 query 转义（本家只有版本号 / 活动码 / 修订版三个值要拼进 URL）。
///
/// 不引依赖：这些值都可能来自上游响应（活动码、configRevision），一旦含
/// `&` / `=` / `?` 就会截断 query，所以必须转义而不是直接拼。
pub fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
