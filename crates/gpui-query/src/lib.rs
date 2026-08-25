//! # gpui-query
//!
//! Declarative async data for [gpui]: stale-while-revalidate caching, request
//! deduplication, invalidation and optimistic updates, deeply integrated with
//! gpui's entity/notify reactivity.
//!
//! This crate is the high-level gpui binding for [swr-rs] — like `vercel/swr`
//! is to React. All SWR semantics (cache, dedup, race resolution, retry, GC)
//! live in `swr-core`; this crate wires them into gpui: a [`QueryClient`]
//! global, per-query mirror entities that `notify` observers, window-focus
//! revalidation, and ergonomic [`Query`] handles for views.
//!
//! [swr-rs]: https://github.com/AprilNEA/swr-rs
//!
//! ```ignore
//! // once, in main
//! gpui_query::init(cx);
//!
//! // in a view
//! struct UserPanel { user: Query<User> }
//!
//! impl UserPanel {
//!     fn new(id: u64, cx: &mut Context<Self>) -> Self {
//!         let user = Query::new(
//!             ("user", id),
//!             |(_, id): (&'static str, u64)| async move { api::fetch_user(id).await },
//!             cx, // auto-wires observe -> notify for this view
//!         )
//!         .stale_time(Duration::from_secs(30));
//!         Self { user }
//!     }
//! }
//!
//! // render
//! match self.user.state(cx) {
//!     QueryState::Loading => spinner(),
//!     QueryState::Ready { data, is_validating } => user_card(data, is_validating),
//!     QueryState::Error { error, stale_data } => error_view(error, stale_data),
//! }
//! ```

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use gpui::{App, Context, Entity, Global, Task, Window};
pub use swr_core::{
    Instant, IntoKeyPrefix, IntoQueryKey, IntoSegment, IntoSegments, MutateOptions, QueryKey,
    QueryOptions, ReadPolicy, Retry, RetryPolicy, SwrClient, SwrEvent,
};
pub use swr_gpui::GpuiRuntime;

/// Install a [`QueryClient`] (backed by gpui's `BackgroundExecutor` via
/// [`GpuiRuntime`]) as an app global. Call once from `main`.
pub fn init(cx: &mut App) {
    let client = QueryClient::new(swr_gpui::client(cx));
    cx.set_global(GlobalQueryClient(client));
}

/// The app-global [`QueryClient`] installed by [`init`]. Panics if [`init`]
/// was not called.
pub fn client(cx: &App) -> QueryClient {
    cx.try_global::<GlobalQueryClient>()
        .expect("gpui_query::init(cx) must be called before gpui_query::client(cx)")
        .0
        .clone()
}

struct GlobalQueryClient(QueryClient);
impl Global for GlobalQueryClient {}

/// Cheaply cloneable handle over the shared SWR cache.
#[derive(Clone)]
pub struct QueryClient {
    inner: SwrClient,
}

impl QueryClient {
    /// Wrap an existing [`SwrClient`] (advanced; most apps use [`init`]).
    pub fn new(inner: SwrClient) -> Self {
        Self { inner }
    }

    /// The underlying [`SwrClient`], for direct swr-core access.
    pub fn inner(&self) -> &SwrClient {
        &self.inner
    }

    /// Mark the entry at exactly `key` stale. Active (subscribed) entries
    /// refetch immediately; idle ones refetch on next read.
    pub fn invalidate(&self, key: impl IntoKeyPrefix) {
        self.inner.invalidate(key);
    }

    /// Mark every entry under `prefix` stale, e.g. `("user",)` invalidates
    /// `("user", 1)`, `("user", 2)`, ...
    pub fn invalidate_prefix(&self, prefix: impl IntoKeyPrefix) {
        self.inner.invalidate(prefix);
    }

    /// Synchronous local write (SWR's `mutate(key, data, { revalidate: false })`).
    pub fn set<K, T, E>(&self, key: K, value: T)
    where
        K: IntoQueryKey<T, E>,
        T: Send + Sync + 'static,
        E: 'static,
    {
        self.inner.set(key, value);
    }

    /// Async mutation with optional optimistic update.
    ///
    /// Writes `optimistic` (if any) into the cache immediately, runs `fut` on
    /// the background executor, then: on `Ok(Some(v))` populates the cache
    /// with `v` and revalidates; on `Err` rolls back to the pre-optimistic
    /// snapshot (unless something else wrote in between). Dropping the
    /// returned task aborts the mutation and rolls back (cancel-safe in
    /// swr-core).
    pub fn mutate<K, T, E, Fut>(
        &self,
        key: K,
        optimistic: Option<T>,
        fut: Fut,
        cx: &App,
    ) -> Task<Result<Option<Arc<T>>, Arc<E>>>
    where
        K: IntoQueryKey<T, E> + Send + 'static,
        T: Send + Sync + 'static,
        E: Send + Sync + 'static,
        Fut: Future<Output = Result<Option<T>, E>> + Send + 'static,
    {
        let _ = (key, optimistic, fut, cx);
        todo!("wave 1 (Thread A): spawn inner.mutate on background executor")
    }

    /// Manually signal "the app regained focus" — revalidates stale entries
    /// that opted into `revalidate_on_focus`, throttled by `focus_throttle`.
    /// Prefer [`attach_window`] for automatic wiring.
    pub fn on_focus(&self) {
        self.inner.broadcast(SwrEvent::Focus);
    }

    /// Report connectivity. A `false -> true` transition broadcasts
    /// [`SwrEvent::Online`], revalidating stale entries that opted into
    /// `revalidate_on_online`.
    pub fn set_online(&self, online: bool) {
        let _ = online;
        todo!("wave 1 (Thread A): track transition, broadcast Online on reconnect")
    }
}

/// Wire window activation to focus revalidation: on activation the client
/// broadcasts [`SwrEvent::Focus`] (per-entry `focus_throttle` applies).
/// Call once per window, e.g. inside `cx.open_window(...)`.
pub fn attach_window(window: &mut Window, cx: &mut App) {
    let _ = (window, cx);
    todo!("wave 1 (Thread A): internal entity + observe_window_activation")
}

/// What a view sees when it reads a query. Cheap to produce (Arc clones).
#[derive(Debug)]
pub enum QueryState<T, E = anyhow::Error> {
    /// No data yet (initial load in flight or not yet started).
    Loading,
    /// Data available. `is_validating` is true while a background
    /// revalidation is in flight — show a subtle spinner, keep the data.
    Ready {
        /// The cached value.
        data: Arc<T>,
        /// A background refresh is in flight.
        is_validating: bool,
    },
    /// The latest fetch failed. `stale_data` carries the last good value, if
    /// any, for degraded rendering.
    Error {
        /// The fetch error.
        error: Arc<E>,
        /// Last good data, if any.
        stale_data: Option<Arc<T>>,
    },
}

/// A live subscription to one cache entry, owned by a view.
///
/// Internally: an swr-core `QueryHandle` (RAII subscription), a mirror
/// [`Entity`] holding the latest typed state, and a foreground watcher task
/// translating watch-channel changes into `entity.update + cx.notify()`.
/// Dropping the `Query` drops watcher and handle: the subscription ends and
/// the entry follows swr-core GC (`gc_time`, default 300s).
///
/// Two views creating a `Query` for the same key share one cache entry and
/// one in-flight request (single-flight dedup in swr-core).
pub struct Query<T: 'static, E: 'static = anyhow::Error> {
    mirror: Entity<Mirror<T, E>>,
    _watcher: Task<()>,
}

/// Mirror entity state: latest core snapshot plus binding-layer extras
/// (previous data kept across key switches when `keep_previous_data`).
/// Opaque to users; observe it via [`Query::entity`], read via
/// [`Query::state`].
pub struct Mirror<T: 'static, E: 'static> {
    #[allow(dead_code)] // wave 1 (Thread A)
    state: swr_core::QueryState<T, E>,
    #[allow(dead_code)]
    previous_data: Option<Arc<T>>,
    #[allow(dead_code)]
    keep_previous_data: bool,
}

impl<T, E> Query<T, E>
where
    T: PartialEq + Send + Sync + 'static,
    E: Send + Sync + 'static,
{
    /// Subscribe to `key`, fetching with `fetcher` (a `Send` future run on
    /// the background executor). Automatically observes the query from the
    /// calling view: when data changes, the view re-renders — no manual
    /// `cx.observe` needed. Equal values (`T: PartialEq`) do not notify.
    ///
    /// Requires [`init`] to have been called.
    pub fn new<V, K, F, Fut>(key: K, fetcher: F, cx: &mut Context<V>) -> Self
    where
        V: 'static,
        K: IntoQueryKey<T, E> + Clone + Send + Sync + 'static,
        F: Fn(K) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
    {
        let _ = (key, fetcher, cx);
        todo!("wave 1 (Thread A): subscribe_eq + mirror entity + watcher + auto-observe")
    }

    /// Freshness window (absorbs SWR's `dedupingInterval`). Default: 2s.
    pub fn stale_time(self, duration: Duration) -> Self {
        let _ = duration;
        todo!("wave 1 (Thread A): update opts, resubscribe via watcher channel")
    }

    /// Idle-entry GC delay after the last subscriber drops. Default: 300s.
    pub fn gc_time(self, duration: Duration) -> Self {
        let _ = duration;
        todo!("wave 1 (Thread A)")
    }

    /// Background refresh interval while subscribed. Default: off.
    pub fn refresh_interval(self, duration: Duration) -> Self {
        let _ = duration;
        todo!("wave 1 (Thread A)")
    }

    /// Revalidate stale data when the window regains focus (requires
    /// [`attach_window`] or manual [`QueryClient::on_focus`]). Default: true.
    pub fn revalidate_on_focus(self, enabled: bool) -> Self {
        let _ = enabled;
        todo!("wave 1 (Thread A)")
    }

    /// Revalidate stale data when connectivity returns
    /// ([`QueryClient::set_online`]). Default: true.
    pub fn revalidate_on_online(self, enabled: bool) -> Self {
        let _ = enabled;
        todo!("wave 1 (Thread A)")
    }

    /// Retry failed fetches with exponential backoff (swr-core `Retry`).
    pub fn retry(self, policy: RetryPolicy) -> Self {
        let _ = policy;
        todo!("wave 1 (Thread A)")
    }

    /// Keep showing the previous key's data while the new key loads
    /// (no flash of Loading when paginating). Default: false.
    pub fn keep_previous_data(self, enabled: bool) -> Self {
        let _ = enabled;
        todo!("wave 1 (Thread A): binding-layer previous_data in Mirror")
    }

    /// Switch to a new key (same key type), e.g. next page. With
    /// [`keep_previous_data`](Self::keep_previous_data), the old data stays
    /// visible until the new key's data arrives.
    pub fn set_key<K>(&mut self, key: K)
    where
        K: IntoQueryKey<T, E> + Clone + Send + Sync + 'static,
    {
        let _ = key;
        todo!("wave 1 (Thread A): resubscribe, swap handle via watcher channel")
    }

    /// Read the current state. Pure read: never triggers fetches, safe in
    /// render. Revalidation is driven by subscription lifecycle, staleness
    /// timers, focus/online events and invalidation — not by reads.
    pub fn state(&self, cx: &App) -> QueryState<T, E> {
        let _ = cx;
        todo!("wave 1 (Thread A): map mirror -> Loading/Ready/Error (+previous_data)")
    }

    /// The mirror entity, to `cx.observe(...)` from additional views.
    pub fn entity(&self) -> &Entity<Mirror<T, E>> {
        &self.mirror
    }

    /// Request a revalidation now (deduplicated against in-flight fetches).
    pub fn refetch(&self) {
        todo!("wave 1 (Thread A): client.revalidate_key")
    }
}
