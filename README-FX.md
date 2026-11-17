# Agent2api-safe

Agent2API 生态的**安全化维护分支**：以 `ProvidersPlugin/Agent2Providers`（官方 providers 插件包源码）为基线，维护客户端指纹对齐与后续传输层修复。

## 定位

- **基线**：`ProvidersPlugin/Agent2Providers` main（与网关 v3.5.0-beta.1 官方插件包同源，34 家 provider）
- **本仓库改动**：`crates/providers/src/providers/lobsterai/` 整体替换为指纹修复实现（来源见下），`FALLBACK_CLIENT_VERSION` 升至 `2026.11.16`
- **不改动**：其余 33 家 provider 实现与上游逐字节一致，便于 rebase

## 许可与来源

AGPL-3.0（与上游一致，见 LICENSE）。

- `ProvidersPlugin/Agent2Providers` — 基线源码（AGPL-3.0）
- `free6589/agent2api-lobsterai-plugin` — `lobsterai/` 指纹修复实现与 `tools/install-lobsterai-fix.sh` 安装钩子（AGPL-3.0），改动清单见提交信息
- `aimod-cc/Agent2API` — 网关本体（另行 fork 时引用）

## 做了什么（LobsterAI 应用层指纹对齐）

| 项 | 官方包（3.5.0-beta.1） | 本仓库 |
|---|---|---|
| UA | `LobsterAI/0.1.0`（自曝） | Electron/43.5.0 形态，随版本头一致 |
| 版本头 | 2026.9.4（三处矛盾） | 运行时从官方更新接口取真实 CalVer（12h 缓存，兜底 2026.11.16） |
| keyfrom | 登录时间戳 | `official` |
| 能力位 | 2/4 段 | 官方 4 段 |
| traceparent | 无 | W3C traceparent（每 turn） |
| 归因头 | 无 | `x-lobster-session-id` 等 |

**未覆盖**：TLS/JA3 层（插件仍为裸 rustls）。见路线图。

## 构建

```bash
./build-fx.sh          # 输出 dist/agent2api-providers
```

- 在 **Linux 宿主机**编译，产物 glibc 版本 = 构建宿主 glibc，需 ≤ 运行容器（Debian 12 = 2.36）
- 内存 <2G 的机器：脚本已默认关 LTO、`codegen-units=8`、`-j2`；建议装 `mold` 作链接器
- 国内网络：rustup 走 USTC 镜像、crates 走 rsproxy（脚本内已配置）

## 安装 / 回滚

```bash
# 安装（独立版本目录，不覆盖官方包；signed=false 本地包，官方更新不会覆盖它）
./tools/install-lobsterai-fix.sh --home <数据目录> \
  --version <网关版本>-fx1 --binary dist/agent2api-providers

# 回滚（恢复 plugins.json 备份并删除本版本目录）
./tools/install-lobsterai-fix.sh --home <数据目录> --rollback
```

## 已验证状态（2026-11-17）

- 网关 `3.5.0-beta.1`，包 `v3.5.0-beta.1-fx1`：加载成功，31 家可用，无验签错误
- 用户现有 provider（trae/autoclaw/codearts/loomy/modelscope…）启停状态保留，回归零
- wire 单测 9/10：唯一失败为测试环境隔离问题（版本接口 URL 不受 mock 覆盖，实测会命中真实接口取到 2026.11.16），非功能缺陷

## 路线图

1. **TLS 层（进行中，待做）**：`reqwest` → `rquest`（Chrome impersonate），先 LobsterAI 单家 PoC，`tls.peet.ws` 验证 JA3/JA4/JA4H
2. **网关核心 fork**（按需）：`core::egress` 同步换 TLS 栈；其余场景不必 fork 网关，维持“官方镜像 + 本仓库插件包”的混合部署
3. **上游反哺**：补丁集成熟后向 `ProvidersPlugin/Agent2Providers` 提 PR

## 维护策略

保持「上游 main + 小补丁集」结构：`git fetch upstream && git rebase upstream/main`，补丁只动 `lobsterai/` 与常量，冲突面最小。
