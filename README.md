<div align="center">

# Agent2Providers · Safe Fork

**基于官方 providers 插件源码的加固分支** — HTTP 身份对齐官方客户端 · 签到避让调度 · 可复现构建

[![License](https://img.shields.io/badge/license-AGPL--3.0-blue)](./LICENSE)
[![Upstream](https://img.shields.io/badge/upstream-3.5.0--beta.1--same--source-2ea44f)](https://github.com/ProvidersPlugin/Agent2Providers)
[![fx](https://img.shields.io/badge/fx--series-fx1→fx2→fx3-orange)](#-改动清单)
[![Tests](https://img.shields.io/badge/tests-27%2F27-success)](#-验证)

</div>

---

## 为什么有这个 fork

官方插件包的出站请求在**应用层**存在多处自曝特征，风控一眼可辨：

| 自曝点 | 官方包行为 | 本仓库行为 |
|---|---|---|
| 全局默认 UA | `undici`（Node.js fetch 标识） | Chrome/147 浏览器形态 |
| LobsterAI UA | `LobsterAI/0.1.0`（真实客户端是 Electron） | Electron/43.5.0 形态 |
| keyfrom 归因 | 登录时间戳（每次不同，可聚类识别） | 官方常量 `official` |
| 版本头 | 三处矛盾（2026.9.4 / 2026.11.6） | 统一 `2026.11.16`（与官方更新接口同步） |
| 同供应商签到 | 多账号背靠背（实测间隔 22 秒） | 同家间隔 ≥30 分钟 + 随机抖动 |

设计原则：**只改"长什么样"，不改"做什么"** — 业务语义、接口行为、数据格式零变更。

> 背景议题：[aimod-cc/Agent2API#590](https://github.com/aimod-cc/Agent2API/issues/590)

## HTTP 层适配矩阵

### ① 逐家深度适配

| Provider | 适配内容 | 来源 |
|---|---|---|
| **lobsterai** | UA → Electron/43.5.0、keyfrom → `official`、版本头三处统一 2026.11.16、Capabilities 官方四段、W3C traceparent、`x-lobster-*` 归因头 | fx1 |
| **modelscope** | 网页会话补 Chrome UA + `Referer`/`Origin` + `sec-fetch-*` + `Accept`（此前仅有 Cookie 一项头，签到/余额/用量全走此通道） | fx2 |

### ② 全局默认身份（fx2 共享层，一处覆盖全部）

`http.rs` 共享客户端默认身份 `undici` → **Chrome/147 + Accept-Language**。所有未显式声明 UA 的请求自动升级，直接受益（签到/余额路径原本裸奔 undici）：

```
autoclaw · loomy · codearts · astudio · officeace
```

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

## 签到避让（fx3）

SDK 层机制（`provider-sdk/src/checkin.rs`，全部 24 家共用），解决同供应商多账号在宿主窗口内背靠背的问题：

```
规则：同家其它账号的 max(上次触碰, 计划占用) + 30min 之内不开签
      叠加 0–10 分钟随机抖动；被推迟账号经 CheckinResult.retry_at 由宿主自动重排
```

| 特性 | 行为 |
|---|---|
| 执行位置 | `checkin::run()` 门槛，**先于 token 刷新**——被推迟账号零上游痕迹 |
| manual 触发 | 面板手动签到直接放行（但记账） |
| 状态过期 | 6 小时，跨天自然失效，不影响宿主补签轮 |
| 计划稳定性 | 自己已有 `planned` 未到点 → 按既定计划走，不重新随机 |
| 加号 | 实时读取账号列表，新号零配置自动纳入错峰链 |
| 完成保证 | 宿主补签轮兜底，全部账号当天完成 |

> ⚠️ 例外：**整点抢额度型平台**加 2+ 号时，第 2 号起会被推迟 30 分钟，可能错过窗口——此类平台需在 SDK 中加入豁免常量（一行）。每日额度型平台无此问题。

## 快速开始

```bash
# 构建（Ubuntu 22.04 / glibc ≤ 2.36 宿主机，x86_64）
git clone https://github.com/liaohuiwen666/Agent2api-safe.git
cd Agent2api-safe
./build-fx.sh                     # 产物: dist/agent2api-providers

# 安装为独立版本目录（官方目录不动，可随时回滚）
cd install-kit
./install.sh --home <agent2api-data-dir> \
             --version <上游版本>-fx3 \
             --binary ../dist/agent2api-providers

# 重启网关，日志应出现:
#   插件包「agent2api-providers」v*-fx3 已加载

# 回滚
./install.sh --home <agent2api-data-dir> --rollback
```

低内存构建参数已内置（1.6GB RAM 可跑）：LTO off、codegen-units=8、可选 mold 链接器；国内环境建议 rsproxy + USTC 镜像。

## 与官方更新并存

本包以 `signed: false` 本地包身份安装：

- 网关不做 ed25519 验签，直接接受加载（官方 beta 网关实测通过）
- **不会被官方自动更新覆盖**
- 跟进上游：`git fetch upstream && git rebase upstream/main`（fx 提交均为小补丁集，冲突面极小）

## 验证

```bash
cargo test -p agent2api-providers     # 19/19 · provider 层（含 LobsterAI wire 断言）
cargo test -p agent2api-provider-sdk  # 8/8  · 签到避让（含复现 22 秒背靠背场景）
```

TLS 指纹验证（Roadmap 落地后）：`tls.peet.ws` 对比改造前后 JA3 / JA4 / JA4H。

## Roadmap

- [ ] **TLS 指纹伪装**（务实档）：依赖 `reqwest → rquest` 全局替换（BoringSSL），共享层 4 个构建点 + 全员 Chrome profile；验证 peet.ws JA3/JA4
- [ ] 抢额度型平台签到豁免配置化
- [ ] 上游 PR 反哺（HTTP 身份修复可提交回 [ProvidersPlugin/Agent2Providers](https://github.com/ProvidersPlugin/Agent2Providers)）

## 改动清单

```
7322c30  fx3: SDK 同供应商签到避让——同家账号间隔≥30分钟+0~10分钟抖动
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
