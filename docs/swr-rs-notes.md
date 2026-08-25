# swr-rs 问卷笔记与路线决策(规范锚点,后续所有决策以此为准)

调研对象:[AprilNEA/swr-rs](https://github.com/AprilNEA/swr-rs),2026-08-23 世代,workspace 版本 0.1.x,
MIT OR Apache-2.0。已发布 crates.io:`swr 0.1.1` / `swr-core 0.1.0` / `swr-gpui 0.1.1`。

## 路线决策:A(纯绑定层),且比预期更进一步

**swr-core 已经运行时无关**(`Runtime` trait 已存在),且上游已有 `swr-gpui` crate
(`GpuiRuntime` + 薄 `Query` 桥接)。因此:

- 不需要给核心提 Runtime 抽象 PR(B 路线的前提不成立)。
- 不存在结构性冲突(C 路线不成立)。
- **gpui-query = 基于 `swr-core`(复用 `swr-gpui` 的 `GpuiRuntime` 思路)的高层 gpui 绑定**,
  补齐上游没有的部分:`init`/Global、任务书 API 形态的 `Query<T>`、keepPreviousData、
  focus/online 自动接线、mutate/invalidate 便捷封装、确定性测试套件、demo、基准。
- 适合回流上游的增量(如 focus 自动接线、keepPreviousData)后续以 PR 反哺 swr-rs。

## 问卷答案

### 1. 运行时无关?

是。`swr-core/src/runtime.rs`:

```rust
pub trait Runtime: MaybeSend + MaybeSync + 'static {
    fn now(&self) -> Instant;
    fn spawn(&self, fut: RuntimeFuture);          // RuntimeFuture = BoxedFuture<()>
    fn sleep_until(&self, at: Instant) -> RuntimeFuture;
}
```

- spawn / timer / clock 全走 trait;状态机(`machine.rs`)是纯同步 sans-I/O:事件进,
  状态变更 + `Effect` 出,不 await、不 spawn、不回调。
- **唯一硬编码点:通知用 `tokio::sync::watch`**(仅启用 tokio 的 `sync` feature,
  不需要 tokio executor)。对 gpui 绑定无碍:watch receiver 可在 gpui 前台任务里 `.changed().await`。
- 已有 runtime 实现:`swr-runtime-tokio`、`swr-runtime-web`、**`swr-gpui::GpuiRuntime`**
  (executor.now() / executor.spawn().detach() / executor.timer())。

### 2. 键模型

结构化、分层、支持前缀失效:

```rust
pub enum Segment { Str(Arc<str>), U64(u64), I64(i64), Bool(bool), Bytes(Arc<[u8]>) }
pub struct QueryKey { type_id: TypeId,  /* TypeId::of::<(T, E)>() */ segments: Arc<[Segment]> }
```

- 最多八元 tuple 构造:`("user", 42u64)` → segments。**没有浮点 Segment,
  key 的 Hash 稳定性坑在类型系统层面就堵死了。**
- 前缀失效:`client.invalidate(prefix)`,`matches_prefix` 忽略 TypeId,O(n) 扫描。
- 同 segments 不同 `(T, E)` 是不同条目(erased downcast 安全)。

### 3. 缓存条目状态机

状态由正交字段组合(非单一 enum):`data/error/data_seq/error_seq/updated_at/seq/inflight/
mutation_active/invalidated/optimistic/subscribers/gc_gen/refresh_gen`。快照层给出
`is_validating()`(inflight)与 `is_loading()`(validating 且无 data)。

- **stale 判定**:`invalidated || updated_at 为空 || now >= updated_at + stale_time`。
- **去重**:同 key 强制 single-flight(inflight 存在不重发);
  **没有独立 dedupingInterval —— `stale_time` 默认 2s,显式吸收 SWR 的 dedupingInterval 语义**。
- **重试**:`Retry` fetcher combinator。默认 5s 基础间隔、最多 3 次、指数退避封顶 shift 8、
  `retry_if` 过滤。整个 retry loop 是同一个 flight(重试期间持续 is_validating,继续去重)。
- **竞态防护**:核心自带单调 `seq`/`data_seq`,watch send 带 `version > current.version` 检查。
  绑定层无需自建序号,但要测到(慢请求后发先至被丢弃)。

### 4. 订阅/通知

- `client.subscribe(key, fetcher, opts)` → sub_id + `watch::Receiver<Snapshot>`;
  每订阅各自带 `QueryOptions`;Drop handle 自动退订。
- 全缓存单把 `parking_lot::Mutex<Inner>`;effect/fetcher/send 都在锁外执行。
- native 上跨 spawn 边界要求 `Send + Sync`(`MaybeSend`/`MaybeSync` 在 wasm 放宽)。
- watch 语义:只保证"变了",允许跳过中间态 —— 绑定层把 watch 变更翻译成 entity update + notify。
- `send_if_modified` + version 检查已抑制无效通知;绑定层再用 `PartialEq` 短路
  (core 另有 `subscribe_eq`,问卷后核对其确切语义并优先复用)。

### 5. SWR 语义清单

| 语义 | 上游状态 | gpui-query 要做的 |
|---|---|---|
| stale-while-revalidate | ✅ 完整 | 桥接 |
| dedup | ✅(single-flight + stale_time 吸收) | 文档写清语义映射 |
| revalidateOnFocus + focus_throttle | ✅ 选项与广播(`SwrEvent::Focus`)已有;**gpui 侧无自动源** | `init` 里接 `observe_window_activation` → `broadcast(Focus)` |
| revalidateOnReconnect | ✅(命名 Online,`SwrEvent::Online`) | `client.set_online(bool)` + 可选 feature 网络监测 |
| mutate + 乐观更新 | ✅(optimistic、rollback_on_error、populate、revalidate;取消安全 MutationGuard) | 便捷封装 `client.mutate(key, optimistic, fut, cx)` |
| 错误重试退避 | ✅ Retry combinator | builder 暴露 |
| keepPreviousData | ❌ 无 | **绑定层实现**(切 key 时旧 `Arc<T>` 保留到新数据到达) |

### 6. OpenLogi 用法校准

OpenLogi 的 GUI asset 层跑在 swr cache 上(生产验证)。可复用的品味信号:结构化 tuple key、
Arc 值共享、订阅句柄 Drop 即退订。(细节以 swr-gpui 的 `Query` 形态为准:
`Entity<QueryState>` + watcher task 把 `handle.changed()` 翻译成 `entity.update + notify` ——
这正是我们三层架构中间层的既有实现,直接站上去。)

## 与任务书规格的映射备忘

- 任务书的"条目 Entity + WeakEntity 注册表 + gc_hold"设计,在 A 路线下**大幅简化**:
  缓存/去重/gc 语义活在 swr-core(它自有 subscribers 计数与 gc),绑定层的 Entity 只是
  **每个 Query 句柄的状态镜像**,不需要自建注册表。QueryClient(Global)持有 `SwrClient`。
- 任务书的"去重窗口默认 2s" = core 的 `stale_time` 默认 2s,语义一致、来源同宗(vercel/swr)。
- 任务书的"竞态序号"核心已有;绑定层测试覆盖即可。
- Task drop 即取消:watcher task 存在 `Query` 内,Drop 即退订+停桥接;core 的 fetch 由
  `Runtime::spawn`(detached)驱动,不受句柄 Drop 影响,天然实现"最后订阅者退出后在飞请求
  不浪费"(结果照常写入缓存,core 的 gc 计时器决定条目寿命)。

## 已核实的 gpui 事实(gpui 0.2.2,crates.io)

1. `Task<T>` drop 即取消,`.detach()` 放行 —— `scheduler/executor.rs`。
2. `cx.observe_window_activation(window, cb)` 在 `Context<T>` 上(App 上没有),
   返回 `Subscription`,激活/失活都触发,状态从 `window` 查询。
3. 测试:`#[gpui::test]` + `TestAppContext`;`executor().advance_clock(d)` 推进虚拟钟
   (只推钟不跑任务),`run_until_parked()` 排空;`BackgroundExecutor::timer` 在
   TestDispatcher 下由 advance_clock 驱动;`executor.now()` 测试下读虚拟钟。
   需要 `gpui = { features = ["test-support"] }`(dev-dep)。
4. Global:`Global` marker trait + `set_global`/`global`/`try_global`/`update_global`;
   `AsyncApp` 用回调式 `read_global`/`update_global`。
5. Entity:`cx.new` / `entity.update` / `cx.observe` / `cx.notify` / `WeakEntity::upgrade`
   (及 `WeakEntity::update -> Result`,失败静默丢弃)。
6. 时间纪律:一切时间戳取自 `BackgroundExecutor::now()`(经 GpuiRuntime 进入 core),
   绑定层内禁用 `std::time::Instant::now()` 与 `std::thread::sleep`(CI grep)。
