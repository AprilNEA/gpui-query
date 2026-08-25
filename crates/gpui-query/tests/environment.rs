use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    div, AppContext as _, Context, Entity, IntoElement, Render, Subscription, TestAppContext,
    Window,
};
use gpui_query::{attach_window, Query, QueryState};

struct QueryView {
    query: Query<i32, String>,
}

struct WindowQueryView {
    _query: Query<i32, String>,
}

impl Render for WindowQueryView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn counter_fetcher(
    calls: Arc<AtomicUsize>,
) -> impl Fn((&'static str,)) -> std::future::Ready<Result<i32, String>> + Send + Sync + 'static {
    move |_| std::future::ready(Ok(calls.fetch_add(1, Ordering::SeqCst) as i32 + 1))
}

#[gpui::test]
async fn focus_revalidates_stale_entries_with_throttle_and_opt_out(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let query_client = cx.update(|cx| gpui_query::client(cx));
    let enabled_calls = Arc::new(AtomicUsize::new(0));
    let disabled_calls = Arc::new(AtomicUsize::new(0));

    let _enabled = cx.update(|cx| {
        cx.new(|cx| QueryView {
            query: Query::new(
                ("focus-enabled",),
                counter_fetcher(Arc::clone(&enabled_calls)),
                cx,
            )
            .stale_time(Duration::ZERO),
        })
    });
    let _disabled = cx.update(|cx| {
        cx.new(|cx| QueryView {
            query: Query::new(
                ("focus-disabled",),
                counter_fetcher(Arc::clone(&disabled_calls)),
                cx,
            )
            .stale_time(Duration::ZERO)
            .revalidate_on_focus(false),
        })
    });
    cx.run_until_parked();

    query_client.on_focus();
    cx.run_until_parked();
    assert_eq!(enabled_calls.load(Ordering::SeqCst), 2);
    assert_eq!(disabled_calls.load(Ordering::SeqCst), 1);

    query_client.on_focus();
    cx.run_until_parked();
    assert_eq!(
        enabled_calls.load(Ordering::SeqCst),
        2,
        "second focus inside the default five-second throttle is ignored"
    );

    cx.executor().advance_clock(Duration::from_secs(5));
    query_client.on_focus();
    cx.run_until_parked();
    assert_eq!(enabled_calls.load(Ordering::SeqCst), 3);
}

#[gpui::test]
async fn focus_throttle_builder_overrides_default_interval(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let query_client = cx.update(|cx| gpui_query::client(cx));
    let calls = Arc::new(AtomicUsize::new(0));

    let _view = cx.update(|cx| {
        cx.new(|cx| QueryView {
            query: Query::new(
                ("focus-throttled",),
                counter_fetcher(Arc::clone(&calls)),
                cx,
            )
            .stale_time(Duration::ZERO)
            .focus_throttle(Duration::from_secs(1)),
        })
    });
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    query_client.on_focus();
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    query_client.on_focus();
    cx.run_until_parked();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "second focus inside the custom one-second throttle is ignored"
    );

    cx.executor().advance_clock(Duration::from_secs(1));
    query_client.on_focus();
    cx.run_until_parked();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "focus after the custom throttle window revalidates again"
    );
}

#[gpui::test]
async fn online_only_broadcasts_on_false_to_true_transition(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let query_client = cx.update(|cx| gpui_query::client(cx));
    let enabled_calls = Arc::new(AtomicUsize::new(0));
    let disabled_calls = Arc::new(AtomicUsize::new(0));

    let _enabled = cx.update(|cx| {
        cx.new(|cx| QueryView {
            query: Query::new(
                ("online-enabled",),
                counter_fetcher(Arc::clone(&enabled_calls)),
                cx,
            )
            .stale_time(Duration::ZERO),
        })
    });
    let _disabled = cx.update(|cx| {
        cx.new(|cx| QueryView {
            query: Query::new(
                ("online-disabled",),
                counter_fetcher(Arc::clone(&disabled_calls)),
                cx,
            )
            .stale_time(Duration::ZERO)
            .revalidate_on_online(false),
        })
    });
    cx.run_until_parked();

    query_client.set_online(true);
    cx.run_until_parked();
    assert_eq!(enabled_calls.load(Ordering::SeqCst), 1);

    query_client.set_online(false);
    query_client.set_online(false);
    query_client.set_online(true);
    cx.run_until_parked();
    assert_eq!(enabled_calls.load(Ordering::SeqCst), 2);
    assert_eq!(disabled_calls.load(Ordering::SeqCst), 1);

    query_client.set_online(true);
    cx.run_until_parked();
    assert_eq!(enabled_calls.load(Ordering::SeqCst), 2);
}

#[gpui::test]
async fn attached_window_activation_broadcasts_focus(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let calls = Arc::new(AtomicUsize::new(0));
    let (_view, cx) = cx.add_window_view(|window, cx| {
        attach_window(window, cx);
        WindowQueryView {
            _query: Query::new(("window-focus",), counter_fetcher(Arc::clone(&calls)), cx)
                .stale_time(Duration::ZERO),
        }
    });
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    cx.deactivate_window();
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[gpui::test]
async fn equal_revalidation_updates_mirror_without_notifying_observers(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = cx.executor();
    let view = cx.update(|cx| {
        cx.new(|cx| {
            let calls = Arc::clone(&calls);
            let executor = executor.clone();
            let query = Query::new(
                ("equal",),
                move |_| {
                    let call = calls.fetch_add(1, Ordering::SeqCst);
                    let executor = executor.clone();
                    async move {
                        if call > 0 {
                            executor.timer(Duration::from_secs(3)).await;
                        }
                        Ok(7)
                    }
                },
                cx,
            );
            QueryView { query }
        })
    });
    cx.run_until_parked();

    let mirror: Entity<_> = cx.update(|cx| view.read(cx).query.entity().clone());
    let notifications = Arc::new(AtomicUsize::new(0));
    let _subscription: Subscription = cx.update(|cx| {
        let notifications = Arc::clone(&notifications);
        cx.observe(&mirror, move |_, _| {
            notifications.fetch_add(1, Ordering::SeqCst);
        })
    });

    cx.update(|cx| view.read(cx).query.refetch());
    cx.run_until_parked();
    assert_eq!(notifications.load(Ordering::SeqCst), 1);
    notifications.store(0, Ordering::SeqCst);

    cx.executor().advance_clock(Duration::from_secs(3));
    cx.run_until_parked();
    assert_eq!(
        notifications.load(Ordering::SeqCst),
        0,
        "subscribe_eq retained Arc identity for equal data"
    );
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready {
            data,
            is_validating,
        } => {
            assert_eq!(*data, 7);
            assert!(!is_validating, "mirror still records validation completion");
        }
        state => panic!("expected unchanged ready data, got {state:?}"),
    });
}
