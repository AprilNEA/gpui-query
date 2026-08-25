use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use gpui::{
    div, percentage, prelude::*, px, rgb, size, svg, Animation, AnimationExt as _, App,
    Application, AssetSource, BackgroundExecutor, Bounds, Context, Entity, FocusHandle,
    IntoElement, Render, SharedString, Transformation, Window, WindowBounds, WindowOptions,
};
use gpui_query::{Query, QueryState};

const SURFACE: u32 = 0xffffff;
const BACKGROUND: u32 = 0xf4f6f8;
const BORDER: u32 = 0xd9dee5;
const TEXT: u32 = 0x17202a;
const MUTED: u32 = 0x667085;
const ACCENT: u32 = 0x2563eb;
const ACCENT_SOFT: u32 = 0xe8f0ff;
const ERROR: u32 = 0xb42318;
const SPINNER_PATH: &str = "demo-spinner.svg";
const SPINNER_SVG: &[u8] = br#"<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
<path d="M12 2a10 10 0 1 0 9.8 12h-3.1a7 7 0 1 1-2-6.9L14 10h8V2l-3.1 3.1A9.9 9.9 0 0 0 12 2Z"/>
</svg>"#;

struct DemoAssets;

impl AssetSource for DemoAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok((path == SPINNER_PATH).then_some(Cow::Borrowed(SPINNER_SVG)))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![SPINNER_PATH.into()])
    }
}

#[derive(Clone, Debug, PartialEq)]
struct User {
    id: u64,
    name: String,
    email: String,
    role: String,
}

#[derive(Clone, Debug, PartialEq)]
struct Todo {
    id: u64,
    title: String,
    completed: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct SlowReport {
    revision: u64,
    summary: String,
}

struct FakeState {
    users: HashMap<u64, User>,
    todos: HashMap<u64, Todo>,
}

#[derive(Clone)]
struct FakeApi {
    state: Arc<Mutex<FakeState>>,
    next_todo_id: Arc<AtomicU64>,
    mutation_attempt: Arc<AtomicU64>,
    slow_revision: Arc<AtomicU64>,
}

impl FakeApi {
    fn new() -> Self {
        let users = [
            User {
                id: 1,
                name: "Ada Lovelace".into(),
                email: "ada@example.test".into(),
                role: "Platform engineer".into(),
            },
            User {
                id: 2,
                name: "Grace Hopper".into(),
                email: "grace@example.test".into(),
                role: "Compiler engineer".into(),
            },
            User {
                id: 3,
                name: "Margaret Hamilton".into(),
                email: "margaret@example.test".into(),
                role: "Reliability engineer".into(),
            },
        ]
        .into_iter()
        .map(|user| (user.id, user))
        .collect();
        let todos = [
            Todo {
                id: 1,
                title: "Inspect shared query state".into(),
                completed: true,
            },
            Todo {
                id: 2,
                title: "Try an optimistic mutation".into(),
                completed: false,
            },
        ]
        .into_iter()
        .map(|todo| (todo.id, todo))
        .collect();

        Self {
            state: Arc::new(Mutex::new(FakeState { users, todos })),
            next_todo_id: Arc::new(AtomicU64::new(3)),
            mutation_attempt: Arc::new(AtomicU64::new(0)),
            slow_revision: Arc::new(AtomicU64::new(0)),
        }
    }

    async fn fetch_users(&self, executor: BackgroundExecutor) -> Result<Vec<User>, String> {
        executor.timer(Duration::from_millis(650)).await;
        let state = self.state.lock().expect("fake API state lock poisoned");
        let mut users = state.users.values().cloned().collect::<Vec<_>>();
        users.sort_by_key(|user| user.id);
        Ok(users)
    }

    async fn fetch_user(&self, id: u64, executor: BackgroundExecutor) -> Result<User, String> {
        executor.timer(Duration::from_millis(900)).await;
        self.state
            .lock()
            .expect("fake API state lock poisoned")
            .users
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("user {id} was not found"))
    }

    async fn fetch_todos(&self, executor: BackgroundExecutor) -> Result<Vec<Todo>, String> {
        executor.timer(Duration::from_millis(550)).await;
        Ok(self.todos())
    }

    fn reserve_todo_id(&self) -> u64 {
        self.next_todo_id.fetch_add(1, Ordering::Relaxed)
    }

    async fn add_todo(
        &self,
        todo: Todo,
        executor: BackgroundExecutor,
    ) -> Result<Option<Vec<Todo>>, String> {
        executor.timer(Duration::from_millis(800)).await;
        self.maybe_fail_mutation()?;
        self.state
            .lock()
            .expect("fake API state lock poisoned")
            .todos
            .insert(todo.id, todo);
        Ok(Some(self.todos()))
    }

    async fn toggle_todo(
        &self,
        id: u64,
        executor: BackgroundExecutor,
    ) -> Result<Option<Vec<Todo>>, String> {
        executor.timer(Duration::from_millis(800)).await;
        self.maybe_fail_mutation()?;
        let mut state = self.state.lock().expect("fake API state lock poisoned");
        let todo = state
            .todos
            .get_mut(&id)
            .ok_or_else(|| format!("todo {id} was not found"))?;
        todo.completed = !todo.completed;
        drop(state);
        Ok(Some(self.todos()))
    }

    async fn fetch_slow_report(&self, executor: BackgroundExecutor) -> Result<SlowReport, String> {
        executor.timer(Duration::from_secs(3)).await;
        let revision = self.slow_revision.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(SlowReport {
            revision,
            summary: format!("Slow response revision {revision} completed after 3 seconds"),
        })
    }

    fn todos(&self) -> Vec<Todo> {
        let state = self.state.lock().expect("fake API state lock poisoned");
        let mut todos = state.todos.values().cloned().collect::<Vec<_>>();
        todos.sort_by_key(|todo| todo.id);
        todos
    }

    fn maybe_fail_mutation(&self) -> Result<(), String> {
        let attempt = self.mutation_attempt.fetch_add(1, Ordering::Relaxed) + 1;
        if attempt % 5 == 0 {
            Err("simulated API failure (the optimistic value was rolled back)".into())
        } else {
            Ok(())
        }
    }
}

fn button(
    label: &'static str,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    div()
        .id(SharedString::from(format!("button-{label}")))
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(rgb(BORDER))
        .bg(rgb(SURFACE))
        .text_color(rgb(TEXT))
        .cursor_pointer()
        .hover(|style| style.bg(rgb(ACCENT_SOFT)).border_color(rgb(ACCENT)))
        .active(|style| style.opacity(0.75))
        .child(label)
        .on_click(on_click)
}

fn loading(message: &'static str) -> gpui::Div {
    div()
        .py_3()
        .text_color(rgb(MUTED))
        .child(format!("◌ {message}"))
}

fn error_message(error: &str) -> gpui::Div {
    div()
        .py_2()
        .text_color(rgb(ERROR))
        .child(format!("Error: {error}"))
}

fn validating_spinner() -> impl IntoElement {
    svg()
        .size_4()
        .path(SPINNER_PATH)
        .text_color(rgb(ACCENT))
        .with_animation(
            "query-validating-spinner",
            Animation::new(Duration::from_millis(850)).repeat(),
            |element, delta| element.with_transformation(Transformation::rotate(percentage(delta))),
        )
}

struct UserDetailView {
    selected_id: u64,
    detail: Query<User, String>,
    users_cache_probe: Query<Vec<User>, String>,
}

impl UserDetailView {
    fn new(api: FakeApi, cx: &mut Context<Self>) -> Self {
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
        .keep_previous_data(true);

        let probe = Query::new(
            ("users",),
            move |(_resource,): (&'static str,)| {
                let api = api.clone();
                let executor = executor.clone();
                async move { api.fetch_users(executor).await }
            },
            cx,
        );

        Self {
            selected_id: 1,
            detail,
            users_cache_probe: probe,
        }
    }

    fn select(&mut self, id: u64, cx: &mut Context<Self>) {
        self.selected_id = id;
        self.detail.set_key(("user", id));
        cx.notify();
    }
}

impl Render for UserDetailView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shared_cache_status = match self.users_cache_probe.state(cx) {
            QueryState::Loading => "joining the shared users request".to_string(),
            QueryState::Ready {
                data,
                is_validating,
            } => format!(
                "shared users cache: {} records{}",
                data.len(),
                if is_validating { " (validating)" } else { "" }
            ),
            QueryState::Error { error, .. } => format!("shared users cache error: {error}"),
        };

        let content = match self.detail.state(cx) {
            QueryState::Loading => loading("Loading selected user…"),
            QueryState::Ready {
                data,
                is_validating,
            } => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_lg()
                        .child(data.name.clone())
                        .when(is_validating, |view| view.child(validating_spinner())),
                )
                .child(format!("Role: {}", data.role))
                .child(format!("Email: {}", data.email)),
            QueryState::Error { error, stale_data } => div()
                .flex()
                .flex_col()
                .gap_2()
                .child(error_message(&error))
                .when_some(stale_data, |view, stale| {
                    view.child(format!("Keeping stale detail visible: {}", stale.name))
                }),
        };

        div()
            .flex_1()
            .min_w(px(300.0))
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child(format!("DETAIL KEY: (\"user\", {})", self.selected_id)),
            )
            .child(div().mt_3().child(content))
            .child(
                div()
                    .mt_4()
                    .pt_3()
                    .border_t_1()
                    .border_color(rgb(BORDER))
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child(shared_cache_status),
            )
    }
}

struct UserListView {
    selected_id: u64,
    users: Query<Vec<User>, String>,
    detail: Entity<UserDetailView>,
}

impl UserListView {
    fn new(api: FakeApi, detail: Entity<UserDetailView>, cx: &mut Context<Self>) -> Self {
        let executor = cx.background_executor().clone();
        let users = Query::new(
            ("users",),
            move |(_resource,): (&'static str,)| {
                let api = api.clone();
                let executor = executor.clone();
                async move { api.fetch_users(executor).await }
            },
            cx,
        );
        Self {
            selected_id: 1,
            users,
            detail,
        }
    }

    fn select(&mut self, id: u64, cx: &mut Context<Self>) {
        self.selected_id = id;
        self.detail.update(cx, |detail, cx| detail.select(id, cx));
        cx.notify();
    }

    fn user_rows(&self, users: &[User], cx: &Context<Self>) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .children(users.iter().map(|user| {
                let id = user.id;
                let selected = id == self.selected_id;
                div()
                    .id(("user-row", id))
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(rgb(ACCENT_SOFT)))
                    .hover(|row| row.bg(rgb(ACCENT_SOFT)))
                    .child(user.name.clone())
                    .on_click(cx.listener(move |this, _, _, cx| this.select(id, cx)))
            }))
    }
}

impl Render for UserListView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.users.state(cx) {
            QueryState::Loading => loading("Loading users…"),
            QueryState::Ready {
                data,
                is_validating,
            } => div()
                .flex()
                .flex_col()
                .gap_2()
                .when(is_validating, |view| {
                    view.child(
                        div()
                            .flex()
                            .gap_2()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child(validating_spinner())
                            .child("Refreshing list"),
                    )
                })
                .child(self.user_rows(&data, cx)),
            QueryState::Error { error, stale_data } => div()
                .flex()
                .flex_col()
                .child(error_message(&error))
                .when_some(stale_data, |view, stale| {
                    view.child(self.user_rows(&stale, cx))
                }),
        };

        div()
            .w(px(280.0))
            .flex_none()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child("LIST KEY: (\"users\",)"),
            )
            .child(div().mt_3().child(content))
    }
}

struct TodoPanel {
    api: FakeApi,
    todos: Query<Vec<Todo>, String>,
    input: String,
    input_focus: FocusHandle,
}

impl TodoPanel {
    fn new(api: FakeApi, cx: &mut Context<Self>) -> Self {
        let executor = cx.background_executor().clone();
        let query_api = api.clone();
        let todos = Query::new(
            ("todos",),
            move |(_resource,): (&'static str,)| {
                let api = query_api.clone();
                let executor = executor.clone();
                async move { api.fetch_todos(executor).await }
            },
            cx,
        );
        Self {
            api,
            todos,
            input: String::new(),
            input_focus: cx.focus_handle(),
        }
    }

    fn visible_todos(&self, cx: &App) -> Option<Vec<Todo>> {
        match self.todos.state(cx) {
            QueryState::Loading => None,
            QueryState::Ready { data, .. } => Some(data.as_ref().clone()),
            QueryState::Error { stale_data, .. } => stale_data.map(|data| data.as_ref().clone()),
        }
    }

    fn add_todo(&mut self, cx: &mut Context<Self>) {
        let title = self.input.trim().to_string();
        if title.is_empty() {
            return;
        }
        let Some(mut optimistic) = self.visible_todos(cx) else {
            return;
        };
        let todo = Todo {
            id: self.api.reserve_todo_id(),
            title,
            completed: false,
        };
        optimistic.push(todo.clone());
        self.input.clear();

        let api = self.api.clone();
        let executor = cx.background_executor().clone();
        gpui_query::client(cx)
            .mutate(
                ("todos",),
                Some(optimistic),
                async move { api.add_todo(todo, executor).await },
                cx,
            )
            .detach();
        cx.notify();
    }

    fn toggle_todo(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(mut optimistic) = self.visible_todos(cx) else {
            return;
        };
        let Some(todo) = optimistic.iter_mut().find(|todo| todo.id == id) else {
            return;
        };
        todo.completed = !todo.completed;

        let api = self.api.clone();
        let executor = cx.background_executor().clone();
        gpui_query::client(cx)
            .mutate(
                ("todos",),
                Some(optimistic),
                async move { api.toggle_todo(id, executor).await },
                cx,
            )
            .detach();
    }

    fn on_input_key_down(
        &mut self,
        event: &gpui::KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "backspace" => {
                self.input.pop();
            }
            "enter" => self.add_todo(cx),
            _ => {
                if let Some(text) = &event.keystroke.key_char {
                    self.input.push_str(text);
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }
}

impl Render for TodoPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.todos.state(cx) {
            QueryState::Loading => loading("Loading todos…"),
            QueryState::Ready {
                data,
                is_validating,
            } => self.todo_rows(&data, is_validating, cx),
            QueryState::Error { error, stale_data } => div()
                .flex()
                .flex_col()
                .child(error_message(&error))
                .when_some(stale_data, |view, stale| {
                    view.child(self.todo_rows(&stale, false, cx))
                }),
        };
        let input_is_focused = self.input_focus.is_focused(window);
        let input_text = if self.input.is_empty() {
            "Type a todo, then press Enter…".to_string()
        } else {
            self.input.clone()
        };

        div()
            .p_5()
            .rounded_lg()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(div().text_lg().child("2 · Optimistic todo updates"))
            .child(
                div()
                    .mt_1()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child("Add or toggle immediately. Every fifth fake mutation fails and rolls back; success populates the cache and invalidates it."),
            )
            .child(
                div()
                    .mt_4()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .id("todo-input")
                            .track_focus(&self.input_focus)
                            .on_key_down(cx.listener(Self::on_input_key_down))
                            .on_click({
                                let focus = self.input_focus.clone();
                                move |_, window, _| focus.focus(window)
                            })
                            .flex_1()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(if input_is_focused { ACCENT } else { BORDER }))
                            .text_color(rgb(if self.input.is_empty() { MUTED } else { TEXT }))
                            .cursor_text()
                            .child(input_text),
                    )
                    .child(button(
                        "Add",
                        cx.listener(|this, _, _, cx| this.add_todo(cx)),
                    )),
            )
            .child(div().mt_4().child(content))
    }
}

impl TodoPanel {
    fn todo_rows(&self, todos: &[Todo], is_validating: bool, cx: &Context<Self>) -> gpui::Div {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .when(is_validating, |view| {
                view.child(
                    div()
                        .flex()
                        .gap_2()
                        .text_sm()
                        .text_color(rgb(MUTED))
                        .child(validating_spinner())
                        .child("Settling mutation / revalidating"),
                )
            })
            .children(todos.iter().map(|todo| {
                let id = todo.id;
                div()
                    .id(("todo-row", id))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(BORDER))
                    .cursor_pointer()
                    .hover(|row| row.bg(rgb(BACKGROUND)))
                    .child(if todo.completed { "☑" } else { "☐" })
                    .child(todo.title.clone())
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_todo(id, cx)))
            }))
    }
}

struct SlowPanel {
    slow: Query<SlowReport, String>,
}

impl SlowPanel {
    fn new(api: FakeApi, cx: &mut Context<Self>) -> Self {
        let executor = cx.background_executor().clone();
        let slow = Query::new(
            ("slow",),
            move |(_resource,): (&'static str,)| {
                let api = api.clone();
                let executor = executor.clone();
                async move { api.fetch_slow_report(executor).await }
            },
            cx,
        )
        .stale_time(Duration::from_secs(5));
        Self { slow }
    }
}

impl Render for SlowPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.slow.state(cx) {
            QueryState::Loading => loading("Waiting 3 seconds for the first response…"),
            QueryState::Ready {
                data,
                is_validating,
            } => div()
                .flex()
                .items_center()
                .gap_3()
                .py_3()
                .child(
                    div()
                        .flex_1()
                        .child(format!("{} (revision {})", data.summary, data.revision)),
                )
                .when(is_validating, |view| {
                    view.child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .text_color(rgb(ACCENT))
                            .child(validating_spinner())
                            .child("stale data stays visible while validating"),
                    )
                }),
            QueryState::Error { error, stale_data } => div()
                .flex()
                .flex_col()
                .child(error_message(&error))
                .when_some(stale_data, |view, stale| {
                    view.child(format!("Stale fallback: {}", stale.summary))
                }),
        };

        div()
            .p_5()
            .rounded_lg()
            .border_1()
            .border_color(rgb(BORDER))
            .bg(rgb(SURFACE))
            .child(
                div()
                    .text_lg()
                    .child("3 · Slow endpoint and stale-while-revalidate"),
            )
            .child(div().mt_1().text_sm().text_color(rgb(MUTED)).child(
                "The fake endpoint waits 3s on GPUI's background executor; stale_time is 5s.",
            ))
            .child(content)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(button(
                        "Refetch",
                        cx.listener(|this, _, _, _| this.slow.refetch()),
                    ))
                    .child(button("Invalidate", |_, _, cx| {
                        gpui_query::client(cx).invalidate(("slow",));
                    })),
            )
    }
}

struct DemoApp {
    user_list: Entity<UserListView>,
    user_detail: Entity<UserDetailView>,
    todos: Entity<TodoPanel>,
    slow: Entity<SlowPanel>,
}

impl DemoApp {
    fn new(cx: &mut Context<Self>) -> Self {
        let api = FakeApi::new();
        let user_detail = cx.new(|cx| UserDetailView::new(api.clone(), cx));
        let user_list = cx.new(|cx| UserListView::new(api.clone(), user_detail.clone(), cx));
        let todos = cx.new(|cx| TodoPanel::new(api.clone(), cx));
        let slow = cx.new(|cx| SlowPanel::new(api, cx));
        Self {
            user_list,
            user_detail,
            todos,
            slow,
        }
    }
}

impl Render for DemoApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .id("demo-scroll")
            .overflow_y_scroll()
            .bg(rgb(BACKGROUND))
            .text_color(rgb(TEXT))
            .child(
                div()
                    .w_full()
                    .max_w(px(1100.0))
                    .mx_auto()
                    .p_6()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(
                        div()
                            .child(div().text_2xl().child("gpui-query · Wave 1 demo"))
                            .child(
                                div()
                                    .mt_1()
                                    .text_color(rgb(MUTED))
                                    .child("Three independent panels backed by one in-memory fake API and one shared QueryClient."),
                            ),
                    )
                    .child(
                        div()
                            .p_5()
                            .rounded_lg()
                            .border_1()
                            .border_color(rgb(BORDER))
                            .bg(rgb(SURFACE))
                            .child(div().text_lg().child("1 · Master-detail and shared cache"))
                            .child(
                                div()
                                    .mt_1()
                                    .text_sm()
                                    .text_color(rgb(MUTED))
                                    .child("The two child Entities both subscribe to (\"users\",); detail switches (\"user\", id) with keep_previous_data."),
                            )
                            .child(
                                div()
                                    .mt_4()
                                    .flex()
                                    .flex_wrap()
                                    .gap_4()
                                    .child(self.user_list.clone())
                                    .child(self.user_detail.clone()),
                            ),
                    )
                    .child(self.todos.clone())
                    .child(self.slow.clone()),
            )
    }
}

fn main() {
    Application::new()
        .with_assets(DemoAssets)
        .run(|cx: &mut App| {
            gpui_query::init(cx);
            let bounds = Bounds::centered(None, size(px(1180.0), px(900.0)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |window, cx| {
                    gpui_query::attach_window(window, cx);
                    cx.new(DemoApp::new)
                },
            )
            .expect("failed to open demo window");
            cx.activate(true);
        });
}
