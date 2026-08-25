use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext as _, TestAppContext};
use gpui_query::{Query, QueryState, RetryPolicy};

struct QueryView {
    query: Query<i32, String>,
}

#[gpui::test]
async fn virtual_clock_drives_retry_timer(cx: &mut TestAppContext) {
    cx.update(gpui_query::init);
    let calls = Arc::new(AtomicUsize::new(0));
    let view = cx.update(|cx| {
        cx.new(|cx| {
            let calls = Arc::clone(&calls);
            let query = Query::new(
                ("retry-clock",),
                move |_| {
                    let call = calls.fetch_add(1, Ordering::SeqCst);
                    async move {
                        match call {
                            0 => Ok(7),
                            1 => Err("temporary".to_string()),
                            _ => Ok(8),
                        }
                    }
                },
                cx,
            )
            .retry(RetryPolicy {
                interval: Duration::from_secs(1),
                max_retries: Some(1),
            });
            QueryView { query }
        })
    });

    cx.run_until_parked();
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready { data, .. } => assert_eq!(*data, 7),
        state => panic!("expected initial data, got {state:?}"),
    });

    cx.update(|cx| view.read(cx).query.refetch());
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 2, "retry is waiting");
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready {
            data,
            is_validating,
        } => {
            assert_eq!(*data, 7);
            assert!(is_validating);
        }
        state => panic!("expected stale data during retry, got {state:?}"),
    });

    // RetryPolicy waits interval << 1 for the first retry.
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    cx.update(|cx| match view.read(cx).query.state(cx) {
        QueryState::Ready {
            data,
            is_validating,
        } => {
            assert_eq!(*data, 8);
            assert!(!is_validating);
        }
        state => panic!("expected retried data, got {state:?}"),
    });
}
