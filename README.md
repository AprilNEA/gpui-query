# gpui-query

Declarative async data for [gpui](https://www.gpui.rs): stale-while-revalidate
caching, request deduplication, invalidation and optimistic updates — deeply
integrated with gpui's entity reactivity (data changes → subscribed views
re-render).

Like TanStack Query / vercel-swr, but for gpui. Powered by
[swr-rs](https://github.com/AprilNEA/swr-rs): all SWR semantics (cache,
single-flight dedup, race resolution, retry, GC, optimistic updates) live in
`swr-core`; this crate is the gpui binding layer.

**Status: under construction.** See [docs/PLAN.md](docs/PLAN.md) for the
implementation plan and [docs/swr-rs-notes.md](docs/swr-rs-notes.md) for the
swr-rs survey and architecture decisions.

```rust
// once, in main
gpui_query::init(cx);

// in a view
let user = Query::new(
    ("user", id),
    |(_, id): (&'static str, u64)| async move { api::fetch_user(id).await },
    cx,
)
.stale_time(Duration::from_secs(30));

// render
match self.user.state(cx) {
    QueryState::Loading => spinner(),
    QueryState::Ready { data, is_validating } => user_card(data, is_validating),
    QueryState::Error { error, stale_data } => error_view(error, stale_data),
}

// anywhere
gpui_query::client(cx).invalidate_prefix(("user",));
```

## License

MIT OR Apache-2.0
