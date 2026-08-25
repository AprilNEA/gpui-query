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

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures::channel::mpsc::{self, UnboundedSender};
use futures::{FutureExt as _, StreamExt as _};
use gpui::{App, AppContext as _, Context, Entity, Global, Subscription, Task, Window};
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
    online: Arc<AtomicBool>,
    window_attachments: Rc<RefCell<Vec<Entity<WindowAttachment>>>>,
}

impl QueryClient {
    /// Wrap an existing [`SwrClient`] (advanced; most apps use [`init`]).
    pub fn new(inner: SwrClient) -> Self {
        Self {
            inner,
            online: Arc::new(AtomicBool::new(true)),
            window_attachments: Rc::new(RefCell::new(Vec::new())),
        }
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
        let inner = self.inner.clone();
        cx.background_executor().spawn(async move {
            inner
                .mutate(
                    key,
                    MutateOptions {
                        optimistic,
                        ..MutateOptions::default()
                    },
                    fut,
                )
                .await
        })
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
        let was_online = self.online.swap(online, Ordering::AcqRel);
        if online && !was_online {
            self.inner.broadcast(SwrEvent::Online);
        }
    }
}

struct WindowAttachment {
    _subscription: Option<Subscription>,
}

/// Wire window activation to focus revalidation: on activation the client
/// broadcasts [`SwrEvent::Focus`] (per-entry `focus_throttle` applies).
/// Call once per window, e.g. inside `cx.open_window(...)`.
pub fn attach_window(window: &mut Window, cx: &mut App) {
    let query_client = client(cx);
    let focus_client = query_client.clone();
    let attachment = cx.new(|_| WindowAttachment {
        _subscription: None,
    });
    attachment.update(cx, |attachment, cx| {
        attachment._subscription =
            Some(cx.observe_window_activation(window, move |_, window, _| {
                if window.is_window_active() {
                    focus_client.on_focus();
                }
            }));
    });
    query_client
        .window_attachments
        .borrow_mut()
        .push(attachment);
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
    client: swr_core::WeakSwrClient,
    runtime: Arc<dyn swr_core::Runtime>,
    key: QueryKey,
    key_type: TypeId,
    key_value: ErasedKey,
    fetcher: Arc<ErasedQueryFetcher<T, E>>,
    opts: QueryOptions,
    retry_policy: Option<RetryPolicy>,
    keep_previous_data: bool,
    replacement_tx: UnboundedSender<WatchCommand<T, E>>,
}

/// Mirror entity state: latest core snapshot plus binding-layer extras
/// (previous data kept across key switches when `keep_previous_data`).
/// Opaque to users; observe it via [`Query::entity`], read via
/// [`Query::state`].
pub struct Mirror<T: 'static, E: 'static> {
    state: swr_core::QueryState<T, E>,
    previous_data: Option<Arc<T>>,
    keep_previous_data: bool,
}

type ErasedKey = Arc<dyn Any + Send + Sync>;
type ErasedQueryFetcher<T, E> =
    dyn Fn(ErasedKey) -> swr_core::BoxedFuture<Result<T, E>> + Send + Sync;

struct WatchCommand<T: 'static, E: 'static> {
    handle: swr_core::QueryHandle<T, E>,
    keep_previous_data: bool,
    key_changed: bool,
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
        let query_client = client(cx);
        let runtime: Arc<dyn swr_core::Runtime> = Arc::new(GpuiRuntime::new(cx));
        let query_key = key.clone().into_query_key();
        let key_value: ErasedKey = Arc::new(key);
        let fetcher = Arc::new(fetcher);
        let erased_fetcher: Arc<ErasedQueryFetcher<T, E>> = Arc::new(move |key| {
            let key = Arc::downcast::<K>(key)
                .expect("Query::set_key requires the same key type used by Query::new");
            Box::pin(fetcher((*key).clone()))
        });
        let opts = QueryOptions::default();
        let handle = subscribe_handle(
            &query_client.inner,
            &runtime,
            &query_key,
            &key_value,
            &erased_fetcher,
            &opts,
            None,
        );
        let initial_state = handle.snapshot();
        let mirror = cx.new(|_| Mirror {
            state: initial_state,
            previous_data: None,
            keep_previous_data: false,
        });
        let weak_mirror = mirror.downgrade();
        let (replacement_tx, mut replacement_rx) = mpsc::unbounded::<WatchCommand<T, E>>();
        let watcher = cx.spawn(async move |_, cx| {
            let mut handle = handle;
            loop {
                enum WatchEvent<T: 'static, E: 'static> {
                    Changed(Result<(), swr_core::Closed>),
                    Replace(Option<WatchCommand<T, E>>),
                }

                let event = {
                    let changed = handle.changed().fuse();
                    let replacement = replacement_rx.next().fuse();
                    futures::pin_mut!(changed, replacement);
                    futures::select_biased! {
                        replacement = replacement => WatchEvent::Replace(replacement),
                        changed = changed => WatchEvent::Changed(changed),
                    }
                };

                let (snapshot, keep_previous_data, key_changed) = match event {
                    WatchEvent::Changed(Ok(())) => (handle.snapshot(), None, false),
                    WatchEvent::Changed(Err(_)) | WatchEvent::Replace(None) => break,
                    WatchEvent::Replace(Some(command)) => {
                        handle = command.handle;
                        (
                            handle.snapshot(),
                            Some(command.keep_previous_data),
                            command.key_changed,
                        )
                    }
                };

                let updated = weak_mirror.update(cx, |mirror, cx| {
                    if apply_snapshot(mirror, snapshot, keep_previous_data, key_changed) {
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    break;
                }
            }
        });
        cx.observe(&mirror, |_, _, cx| cx.notify()).detach();

        Self {
            mirror,
            _watcher: watcher,
            client: query_client.inner.downgrade(),
            runtime,
            key: query_key,
            key_type: TypeId::of::<K>(),
            key_value,
            fetcher: erased_fetcher,
            opts,
            retry_policy: None,
            keep_previous_data: false,
            replacement_tx,
        }
    }

    /// Freshness window (absorbs SWR's `dedupingInterval`). Default: 2s.
    pub fn stale_time(mut self, duration: Duration) -> Self {
        self.opts.stale_time = duration;
        self.resubscribe(false);
        self
    }

    /// Idle-entry GC delay after the last subscriber drops. Default: 300s.
    pub fn gc_time(mut self, duration: Duration) -> Self {
        self.opts.gc_time = duration;
        self.resubscribe(false);
        self
    }

    /// Background refresh interval while subscribed. Default: off.
    pub fn refresh_interval(mut self, duration: Duration) -> Self {
        self.opts.refresh_interval = Some(duration);
        self.resubscribe(false);
        self
    }

    /// Revalidate stale data when the window regains focus (requires
    /// [`attach_window`] or manual [`QueryClient::on_focus`]). Default: true.
    pub fn revalidate_on_focus(mut self, enabled: bool) -> Self {
        self.opts.revalidate_on_focus = enabled;
        self.resubscribe(false);
        self
    }

    /// Minimum spacing between focus-triggered revalidations (SWR's
    /// `focusThrottleInterval`). Default: 5s.
    pub fn focus_throttle(mut self, duration: Duration) -> Self {
        self.opts.focus_throttle = duration;
        self.resubscribe(false);
        self
    }

    /// Revalidate stale data when connectivity returns
    /// ([`QueryClient::set_online`]). Default: true.
    pub fn revalidate_on_online(mut self, enabled: bool) -> Self {
        self.opts.revalidate_on_online = enabled;
        self.resubscribe(false);
        self
    }

    /// Retry failed fetches with exponential backoff (swr-core `Retry`).
    pub fn retry(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = Some(policy);
        self.resubscribe(false);
        self
    }

    /// Keep showing the previous key's data while the new key loads
    /// (no flash of Loading when paginating). Default: false.
    pub fn keep_previous_data(mut self, enabled: bool) -> Self {
        self.keep_previous_data = enabled;
        self.resubscribe(false);
        self
    }

    /// Switch to a new key (same key type), e.g. next page. With
    /// [`keep_previous_data`](Self::keep_previous_data), the old data stays
    /// visible until the new key's data arrives.
    pub fn set_key<K>(&mut self, key: K)
    where
        K: IntoQueryKey<T, E> + Clone + Send + Sync + 'static,
    {
        assert_eq!(
            TypeId::of::<K>(),
            self.key_type,
            "Query::set_key requires the same key type used by Query::new"
        );
        self.key = key.clone().into_query_key();
        self.key_value = Arc::new(key);
        self.resubscribe(true);
    }

    /// Read the current state. Pure read: never triggers fetches, safe in
    /// render. Revalidation is driven by subscription lifecycle, staleness
    /// timers, focus/online events and invalidation — not by reads.
    pub fn state(&self, cx: &App) -> QueryState<T, E> {
        let mirror = self.mirror.read(cx);
        if let Some(error) = &mirror.state.error {
            QueryState::Error {
                error: Arc::clone(error),
                stale_data: mirror.state.data.clone(),
            }
        } else if let Some(data) = &mirror.state.data {
            QueryState::Ready {
                data: Arc::clone(data),
                is_validating: mirror.state.is_validating,
            }
        } else if mirror.keep_previous_data {
            match &mirror.previous_data {
                Some(data) => QueryState::Ready {
                    data: Arc::clone(data),
                    is_validating: true,
                },
                None => QueryState::Loading,
            }
        } else {
            QueryState::Loading
        }
    }

    /// The mirror entity, to `cx.observe(...)` from additional views.
    pub fn entity(&self) -> &Entity<Mirror<T, E>> {
        &self.mirror
    }

    /// Request a revalidation now (deduplicated against in-flight fetches).
    pub fn refetch(&self) {
        if let Some(client) = self.client.upgrade() {
            client.revalidate_key(self.key.clone());
        }
    }

    fn resubscribe(&mut self, key_changed: bool) {
        let Some(client) = self.client.upgrade() else {
            return;
        };
        let handle = subscribe_handle(
            &client,
            &self.runtime,
            &self.key,
            &self.key_value,
            &self.fetcher,
            &self.opts,
            self.retry_policy.clone(),
        );
        self.replacement_tx
            .unbounded_send(WatchCommand {
                handle,
                keep_previous_data: self.keep_previous_data,
                key_changed,
            })
            .expect("query watcher lives as long as its replacement sender");
    }
}

fn subscribe_handle<T, E>(
    client: &SwrClient,
    runtime: &Arc<dyn swr_core::Runtime>,
    key: &QueryKey,
    key_value: &ErasedKey,
    fetcher: &Arc<ErasedQueryFetcher<T, E>>,
    opts: &QueryOptions,
    retry_policy: Option<RetryPolicy>,
) -> swr_core::QueryHandle<T, E>
where
    T: PartialEq + Send + Sync + 'static,
    E: Send + Sync + 'static,
{
    let segments = key.segments().to_vec();
    let key_value = Arc::clone(key_value);
    let fetcher = Arc::clone(fetcher);
    let fetch = move |_segments: Vec<swr_core::Segment>| fetcher(Arc::clone(&key_value));
    match retry_policy {
        Some(policy) => client.subscribe_eq(
            segments,
            Retry::new(Arc::clone(runtime), fetch, policy),
            opts.clone(),
        ),
        None => client.subscribe_eq(segments, fetch, opts.clone()),
    }
}

fn apply_snapshot<T, E>(
    mirror: &mut Mirror<T, E>,
    snapshot: swr_core::QueryState<T, E>,
    keep_previous_data: Option<bool>,
    key_changed: bool,
) -> bool {
    let old_previous_data = mirror.previous_data.clone();
    let old_keep_previous_data = mirror.keep_previous_data;

    if key_changed {
        mirror.previous_data = keep_previous_data
            .filter(|enabled| *enabled)
            .and_then(|_| mirror.state.data.clone().or(mirror.previous_data.clone()));
    }
    if snapshot.data.is_some() {
        mirror.previous_data = None;
    }
    if let Some(enabled) = keep_previous_data {
        mirror.keep_previous_data = enabled;
    }

    let changed = !same_state(&mirror.state, &snapshot)
        || !same_optional_arc(&old_previous_data, &mirror.previous_data)
        || old_keep_previous_data != mirror.keep_previous_data;
    mirror.state = snapshot;
    changed
}

fn same_state<T, E>(left: &swr_core::QueryState<T, E>, right: &swr_core::QueryState<T, E>) -> bool {
    let same_values =
        same_optional_arc(&left.data, &right.data) && same_optional_arc(&left.error, &right.error);
    if !same_values {
        return false;
    }

    if left.is_loading == right.is_loading && left.is_validating == right.is_validating {
        return true;
    }

    // subscribe_eq preserves the Arc when a revalidation commits equal data.
    // The mirror still records validation completion, but its observers need
    // not rebuild for a payload whose identity did not change.
    left.data.is_some()
        && left.is_validating
        && !right.is_validating
        && !left.is_loading
        && !right.is_loading
}

fn same_optional_arc<T>(left: &Option<Arc<T>>, right: &Option<Arc<T>>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}
