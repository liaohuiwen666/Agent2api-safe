<div align="center">

# Agent2Providers · Safe Fork

**基于官方 providers 插件源码的加固分支** — HTTP 身份对齐官方客户端 · 可复现构建 · 原版签到节奏

[![License](https://img.shields.io/badge/license-AGPL--3.0-blue)](./LICENSE)
[![Upstream](https://img.shields.io/badge/upstream-3.5.0--beta.1--same--source-2ea44f)](https://github.com/ProvidersPlugin/Agent2Providers)
[![fx](https://img.shields.io/badge/fx--series-fx1→fx2→fx3→fx4→fx5→fx6-orange)](#-改动清单)
[![Tests](https://img.shields.io/badge/tests-25%2F25-success)](#-验证)

</div>

---

## 为什么有这个 fork

官方插件包的出站请求在**应用层**存在多处自曝特征，风控一眼可辨：

| 自曝点 | 官方包行为 | 本仓库行为 |
|---|---|---|
| 全局默认 UA | `undici`（Node.js fetch 标识） | Chrome/147 浏览器形态 |
| LobsterAI UA | `LobsterAI/0.1.0`（真实客户端是 Electron） | Electron/43.5.0 形态 |
| keyfrom 归因 | 登录时间戳（每次不同，可聚类识别） | 官方常量 `official` |
| 版本头 | 两处值矛盾（2026.9.4 / 2026.11.6） | 同源 `2026.11.16`（各请求按需携带，取值同一份已解析版本） |
| 聊天 plan | 不带 UA，核心 egress 默认补 `undici` | 5 家 plan 逐条带 Chrome/147 |

设计原则：**只改"长什么样"，不改"做什么"** — 业务语义、接口行为、数据格式零变更。

> 背景议题：[aimod-cc/Agent2API#590](https://github.com/aimod-cc/Agent2API/issues/590)

## HTTP 层适配矩阵

### ① 逐家深度适配

| Provider | 适配内容 | 来源 |
|---|---|---|
| **lobsterai** | UA → Electron/43.5.0、keyfrom → `official`、版本同源 2026.11.16（对话：版本头＋UA；鉴权：body.version＋UA；目录：URL＋版本头＋UA）、Capabilities 官方四段、W3C traceparent、`x-lobster-*` 归因头 | fx1 |
| **modelscope** | 网页会话补 Chrome UA + `Referer` + `sec-fetch-*` + `Accept`（同源 GET 不带 `Origin`；此前仅有 Cookie 一项头，签到/余额/用量全走此通道） | fx2 |
| **loomy** | 聊天与**图像生成**两处均带 Chrome/147（图像 plan 同走核心 egress，不带即自曝 undici） | fx4 / fx5 |

### ② 全局默认身份（fx2 共享层，一处覆盖全部）

`http.rs` 共享客户端默认身份 `undici` → **Chrome/147 + Accept-Language**。所有未显式声明 UA 的请求自动升级，直接受益（签到/余额路径原本裸奔 undici）：

```
autoclaw · loomy · codearts · astudio · officeace
```

聊天是另一条路：走插件自有客户端的聊天（astudio / codearts / catpaw）直接用上面这份默认身份；走 plan 的聊天（autoclaw / loomy / officeace / modelscope / raccoon）在 plan 里逐条带同一份 Chrome/147 UA —— 核心 egress 默认是 `undici`，plan 里不带就自曝。

### ③ 上游已有显式 UA（保留不动）

以下 provider 上游已对齐各自官方客户端形态（VSCode UA、独立标识、浏览器串等），本仓库**不覆盖、保持原样**：

```
trae(VSCode) · accio(独立标识) · qoder · kiro · antigravity · kuku
monkeycode · orcaterm · commandcode · zcode · workbuddy · doubaowork
```

### ④ 边界（明确不适用）

| 范围 | 原因 |
|---|---|
| atomcode | 线上部署使用独立闭源 wasm 包，本仓库实现不生效 |
| custom providers | 由网关核心直连，不经插件出站 |
| **TLS / JA3 层** | 全部 provider 仍为 rustls 裸奔（见 Roadmap） |

## 签到调度（原版节奏 + 窗口扩宽）

**插件层不做任何跨账号协调**——`provider-sdk/src/checkin.rs` 与上游逐字节一致（47 行）：刷新令牌 → 发签到 → 401 才重签一次。谁被点到谁就签。

调度交给宿主，靠**窗口**而非插件闸机控制密度：

```
窗口   00:01 – 00:40   （39 分钟，19 个号平均间隔约 2 分钟）
       宿主把每个账号随机撒进窗口，accountPlan.at 每号独立随机点
补签   00:40 + retryGapMinutes(16) = 00:56   未签成的再捞一轮
```

### 为什么撤销签到避让（fx3→fx6 的历史）

fx3 曾在 SDK 层加"同供应商 ≥30 分钟间隔"闸机，fx4/fx5 连续修补其缺陷，最终**整体撤销**（fx6）：

| 问题 | 实情 |
|---|---|
| 收益 | "更难被识别"零证据——背靠背跑数周无风控 |
| 代价 | 第 2 号从窗口内变成半小时~十小时后才签 |
| 致命缺陷 | 推迟依赖 `CheckinResult.retry_at`，但宿主 `retryAccounts` 恒空**不登记**，被推迟的号只能等补签轮 |
| 叠加成本 | fx3 闸机 → fx4 修 `mine.take` 死逻辑 → fx5 修 `Failed`/`Inactive` 语义 → 改宿主 retryGap，四层补丁 |

**实测数据（11-15）**：18 个号在原 9 分钟窗口里跨 495 秒、占用 92%、余量仅 45 秒。真正的瓶颈是**窗口容量**，不是账号间隔。拉宽窗口即可让随机撒点自然把间隔拉开，零代码、零新失败面。

### 宿主调度机制（实测）

| 字段（`kv.autoCheckin.schedules[]`） | 实测值 | 含义 |
|---|---|---|
| `time` / `timeEnd` | 00:01 / 00:40 | 首轮窗口 |
| `retryTime` | 00:40 | 补签轮起算点（= 窗口结束） |
| `retryGapMinutes` | 16 | 补签轮间隔 |
| `retryLimit` | 6 | 上限保护（实测从未跑满，每日仅一轮补签） |
| `accountPlan.at` | 每号随机 | 宿主预分的计划时刻，跨度与窗口完全吻合 |

> 宿主内置三个候选窗口时刻：`00:01` / `10:01` / `19:01`（`core::auto_checkin::times`），按 provider 的 `daily_window_start` 分配。qoder 实测在 10:01 窗口，zcode 在 19:01。本网关 18 家均落在 00:01。

> 改窗口只能走 **kv 直改 + 容器重启**：`/api/checkin-schedules` 的所有端点形态均 404，面板无编辑入口。脚本见 `tools/set-checkin-window.py`。## 快速开始

```bash
# 构建（Ubuntu 22.04 / glibc ≤ 2.36 宿主机，x86_64）
git clone https://github.com/liaohuiwen666/Agent2api-safe.git
cd Agent2api-safe
./build-fx.sh                     # 产物: dist/agent2api-providers

# 安装为独立版本目录（官方目录不动，可随时回滚）
./tools/install-lobsterai-fix.sh --home <agent2api-data-dir> \
             --version <上游版本>-fx6 \
             --binary dist/agent2api-providers

# 重启网关，日志应出现:
#   插件包「agent2api-providers」v*-fx6 已加载

# 回滚
./tools/install-lobsterai-fix.sh --home <agent2api-data-dir> --rollback
```

低内存构建参数已内置（1.6GB RAM 可跑）：LTO off、codegen-units=8、可选 mold 链接器；国内环境建议 rsproxy + USTC 镜像。

## 签到窗口调整

```bash
python3 tools/set-checkin-window.py --data-dir <agent2api-data-dir> \
        --start 00:01 --end 00:40
# 自动：停容器 → 备份 DB → 改 time/timeEnd/retryTime → 启容器 → 复核
```

脚本会同步 `retryTime = timeEnd`（补签轮跟着窗口后移），并在失败时自动回滚。

## 与官方更新并存

本包以 `signed: false` 本地包身份安装：

- 网关不做 ed25519 验签，直接接受加载（官方 beta 网关实测通过）
- **不会被官方自动更新覆盖**
- 跟进上游：`git fetch upstream && git rebase upstream/main`（fx 提交均为小补丁集，冲突面极小）

## 验证

```bash
cargo test -p agent2api-providers     # 22/22 · provider 层（含 LobsterAI wire 断言与聊天 UA 断言）
cargo test -p agent2api-provider-sdk  # 3/3  · SDK 层（fx6 撤回闸机后回到上游基线）
```

TLS 指纹验证（Roadmap 落地后）：`tls.peet.ws` 对比改造前后 JA3 / JA4 / JA4H。

## Roadmap

- [ ] **TLS 指纹伪装**（务实档）：依赖 `reqwest → rquest` 全局替换（BoringSSL），共享层 4 个构建点 + 全员 Chrome profile；验证 peet.ws JA3/JA4
- [ ] 多窗口分流（把部分 provider 的 `daily_window_start` 挪到 10:01 / 19:01，进一步降密度）
- [ ] 上游 PR 反哺（HTTP 身份修复可提交回 [ProvidersPlugin/Agent2Providers](https://github.com/ProvidersPlugin/Agent2Providers)）

## 改动清单

```
f7cf1dd  fx6: 撤销签到避让，checkin.rs 还原上游基线
ca03bb7  fx4: 聊天 UA 补齐（5 家 plan）＋ fx3 两处修正（mine.take 死逻辑、抖动真随机）
b95baec  fx5: 推迟改报 Inactive、planned 到点放行、retry_at 垫底、loomy 图像 UA
7322c30  fx3: SDK 同供应商签到避让（已于 fx6 撤销）
a088c96  fx2: 全线 HTTP 身份补齐——消灭 undici 自曝（共享层默认身份 + modelscope 浏览器头）
13b2f56  fx:  仓库脚手架——构建脚本、安装钩子、说明文档
5a5de37  fx:  LobsterAI 官方客户端指纹对齐 + 兜底版本 2026.11.16
```

详细变更说明见 [README-FX.md](./README-FX.md)。

## 来源与许可

| 仓库 | 角色 |
|---|---|
| [aimod-cc/Agent2API](https://github.com/aimod-cc/Agent2API) | 网关核心（本仓库不修改，上游仅引用） |
| [ProvidersPlugin/Agent2Providers](https://github.com/ProvidersPlugin/Agent2Providers) | **上游源码**（本仓库的直接基础） |
| [free6589/agent2api-lobsterai-plugin](https://github.com/free6589/agent2api-lobsterai-plugin) | LobsterAI 适配参考实现（AGPL-3.0） |

本项目及全部上游均以 **AGPL-3.0** 授权：自用无限制；对外提供服务须向使用者提供对应源码；分发须继续以 AGPL-3.0 开源。