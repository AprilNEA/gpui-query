# gpui-query

Declarative async data for [gpui](https://www.gpui.rs): stale-while-revalidate
caching, request deduplication, invalidation and optimistic updates — deeply
integrated with gpui's entity reactivity (data changes → subscribed views
re-render).

Like TanStack Query / vercel-swr, but for gpui. Powered by
[swr-rs](https://github.com/AprilNEA/swr-rs): all SWR semantics (cache,
single-flight dedup, race resolution, retry, GC, optimistic updates) live in
`swr-core`; this crate is the gpui binding layer.

## Quick start

These are the three integration points, distilled from the
[demo](crates/gpui-query/examples/demo.rs): initialize once, construct a query
inside an entity, and match its state while rendering.

```rust
// 1. Once at application startup (`cx: &mut gpui::App`).
gpui_query::init(cx);

// 2. In an entity constructor (`cx: &mut gpui::Context<Self>`).
let executor = cx.background_executor().clone();
let detail_api = api.clone();
let detail_executor = executor.clone();
let detail = Query::new(
    ("user", 1_u64),
    move |(_, id): (&'static str, u64)| {
        let api = detail_api.clone();
        let executor = detail_executor.clone();
        async move { api.fetch_user(id, executor).await }
    },
    cx,
)
.stale_time(Duration::from_secs(30));

// 3. In Render::render.
let status = match self.detail.state(cx) {
    QueryState::Loading => "loading".to_string(),
    QueryState::Ready { data, is_validating } => {
        format!("{} (validating: {is_validating})", data.name)
    }
    QueryState::Error { error, .. } => format!("error: {error}"),
};
```

`Query::new` automatically observes its mirror entity from the calling view,
so a changed result calls `notify` and re-renders that view. Keys are typed,
structured tuples; the same key and `(T, E)` pair shares one cache entry and
one in-flight request.

## vercel/swr option mapping

The table maps SWR names to APIs that exist in the current crate. Durations are
Rust `Duration` values.

| vercel/swr | gpui-query | Semantics and default |
|---|---|---|
| `fetcher` | The second argument to `Query::new(key, fetcher, cx)` | Receives the full typed key and returns a `Send` future. |
| `dedupingInterval` | `.stale_time(duration)` | There is no separate dedup timer. Concurrent requests are always single-flight; fresh cached data suppresses another fetch for `stale_time`, which defaults to 2s. |
| `revalidateOnFocus` | `.revalidate_on_focus(bool)` plus `attach_window(window, cx)` | Defaults to `true`. Call `attach_window` once for each window, or signal focus manually with `client(cx).on_focus()`. |
| `focusThrottleInterval` | `.focus_throttle(duration)` | Defaults to 5s. |
| `revalidateOnReconnect` | `.revalidate_on_online(bool)` plus `client(cx).set_online(bool)` | Defaults to `true`; only a `false` → `true` transition broadcasts the online event. |
| `refreshInterval` | `.refresh_interval(duration)` | Defaults to off. The timer runs while the entry is subscribed. |
| `errorRetryCount`, `errorRetryInterval` | `.retry(RetryPolicy { max_retries, interval })` | Retry is opt-in on `Query`. `RetryPolicy::default()` is 3 retries with a 5s base interval and exponential backoff. |
| `keepPreviousData` | `.keep_previous_data(bool)` | Defaults to `false`; implemented in the binding layer when `set_key` switches keys. |
| `mutate`, `optimisticData`, `rollbackOnError` | `client(cx).mutate(key, optimistic, future, cx)` | The convenience API writes the optimistic value, rolls back on error, populates a successful result, and revalidates. For different flags, use `client(cx).inner().mutate` with `MutateOptions`. `client(cx).set` is the synchronous local-write form. |
| `useSWRImmutable` | Core-level `QueryOptions::immutable()`; high-level equivalent: `.stale_time(Duration::MAX).revalidate_on_focus(false).revalidate_on_online(false)` | Automatic stale, focus, and online revalidation are disabled; manual invalidation/refetch still works. There is no dedicated high-level `immutable` builder yet. |

Other binding APIs include `Query::refetch`, `QueryClient::invalidate`, and
`QueryClient::invalidate_prefix`.

## Architecture

```diagram
┌──────────────────────────┐
│ swr-core 0.1             │
│ cache and SWR semantics  │
└────────────▲─────────────┘
             │ Runtime trait
┌────────────┴─────────────┐
│ GpuiRuntime              │
│ GPUI clock/spawn/timer   │
└────────────▲─────────────┘
             │ high-level binding
┌────────────┴─────────────┐
│ gpui-query               │
│ Global, Query, entities  │
└──────────────────────────┘
```

The project reuses [`swr-core` from AprilNEA/swr-rs](https://github.com/AprilNEA/swr-rs)
rather than rebuilding a cache. The core owns cache entries, structured keys,
freshness, single-flight deduplication, race resolution, retry, refresh timers,
mutation/rollback, invalidation, and garbage collection. `GpuiRuntime` adapts
the core's runtime trait to GPUI's background executor clock, spawn, and timer.

The binding layer owns the app-global `QueryClient`, each `Query` handle's
mirror `Entity`, the watcher that translates core snapshots to
`entity.update + notify`, `keep_previous_data`, and window-focus wiring. The
mirror is deliberately not another cache.

## Environment fallbacks

- Without a window activation source, call `gpui_query::client(cx).on_focus()`
  from the host application's focus callback.
- gpui-query does not monitor network connectivity. Feed the host's result to
  `gpui_query::client(cx).set_online(bool)`.
- The "offline pending queue" is approximate today: `set_online(false)`
  records connectivity but does not suspend new or in-flight fetches. When the
  host reports online again, all active stale entries that opted into online
  revalidation are revalidated. Fully suspending and queuing requests requires
  upstream swr-core support.

## Testing

The behavior suite uses GPUI's `TestAppContext`. Its `TestDispatcher` has a
virtual clock, so timers are tested deterministically with `advance_clock` and
`run_until_parked`, never wall-clock sleeps:

```rust
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, TestAppContext};
use gpui_query::Query;

struct TestView {
    _query: Query<usize, String>,
}

#[gpui::test]
async fn refresh_uses_the_virtual_clock(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let calls = Arc::new(AtomicUsize::new(0));
    let query_calls = Arc::clone(&calls);
    let _view = cx.update(|cx| {
        cx.new(|cx| TestView {
            _query: Query::new(
                ("refresh",),
                move |_| {
                    std::future::ready(Ok(query_calls.fetch_add(1, Ordering::SeqCst) + 1))
                },
                cx,
            )
            .refresh_interval(Duration::from_secs(3)),
        })
    });

    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    cx.executor().advance_clock(Duration::from_secs(3));
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
```

CI runs formatting, clippy for all workspace targets, the workspace test suite,
the demo build, and `scripts/check-time-discipline.sh`.

## Benchmarks

`cargo bench -p gpui-query --bench notify` creates one cache entry and one
`Query` mirror, attaches 1,000 observer entities, then measures from
`QueryClient::set` until all 1,000 callbacks have completed. Setup is excluded;
100 updates are measured in the optimized bench profile and the median is
reported. `TestAppContext` still supplies deterministic task dispatch and its
virtual clock; `std::time::Instant` is used only by this benchmark to measure
real elapsed time.

| Environment | Median for one 1,000-subscriber notify |
|---|---:|
| Amp orb VM (KVM), 16 vCPU, Intel Xeon Processor @ 2.60GHz, x86_64, rustc 1.98.0 | **326.64µs** |

This measures cache-to-entity callback propagation, not view rendering or GPU
work. Treat it as a reproducible baseline for this VM rather than a universal
latency guarantee.

## Demo

Run the full master-detail, optimistic mutation, and slow-endpoint demo with:

```sh
cargo run --example demo
```

The demo opens a real GPUI window and therefore requires a local GPU/display
environment.

## Version and v2 roadmap

gpui-query targets [gpui 0.2.x from crates.io](https://crates.io/crates/gpui)
(the current lockfile resolves 0.2.2).

The v2 roadmap is persistent caching (`CacheStore` trait plus serde-based disk
prewarming), infinite query/cursor pagination, dependent queries, `new_local`
for non-`Send` fetchers, gpui-devtools integration, and a `network-monitor`
feature. SSR will never be supported.

## License

MIT OR Apache-2.0
