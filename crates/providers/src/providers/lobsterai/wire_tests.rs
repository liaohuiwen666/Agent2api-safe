//! 上线前抓包自测：把「发往上游的最终请求」在本机 mock 上抓下来逐条断言。
//!
//! ── 为什么要有这一层（而不是只写单元测试）───────────────
//! 指纹错误的共同特征是**不报错**：keyfrom 换个值、能力位少两段、UA 换个串，
//! 上游都照常回 200，只在服务端风控那边记一笔。要证明「这次发出去的就是官方
//! 形态」，唯一可靠的证据是**线上字节**：真的把请求发出去、在另一端读回来。
//!
//! 做法：起一个 127.0.0.1 的假上游（裸 `TcpListener`，不引测试依赖），把
//! `LOBSTERAI_API_BASE` 指到它，再走**真实**的 oauth / models 路径。
//! 请求行、头、体全部原样保存下来断言。
//!
//! 转发（chat）路径的断言在 **plan 层**：`chat::build_chat_request` 产出的
//! `ChatPlan.headers` 就是网关逐条 `builder.header(key, value)` 发出去的那份
//! （`core/upstream/request.rs::send_chat_with_client` 的头循环，不增不删不改名），
//! 而本测试进程里没有网关的 HTTP 发送层，所以这一条验的是「交给网关的那份头」。
//!
//! ── 同一个进程里的环境变量 ──────────────────────────────
//! `LOBSTERAI_API_BASE` 是进程级的，所以四条用例用一把锁串行（`ENV_LOCK`），
//! 结束时清掉变量，避免污染别的用例。
//!
//! ── 关于本文件里的 unwrap ───────────────────────────────
//! 只在 `#[cfg(test)]` 下编译。crate 级的 `deny(clippy::unwrap_used)` 是为
//! 发布代码设的；测试里失败就该 panic（断言失败 = 这次修复不成立），所以这里
//! 显式放宽，而不是把断言写成「静默跳过」。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::credentials::LobsterCredentials;
use super::{chat, endpoints, fingerprint, models, oauth};

/// 串行化「改环境变量 + 发请求」这一段（进程级 env 是共享状态）
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// 抓到的一条请求
#[derive(Clone, Debug)]
struct Captured {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Captured {
    /// 取某个头的**全部**取值（用来验「没有重复头」）
    fn header_all(&self, name: &str) -> Vec<String> {
        self.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
            .collect()
    }

    /// 取某个头的唯一取值（缺头时给空串）
    fn header(&self, name: &str) -> String {
        self.header_all(name).join(",")
    }

    /// 请求体解析成 JSON（不是 JSON 时给 Null，断言会自然失败）
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// 起一个假上游，按 `responses` 顺序回应，返回基址与抓包槽。
fn mock_upstream(responses: Vec<String>) -> (String, Arc<Mutex<Vec<Captured>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock upstream");
    let addr = listener.local_addr().expect("mock upstream addr");
    let captured: Arc<Mutex<Vec<Captured>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    std::thread::spawn(move || {
        for response in responses {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let Some(request) = read_request(&stream) else {
                continue;
            };
            if let Ok(mut slot) = sink.lock() {
                slot.push(request);
            }
            write_response(&stream, &response);
        }
    });
    (format!("http://{addr}"), captured)
}

/// 读一条 HTTP/1.1 请求（请求行 + 头 + 按 Content-Length 读体）。
fn read_request(stream: &std::net::TcpStream) -> Option<Captured> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut length = 0usize;
    loop {
        let mut header_line = String::new();
        if reader.read_line(&mut header_line).ok()? == 0 {
            break;
        }
        let trimmed = header_line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((key, value)) = trimmed.split_once(':') {
            let name = key.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if name == "content-length" {
                length = value.parse::<usize>().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }

    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some(Captured {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

/// 回一条 JSON（`Connection: close`：每条用例一个连接，读起来最省事）
fn write_response(mut stream: &std::net::TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.as_bytes().len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// 令牌信封（换码 / 续期共用形态）
fn token_envelope() -> String {
    json!({
        "code": 0,
        "msg": "ok",
        "data": {
            "accessToken": "at-test-1",
            "refreshToken": "rt-test-1",
            "expiresIn": 3600,
            "user": {"id": "uid-1", "userId": "userid-1", "nickname": "tester"}
        }
    })
    .to_string()
}

/// 目录信封：含一条正常模型、一条**虚拟模型**（应被过滤）、一条 kimi-k3
fn catalog_envelope() -> String {
    json!({
        "code": 0,
        "data": [
            {"modelId": "deepseek-v4-flash", "modelName": "DeepSeek V4 Flash", "contextWindow": 128000, "maxTokens": 8192, "costMultiplier": 0.5},
            {"modelId": "yd-fast", "modelName": "YD Fast"},
            {"modelId": "kimi-k3", "modelName": "Kimi K3"}
        ]
    })
    .to_string()
}

/// 测试账号的凭证快照（`official` 两个 keyfrom + 载荷件齐全）
fn credentials() -> LobsterCredentials {
    LobsterCredentials {
        id: "lobsterai-test".to_string(),
        access_token: "at-test-1".to_string(),
        refresh_token: "rt-test-1".to_string(),
        expires_at: 0,
        uid: "uid-1".to_string(),
        user_id: "userid-1".to_string(),
        nickname: "tester".to_string(),
        uuid: "158d6395-ebc3-4235-b5d6-a46df4db9af4".to_string(),
        first_keyfrom: fingerprint::OFFICIAL_KEYFROM.to_string(),
        latest_keyfrom: fingerprint::OFFICIAL_KEYFROM.to_string(),
    }
}

/// 官方四段能力位（顺序与官方常量逐字一致）
const OFFICIAL_CAPABILITIES: &str =
    "virtual-models-v1,kimi-k3-agentic-v1,thinking-level-control-v1,upstream-error-details-v1";

/// ── 用例 1：登录换码路径 ────────────────────────────────────
/// 体里两个 keyfrom 必须是 `official`、uuid / version 齐全；
/// 头里必须是浏览器形态的 UA，且**不带**能力位头（官方 auth 接口不带）。
#[test]
fn exchange_path_sends_official_keyfrom_and_browser_ua() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (base, sink) = mock_upstream(vec![token_envelope()]);
    std::env::set_var("LOBSTERAI_API_BASE", &base);

    let bundle = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(oauth::exchange(
            "auth-code-xyz",
            "158d6395-ebc3-4235-b5d6-a46df4db9af4",
            fingerprint::OFFICIAL_KEYFROM,
            fingerprint::OFFICIAL_KEYFROM,
            "2026.11.6",
        ));
    std::env::remove_var("LOBSTERAI_API_BASE");

    assert!(bundle.is_ok(), "换码应当成功：{:?}", bundle.err().map(|e| e.message));
    let request = sink.lock().expect("captured").first().cloned().expect("one request");

    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/api/auth/exchange");

    let body = request.json();
    assert_eq!(body["authCode"], "auth-code-xyz");
    assert_eq!(body["firstKeyfrom"], "official");
    assert_eq!(body["latestKeyfrom"], "official");
    assert_eq!(body["uuid"], "158d6395-ebc3-4235-b5d6-a46df4db9af4");
    assert_eq!(body["version"], "2026.11.6");

    // 头：浏览器形态 UA（含 LobsterAI/<CalVer> + Electron token）
    let ua = request.header("user-agent");
    assert!(ua.starts_with("Mozilla/5.0 ("), "UA 必须是 Chromium 形态：{ua}");
    assert!(ua.contains("LobsterAI/2026.11.6"), "{ua}");
    assert!(ua.contains("Electron/43.5.0"), "{ua}");
    // 官方 auth 接口不带能力位头；也不该带 Authorization
    assert!(request.header_all("x-lobsterai-client-capabilities").is_empty());
    assert!(request.header_all("authorization").is_empty());
    // 不出现重复的内容协商头（reqwest 的 header() 是 append 语义）
    assert_eq!(request.header_all("content-type").len(), 1, "{:?}", request.headers);
    assert_eq!(request.header_all("accept").len(), 1, "{:?}", request.headers);
}

/// ── 用例 2：续期路径 + 存量账号的 keyfrom 纠正 ───────────────
/// 记录里存的是旧实现写进去的毫秒时间戳时，续期体里必须已经变成 `official`。
#[test]
fn refresh_path_normalizes_legacy_timestamp_keyfrom() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (base, sink) = mock_upstream(vec![token_envelope()]);
    std::env::set_var("LOBSTERAI_API_BASE", &base);

    // 模拟存量账号：两个 keyfrom 都是旧实现写入的时间戳
    let record = json!({
        "id": "lobsterai-legacy",
        "accessToken": "at-legacy",
        "refreshToken": "rt-legacy",
        "uuid": "9b2f0c44-5f5f-4c58-9a38-2b3f49a1d1c9",
        "firstKeyfrom": "1793869387181",
        "latestKeyfrom": "1793869387181",
        "userId": "userid-1"
    });
    let credentials = super::credentials::from_record(Some(&record)).expect("credentials");
    assert_eq!(credentials.first_keyfrom, "official");
    assert_eq!(credentials.latest_keyfrom, "official");

    let bundle = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(oauth::refresh(&credentials, "2026.11.6"));
    std::env::remove_var("LOBSTERAI_API_BASE");

    assert!(bundle.is_ok(), "续期应当成功：{:?}", bundle.err().map(|e| e.message));
    let request = sink.lock().expect("captured").first().cloned().expect("one request");

    assert_eq!(request.path, "/api/auth/refresh");
    let body = request.json();
    assert_eq!(body["firstKeyfrom"], "official", "{body}");
    assert_eq!(body["latestKeyfrom"], "official", "{body}");
    assert_eq!(body["version"], "2026.11.6");
    assert_eq!(body["userId"], "userid-1");
    assert_eq!(body["refreshToken"], "rt-legacy");
    assert_eq!(body["uuid"], "9b2f0c44-5f5f-4c58-9a38-2b3f49a1d1c9");
}

/// ── 用例 3：模型目录路径 ────────────────────────────────────
/// query 里两个 keyfrom 是 `official`、头里是官方四段能力位 + CalVer + 浏览器 UA，
/// 且虚拟模型（yd-fast）不进对外清单。
#[test]
fn catalog_path_sends_four_capabilities_and_filters_virtual_models() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (base, sink) = mock_upstream(vec![catalog_envelope()]);
    std::env::set_var("LOBSTERAI_API_BASE", &base);

    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(models::refresh(&credentials(), true));
    std::env::remove_var("LOBSTERAI_API_BASE");

    assert!(outcome.refreshed, "目录刷新应当成功：{outcome:?}");
    let request = sink.lock().expect("captured").first().cloned().expect("one request");

    assert_eq!(request.method, "GET");
    assert!(request.path.starts_with("/api/models/available?"), "{}", request.path);
    assert!(request.path.contains("firstKeyfrom=official"), "{}", request.path);
    assert!(request.path.contains("latestKeyfrom=official"), "{}", request.path);
    // 版本号运行时从官方更新接口实时获取（12h 缓存），不硬编码具体值，
    // 只断言是合法 CalVer 且三处（URL / 版本头 / UA）完全一致。
    let version = request
        .path
        .split("version=")
        .nth(1)
        .and_then(|rest| rest.split('&').next())
        .expect("version 参数存在")
        .to_string();
    assert!(version.starts_with("2026."), "{}", request.path);
    assert!(request.path.contains("uuid=158d6395"), "{}", request.path);

    assert_eq!(request.header("x-lobsterai-client-capabilities"), OFFICIAL_CAPABILITIES);
    assert_eq!(request.header("x-lobsterai-client-version"), version);
    let ua = request.header("user-agent");
    assert!(ua.contains(&format!("LobsterAI/{version}")), "{ua}");
    assert_eq!(request.header_all("accept").len(), 1, "{:?}", request.headers);

    // 对外清单：虚拟模型被过滤，正常模型保留
    let listed = models::raw_list();
    let ids: Vec<String> = listed
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    assert!(ids.contains(&"deepseek-v4-flash".to_string()), "{ids:?}");
    assert!(ids.contains(&"kimi-k3".to_string()), "{ids:?}");
    assert!(!ids.contains(&"yd-fast".to_string()), "虚拟模型不该出现在目录里：{ids:?}");
}

/// ── 用例 4：转发路径的头集合（plan 层 = 网关发出去的那一份）──────
#[test]
fn chat_plan_carries_full_official_header_set() {
    let account = json!({"auth": {"accessToken": "at-test-1"}});
    let body = json!({
        "model": "deepseek-v4-flash",
        "stream": true,
        "messages": [
            {"role": "system", "content": "be brief"},
            {"role": "user", "content": "你好 世界"}
        ]
    });
    let headers = super::super::super::headers::Headers::from_map(&Default::default());
    let plan = chat::build_chat_request(&account, "lobsterai-test", &body, &headers)
        .expect("plan");

    assert_eq!(plan.url, endpoints::chat_url());
    assert!(plan.url.ends_with("/api/proxy/v1/chat/completions"), "{}", plan.url);

    let get = |name: &str| -> String {
        plan.headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>()
            .join(",")
    };

    assert_eq!(get("authorization"), "Bearer at-test-1");
    assert_eq!(get("content-type"), "application/json");
    // 官方四段能力位（恒定量）
    assert_eq!(get("x-lobsterai-client-capabilities"), OFFICIAL_CAPABILITIES);
    // 版本头是 CalVer；UA 里的版本 token 与它一致（三处一致里的两处，第三处是
    // 账号侧 body.version，由 oauth 用例覆盖）
    let version = get("x-lobsterai-client-version");
    assert!(!version.trim().is_empty(), "版本头不能为空");
    let ua = get("user-agent");
    assert!(ua.contains(&format!("LobsterAI/{version}")), "UA 与版本头必须同版本：{ua} / {version}");
    assert!(ua.contains("Electron/43.5.0"), "{ua}");
    assert!(!ua.contains("LobsterAI/0.1.0"), "旧写死 UA 必须消失：{ua}");

    // traceparent：W3C 形态
    let traceparent = get("traceparent");
    let parts: Vec<&str> = traceparent.split('-').collect();
    assert_eq!(parts.len(), 4, "{traceparent}");
    assert_eq!(parts[0], "00");
    assert_eq!(parts[1].len(), 32);
    assert_eq!(parts[2].len(), 16);
    assert_eq!(parts[3], "01");

    // 用量归因头：turn-id = traceId，user-message 是**这条请求的**最后一条用户消息
    assert_eq!(get("x-lobster-turn-id"), parts[1]);
    let session = get("x-lobster-session-id");
    assert_eq!(session.len(), 32, "{session}");
    assert_eq!(get("x-lobster-user-message"), "%E4%BD%A0%E5%A5%BD%20%E4%B8%96%E7%95%8C");

    // 每个头只出现一次（重复头同样是官方端不会有的形态）
    for name in [
        "authorization",
        "content-type",
        "accept",
        "user-agent",
        "x-lobsterai-client-capabilities",
        "x-lobsterai-client-version",
        "traceparent",
        "x-lobster-session-id",
        "x-lobster-turn-id",
        "x-lobster-user-message",
    ] {
        let count = plan
            .headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .count();
        assert_eq!(count, 1, "头 {name} 出现了 {count} 次");
    }

    // 入站的自定义头一个都不透传（官方本地代理是重建头，不是透传）
    assert!(!plan.headers.iter().any(|(key, _)| key.starts_with("x-stainless")));
    assert!(!plan.headers.iter().any(|(key, _)| key.starts_with("openai-")));
}
