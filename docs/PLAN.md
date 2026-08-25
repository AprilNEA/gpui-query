# gpui-query 实施计划

定位:**swr-rs 的 gpui 高层绑定**(vercel/swr 之于 React)。路线 A,详见
[swr-rs-notes.md](./swr-rs-notes.md)。本文件是所有实施 thread 的单一事实来源。

## 架构(三层,勿偏离)

```
swr-core 0.1(缓存、SWR 状态机、single-flight 去重、retry、mutate/乐观更新、gc)
    ↑ Runtime trait
GpuiRuntime(executor.now / spawn.detach / timer)——复用 swr-gpui 的实现思路,必要时内联
    ↑
gpui-query 绑定层:
  QueryClient(App Global,包 SwrClient)
  Query<T, E>(视图句柄:core QueryHandle + Entity<镜像状态> + watcher Task)
  keepPreviousData(绑定层实现,core 没有)
  focus 接线(observe_window_activation → broadcast(SwrEvent::Focus))
  online(set_online → broadcast(SwrEvent::Online))
  mutate / invalidate / invalidate_prefix 便捷封装
```

关键设计点(相对任务书原文的修正,以 swr-core 实际形态为准):

1. **不自建注册表/去重/序号/gc**——全部活在 swr-core。绑定层的 Entity 是每个 Query 句柄
   的状态镜像(`cx.new` 创建,watcher task 把 `handle.changed().await` 翻译成
   `weak_entity.update(cx, |s, cx| { *s = snapshot; cx.notify() })`)。视图 observe 该 Entity。
2. **PartialEq 短路**:watcher 收到 snapshot 后与镜像比较,未变不 notify(core 的
   `subscribe_eq` 可用则优先)。
3. **竞态/去重/重试语义由 core 保证,由我们的确定性测试锁死**(测试即规格)。
4. **Task drop 语义**:watcher Task 存在 Query 内,Query Drop → 退订 + 停桥接;core 的
   fetch 是 detached spawn,不因句柄 Drop 而取消,"即将到手的数据"不浪费,条目寿命由
   core gc 决定。
5. **fetcher 默认 Send(BackgroundExecutor 经 GpuiRuntime 驱动)**;`new_local` 变体
   v1 先不做(core 的 native 边界要求 Send,放 v2,README 写明)。
6. 时间纪律:绑定层代码禁 `std::time::Instant::now()`/`std::thread::sleep`,
   由 `scripts/check-time-discipline.sh` 在 CI grep。

## 公开 API(骨架已锁,见 crates/gpui-query/src/lib.rs)

```rust
gpui_query::init(cx);                                  // SwrClient + GpuiRuntime 装入 Global
let client = gpui_query::client(cx);                   // 取 Global

let q = Query::new(("user", id), |k| async move { .. }, cx)  // K: IntoSegments 元组
    .stale_time(Duration::from_secs(30))
    .keep_previous_data(true)
    .revalidate_on_focus(true);
q.state(cx) -> QueryState<T, E>                        // Loading / Ready{data, is_validating} / Error{error, stale_data}
q.set_key(new_key, cx)                                 // 切 key(keepPreviousData 生效点)
q.refetch(cx)

client.invalidate(key, cx); client.invalidate_prefix(prefix, cx);
client.mutate(key, Some(optimistic), fut, cx);         // 成功覆盖+失效;失败回滚
client.set_online(bool, cx);
client.on_focus(cx);                                   // 手动降级入口(自动接线见下)
```

builder 方法在首次 `state()`/订阅生效前收集 options,`Query::new` 惰性订阅或
重订阅(core 每订阅独立 opts,重订阅成本可接受)——实现 thread 定夺,保持链式形态不变。

## 实施波次(多 thread 并行)

### Wave 1(并行)

- **Thread A `feat/core`(high)**:实施顺序 2→5
  1. GpuiRuntime 接入 + 确定性测试骨架:`#[gpui::test]` 下 advance_clock 驱动 core 的
     stale 判定与 retry timer(跑通即一切可测)。
  2. init/client Global;Query 句柄(订阅、镜像 Entity、watcher、Drop 退订)。
  3. SWR 语义逐条确定性测试:stale 到期自动重验证 / 去重(inflight + stale_time 窗口)/
     竞态(慢请求后发先至被 core seq 丢弃)/ 错误退避重试 / keepPreviousData(绑定层实现)。
  4. mutation 三路径测试:成功覆盖 / 失败回滚(快照先拍,顺序锁死)/ settled 失效。
  5. focus/online 接线:`QueryClient::attach_window(window, cx)` 或 init 内自动
     (核对 0.2.2 的 observe_window_activation 需要 Context<T>,用内部 Entity 持 Subscription)
     + focus_throttle(core 已有)确定性测试;online 队列行为测试。
  6. 双视图共享缓存只发一次请求;句柄全 Drop 后 core gc 生效(条目消失)测试。
- **Thread B `feat/demo`(high)**:demo 应用(`crates/gpui-query/examples/demo.rs` 或
  examples crate),针对骨架 API 编写(能编译;行为待 A 合入):
  主从视图(列表+详情,共享缓存 + keepPreviousData 切 key 无闪白)、
  乐观更新 todo(三路径)、故意 3s 慢接口(stale 展示 + is_validating 指示)。
  orb 无 GPU:验证以编译 + TestAppContext 冒烟为主,运行验证留给用户本机。

### Wave 2(A、B 合入 main 后,并行)

- **Thread C(high)**:基准(1000 订阅者单次 notify 开销,写入 README)+ README 完整版
  (三行接入、vercel/swr 选项对照表、swr-rs 复用说明、降级说明、gpui 版本注记、v2 路线)
  + CI(fmt/clippy/test + 时间纪律 grep)。
- 集成验收由规划 thread(本 thread)对照第 7 节验收清单执行。

## Git 流程

骨架直推 `main`;实施 thread 各自在 `feat/*` 分支工作并推分支;规划 thread 负责合并回
`main`。任何 thread 不得改写他人分支或 force-push main。

## 验收标准(照抄任务书,逐条勾)

- [x] swr-rs 问卷笔记 + 路线决策(docs/swr-rs-notes.md;A 路线,无需上游 PR)
- [ ] 全部 SWR 语义测试在 TestAppContext 虚拟时钟下确定性通过,零真实 sleep
- [ ] 双视图共享缓存只发一次请求;句柄全释放后条目被 gc
- [ ] 乐观更新三路径(成功/失败回滚/失效)测试通过
- [ ] 竞态测试:慢请求后发先至被丢弃
- [ ] demo 三场景编译通过 + 冒烟;切 key 无闪白(keepPreviousData)
- [ ] 基准:1000 订阅者单次 notify 开销写入 README
- [ ] README 完整,含 swr 选项对照表与降级说明

## v2 路线(README 记录,首版不做)

持久化缓存(CacheStore trait + serde 落盘预热)、infinite query/分页游标、依赖查询、
`new_local`(非 Send fetcher)、gpui-devtools 集成、network-monitor feature。SSR 永不做。
