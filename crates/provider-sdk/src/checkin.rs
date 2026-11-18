use std::sync::atomic::{AtomicU64, Ordering};

use agent2api_provider_protocol as proto;
use serde::{Deserialize, Serialize};

use crate::{PluginCtx, Provider};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckinResult {
    #[serde(flatten)]
    pub report: proto::CheckinReport,
    pub retry_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl From<proto::CheckinReport> for CheckinResult {
    fn from(report: proto::CheckinReport) -> Self {
        Self {
            report,
            retry_at: None,
            details: None,
        }
    }
}

// ── 同供应商签到避让（fx3，2026-11-17）───────────────────────────────
// 背景：宿主给每个 provider 发独立签到窗口（如 00:01–00:10），窗口内
// 逐账号随机。但同一家的两个账号会背靠背（实测 loomy 只隔 22 秒、
// autoclaw/codearts 各隔 81 秒）——上游画像一眼就是批量脚本。
//
// 机制：SDK 层在 token 刷新之前设门槛，所有 provider 的签到必经此处：
//   1. 每次真实触碰上游（无论成功失败）都在台账里记 `at`；
//   2. 需要避让的账号写 `planned` 并通过 retry_at 告知宿主回来时刻，
//      这个计划时刻对同家其它账号也是占用；
//   3. 同家其它账号的 max(at, planned) + MIN_INTERVAL 之内不开签，
//      并叠加 [0, JITTER) 抖动，避免推迟时刻又聚在同一秒；
//   4. 自己已有 planned 且未到点 → 按既定计划走（不重新随机）；
//   5. 状态 6 小时过期：跨天/跨轮自然失效，不影响宿主补签轮；
//   6. manual=true（面板手动点击）直接放行，但同样记账。
pub(crate) const STAGGER_MIN_INTERVAL_MS: i64 = 30 * 60 * 1000;
pub(crate) const STAGGER_JITTER_MS: i64 = 10 * 60 * 1000;
const STAGGER_STALE_MS: i64 = 6 * 60 * 60 * 1000;
const STAGGER_KIND: &str = "checkinStagger";

#[derive(Clone, Debug, Deserialize)]
struct StaggerState {
    /// 最近一次真实触碰上游的时刻（epoch ms）。
    at: i64,
    /// 承诺宿主 retry_at 回来的时刻；到点放行。
    planned: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StaggerGate {
    Proceed,
    Defer(i64),
}

/// 纯决策函数：给定当前时刻、自己的状态、同家其它账号的状态与抖动量，
/// 决定放行还是推迟。无副作用，方便单测。
fn plan_stagger(
    now: i64,
    mine: Option<StaggerState>,
    others: &[StaggerState],
    jitter_ms: i64,
) -> StaggerGate {
    // 自己已有既定计划且未到点：按计划走（保证宿主重排稳定，不再随机）。
    if let Some(state) = &mine {
        if let Some(planned) = state.planned {
            if now < planned {
                return StaggerGate::Defer(planned);
            }
        }
    }
    // 其它账号的最近占用（真实触碰或计划占用），过期条目忽略。
    let occupied = others
        .iter()
        .flat_map(|s| [Some(s.at), s.planned])
        .flatten()
        .filter(|t| now - *t < STAGGER_STALE_MS)
        .max()
        .unwrap_or(0);
    let earliest = occupied + STAGGER_MIN_INTERVAL_MS;
    if now < earliest {
        return StaggerGate::Defer(earliest + jitter_ms);
    }
    StaggerGate::Proceed
}

fn now_ms() -> Option<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as i64)
}

/// 逐次调用各不相同的抖动（毫秒，[0, STAGGER_JITTER_MS)）。
///
/// 不引 rand：SDK 只依赖 serde / tokio / protocol。SystemTime 纳秒与进程内
/// 单调计数器混哈希 —— 同一毫秒内的连续调用也不会撞车（raccoon 的
/// `request_id` 同款做法）。旧实现是 `now_ms() % JITTER`（时间推导的确定值），
/// 同一毫秒内两次调用抖动必然相同，叫「随机」是错的。
fn jitter_ms() -> i64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut state = nanos ^ counter.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    state ^= state >> 33;
    state = state.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    state ^= state >> 29;
    (state % STAGGER_JITTER_MS as u64) as i64
}

fn parse_state(raw: serde_json::Value) -> Option<StaggerState> {
    serde_json::from_value::<StaggerState>(raw).ok()
}

/// 避让门槛。返回 Some(result) 表示本次不签到（已写 planned 并告知宿主）。
async fn stagger_gate(
    provider: &dyn Provider,
    ctx: &PluginCtx,
    account: &str,
    manual: bool,
) -> Option<CheckinResult> {
    let now = now_ms()?;
    let provider_id = provider.meta().id;

    let mut mine = ctx
        .ledger_get(account, STAGGER_KIND)
        .await
        .ok()
        .and_then(parse_state);

    if manual {
        // 手动触发不受避让约束，但同样记账（它是真实触碰）。
        let state = serde_json::json!({ "at": now, "planned": null });
        let _ = ctx.ledger_put(account, STAGGER_KIND, &state).await;
        return None;
    }

    let mut others = Vec::new();
    if let Ok(accounts) = ctx.store_list(&provider_id).await {
        for item in accounts {
            if item.id == account {
                continue;
            }
            if let Ok(raw) = ctx.ledger_get(&item.id, STAGGER_KIND).await {
                if let Some(state) = parse_state(raw) {
                    others.push(state);
                }
            }
        }
    }

    let jitter = jitter_ms();
    // `mine.take()` 会把 mine 置空：Defer 分支要用的 `at` 必须先留出来，
    // 否则下面读到的永远是 None、at 永远写成 now（fx3 的死逻辑）。
    let mine_at = mine.as_ref().map(|s| s.at);
    match plan_stagger(now, mine.take(), &others, jitter) {
        StaggerGate::Proceed => {
            let state = serde_json::json!({ "at": now, "planned": null });
            let _ = ctx.ledger_put(account, STAGGER_KIND, &state).await;
            None
        }
        StaggerGate::Defer(planned) => {
            let at = mine_at.unwrap_or(now);
            let state = serde_json::json!({ "at": at, "planned": planned });
            let _ = ctx.ledger_put(account, STAGGER_KIND, &state).await;
            let wait_min = (planned - now) / 60_000;
            Some(CheckinResult {
                report: proto::CheckinReport {
                    outcome: proto::CheckinOutcome::Failed,
                    credit: None,
                    streak: None,
                    message: Some(format!(
                        "同供应商签到避让：{} 分钟后重试（同家账号间隔 ≥ {} 分钟）",
                        wait_min,
                        STAGGER_MIN_INTERVAL_MS / 60_000
                    )),
                },
                retry_at: Some(planned),
                details: Some(serde_json::json!({
                    "reason": "providerStagger",
                    "plannedAt": planned,
                })),
            })
        }
    }
}

pub(crate) async fn run(
    provider: &dyn Provider,
    ctx: &PluginCtx,
    account: &str,
    manual: bool,
) -> Result<CheckinResult, String> {
    // 同供应商避让：先于刷新。被推迟时连令牌刷新都省掉。
    if let Some(deferred) = stagger_gate(provider, ctx, account, manual).await {
        return Ok(deferred);
    }
    let refreshable = provider.meta().capabilities.refresh;
    if refreshable {
        if provider.token_expiring(ctx, account).await? {
            provider.refresh_token(ctx, account).await?;
        } else {
            provider.ensure_token(ctx, account).await?;
        }
    }
    match provider.checkin_with_options(ctx, account, manual).await {
        Err(error) if refreshable && crate::errors::is_unauthorized(&error) => {
            provider.refresh_token(ctx, account).await?;
            provider.checkin_with_options(ctx, account, manual).await
        }
        result => result,
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod stagger_tests {
    use super::*;

    fn st(at: i64, planned: Option<i64>) -> StaggerState {
        StaggerState { at, planned }
    }

    const T0: i64 = 1_000_000_000_000;

    #[test]
    fn first_account_proceeds() {
        assert_eq!(plan_stagger(T0, None, &[], 0), StaggerGate::Proceed);
    }

    #[test]
    fn second_account_within_interval_is_deferred() {
        // 复现用户实测：loomy 两号只隔 22 秒 → 推到间隔下限。
        let now = T0 + 22_000;
        let others = vec![st(T0, None)];
        match plan_stagger(now, None, &others, 0) {
            StaggerGate::Defer(at) => assert_eq!(at, T0 + STAGGER_MIN_INTERVAL_MS),
            _ => panic!("同家 22 秒内第二号应当被推迟"),
        }
    }

    #[test]
    fn second_account_after_interval_proceeds() {
        let now = T0 + STAGGER_MIN_INTERVAL_MS + 1;
        let others = vec![st(T0, None)];
        assert_eq!(plan_stagger(now, None, &others, 0), StaggerGate::Proceed);
    }

    #[test]
    fn jitter_is_added_on_top_of_interval() {
        let now = T0 + 60_000;
        let others = vec![st(T0, None)];
        match plan_stagger(now, None, &others, 5 * 60_000) {
            StaggerGate::Defer(at) => {
                assert_eq!(at, T0 + STAGGER_MIN_INTERVAL_MS + 5 * 60_000)
            }
            _ => panic!("应当推迟且叠加抖动"),
        }
    }

    #[test]
    fn planned_reservation_blocks_others() {
        // 账号 A 已被宿主约在未来 5 分钟回来 → B 至少要等到 A 回来后再隔一个间隔。
        let now = T0 + STAGGER_MIN_INTERVAL_MS + 60_000;
        let others = vec![st(T0, Some(now + 5 * 60_000))];
        match plan_stagger(now, None, &others, 0) {
            StaggerGate::Defer(at) => assert_eq!(at, now + 5 * 60_000 + STAGGER_MIN_INTERVAL_MS),
            _ => panic!("计划占用应当把后来者推开"),
        }
    }

    #[test]
    fn own_pending_plan_is_stable_until_due() {
        let mine = st(T0 - 60_000, Some(T0 + 10 * 60_000));
        // 未到点：按既定计划走，不重新随机。
        assert_eq!(
            plan_stagger(T0, Some(mine.clone()), &[], 42),
            StaggerGate::Defer(T0 + 10 * 60_000)
        );
        // 到点：放行。
        let due = T0 + 10 * 60_000 + 1;
        assert_eq!(plan_stagger(due, Some(mine), &[], 42), StaggerGate::Proceed);
    }

    #[test]
    fn stale_entries_are_ignored() {
        let now = T0;
        let others = vec![st(now - STAGGER_STALE_MS - 1, None)];
        assert_eq!(plan_stagger(now, None, &others, 0), StaggerGate::Proceed);
    }

    #[test]
    fn takes_max_of_at_and_planned() {
        let now = T0 + 60_000;
        let others = vec![st(T0 - 3_600_000, Some(T0 + 3 * 60_000))];
        match plan_stagger(now, None, &others, 0) {
            StaggerGate::Defer(at) => {
                assert_eq!(at, T0 + 3 * 60_000 + STAGGER_MIN_INTERVAL_MS)
            }
            _ => panic!("planned 比 at 更近时应当按 planned 算"),
        }
    }

    #[test]
    fn jitter_varies_per_call() {
        // 抖动必须逐次调用各不相同：旧实现是 now_ms() % JITTER，
        // 同一毫秒内两次调用必然相同。
        let samples: Vec<i64> = (0..16).map(|_| jitter_ms()).collect();
        assert!(samples.iter().all(|v| (0..STAGGER_JITTER_MS).contains(v)));
        let distinct: std::collections::HashSet<i64> = samples.iter().copied().collect();
        assert!(distinct.len() > 1, "抖动逐次调用应当不同");
    }
}
