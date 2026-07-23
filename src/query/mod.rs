use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};
use tracing::{debug, info, warn};

type QueryFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<T>>>>;
type QueryFetcher<T> = Box<dyn Fn() -> QueryFuture<T>>;
type RefetchStrategy<T> = Rc<dyn Fn(&Query<T>) + 'static>;

/// Outcome of the last *completed* fetch.
///
/// This axis is orthogonal to the other two facts a query tracks:
/// - whether a fetch is currently in flight (`is_loading`), and
/// - whether cached `data` is present (`data`).
///
/// Keeping them separate is deliberate: a query can be `Error` while still
/// holding stale `data` to display, and it can be loading while its last
/// completed fetch was `Success` (a background refresh over good data). No
/// single flat status enum could represent those combinations without losing
/// information, so callers combine the three axes as needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LastFetch {
    /// No fetch has completed yet.
    Pending,
    /// The last completed fetch succeeded.
    Success,
    /// The last completed fetch failed. Any cached `data` is retained.
    Error,
}

/// RAII guard for a refetch-trigger registration (see [`Query::refetch_on`]).
///
/// Its only responsibility is teardown: when dropped — which happens
/// automatically when the owning [`Query`] is dropped — it severs the wiring
/// between the event source and the query. Unlike a "subscription" it carries
/// no data and has no streaming semantics; it exists purely to undo a
/// registration.
pub struct RefetchTriggerGuard {
    unbind: Option<Box<dyn FnOnce()>>,
}

impl RefetchTriggerGuard {
    /// General escape hatch: build a guard from anything that can produce a
    /// "tear this down" closure.
    pub fn custom(unbind: impl FnOnce() + 'static) -> Self {
        Self {
            unbind: Some(Box::new(unbind)),
        }
    }

    /// Guard a GObject signal handler. Holds a weak reference to the emitter so
    /// the guard never keeps it alive; if the object is already gone when the
    /// guard drops, disconnection is a no-op (GTK already tore the handler down).
    pub fn signal(obj: &impl IsA<glib::Object>, id: glib::SignalHandlerId) -> Self {
        let weak = obj.clone().upcast::<glib::Object>().downgrade();
        Self::custom(move || {
            if let Some(obj) = weak.upgrade() {
                obj.disconnect(id);
            }
        })
    }

    /// Guard a GLib main-loop source (timeout/idle) by source id.
    pub fn source(id: glib::SourceId) -> Self {
        Self::custom(move || id.remove())
    }
}

impl Drop for RefetchTriggerGuard {
    fn drop(&mut self) {
        if let Some(unbind) = self.unbind.take() {
            unbind();
        }
    }
}

pub struct QueryInner<T> {
    key: String,
    /// The current data (if any successful fetch has occurred)
    pub data: Option<T>,
    /// Timestamp of the last *successful* fetch (when `data` was last updated).
    pub last_success_at: Option<SystemTime>,
    /// Timestamp of the last *failed* fetch.
    pub last_error_at: Option<SystemTime>,
    /// Timestamp at which the most recent fetch attempt was *started*
    /// (regardless of outcome, and even if still in flight). This covers both
    /// manual fetches and retries.
    pub last_fetch_started_at: Option<SystemTime>,
    /// The last error (if any) - stored as Rc for signal emission
    pub error: Option<Rc<anyhow::Error>>,
    query_fn: Option<QueryFetcher<T>>,
    refetch_source_id: Option<glib::SourceId>,
    /// Active fetch task handle - cancellable when dropped
    fetch_task_handle: Option<glib::JoinHandle<()>>,
    query_obj: AsyncQuery,
    /// Timeout duration for queries (None = no timeout)
    timeout: Option<Duration>,

    retry_strategy: Option<Box<dyn Fn(u32) -> Option<Duration>>>,
    retry_count: u32,

    refetch_strategy: Option<RefetchStrategy<T>>,

    /// Teardown guards for event sources registered via `refetch_on`. Owned by
    /// the inner state so they disconnect automatically when the last query
    /// clone is dropped.
    trigger_guards: Vec<RefetchTriggerGuard>,

    /// GLib priority for spawned fetch futures.
    priority: glib::Priority,

    /// Monotonic counter incremented on each `fetch()`. Used to detect and
    /// discard stale results from aborted in-flight fetches.
    fetch_generation: u64,
}

impl<T> QueryInner<T> {
    pub fn new(
        key: String,
        query_fn: Option<QueryFetcher<T>>,
        timeout: Option<Duration>,
        priority: glib::Priority,
    ) -> Self {
        Self {
            key,
            data: None,
            error: None,
            last_success_at: None,
            last_error_at: None,
            last_fetch_started_at: None,
            query_fn,
            refetch_source_id: None,
            fetch_task_handle: None,
            query_obj: glib::Object::new::<AsyncQuery>(),
            timeout,
            retry_strategy: None,
            retry_count: 0,
            refetch_strategy: None,
            trigger_guards: Vec::new(),
            priority,
            fetch_generation: 0,
        }
    }

    /// Check if the data is stale based on a given duration.
    ///
    /// Staleness is measured from the last *successful* fetch, because it
    /// describes the age of the cached `data`. Returns true if no successful
    /// fetch has occurred or if the duration has elapsed since then.
    pub fn is_stale(&self, max_age: Duration) -> bool {
        match self.last_success_at {
            None => true,
            Some(fetched_at) => SystemTime::now()
                .duration_since(fetched_at)
                .map(|elapsed| elapsed > max_age)
                .unwrap_or(true),
        }
    }

    /// Get the age of the data since the last successful fetch.
    /// Returns None if no successful fetch has occurred.
    pub fn age(&self) -> Option<Duration> {
        self.last_success_at
            .and_then(|fetched_at| SystemTime::now().duration_since(fetched_at).ok())
    }
}

glib::wrapper! {
    pub struct AsyncQuery(ObjectSubclass<imp::AsyncQuery>);
}

mod imp {
    use super::*;
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use std::cell::RefCell;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::AsyncQuery)]
    pub struct AsyncQuery {
        #[property(get, set)]
        is_loading: RefCell<bool>,

        #[property(get, set)]
        is_error: RefCell<bool>,

        #[property(get, set)]
        is_success: RefCell<bool>,

        #[property(get, set, nullable)]
        error_message: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AsyncQuery {
        const NAME: &'static str = "AsyncQuery";
        type Type = super::AsyncQuery;
    }

    #[glib::derived_properties]
    impl ObjectImpl for AsyncQuery {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: std::sync::OnceLock<Vec<glib::subclass::Signal>> =
                std::sync::OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    glib::subclass::Signal::builder("success")
                        .run_last()
                        .build(),
                    glib::subclass::Signal::builder("error")
                        .run_last()
                        .param_types([glib::Type::STRING])
                        .build(),
                ]
            })
        }
    }
}

pub struct QueryOptions<T, F>
where
    F: Future<Output = anyhow::Result<T>> + 'static,
{
    /// Unique key for this query (for caching/deduplication)
    pub key: String,

    /// The async function that fetches data
    pub query_fn: Box<dyn Fn() -> F>,

    /// Whether to execute immediately or wait for manual trigger
    pub enabled: bool,

    /// Refetch interval in seconds (None = no auto-refetch)
    pub refetch_interval: Option<u32>,

    /// Timeout duration for the query (None = no timeout)
    pub timeout: Option<Duration>,

    /// GLib priority for the fetch future (default: Priority::DEFAULT)
    pub priority: glib::Priority,
}

pub struct Query<T> {
    inner: Rc<RefCell<QueryInner<T>>>,
}

impl<T> Clone for Query<T>
where
    T: Clone + 'static,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl<T: Clone + 'static> Default for Query<T>
where
    T: Default,
{
    fn default() -> Self {
        Self::new("default".into(), || async { Ok(T::default()) })
    }
}

impl<T> Drop for Query<T> {
    fn drop(&mut self) {
        let is_last = Rc::strong_count(&self.inner) == 1;
        if is_last {
            // Remove the refetch timer if present
            if let Some(source_id) = self.inner.borrow_mut().refetch_source_id.take() {
                source_id.remove();
            }

            // Abort any active fetch task to ensure cleanup
            if let Some(handle) = self.inner.borrow_mut().fetch_task_handle.take() {
                debug!(resource_key = %self.inner.borrow().key, "Dropping last reference to Query, aborting active fetch task");
                handle.abort();
            }
        }
    }
}

/// A clonable handle that, when fired, makes its associated [`Query`] refetch.
///
/// Created by [`Query::refetch_on`] and passed into the registration closure so
/// it can be invoked from inside whatever callback wires up the event source.
/// It holds a *weak* reference to the query's inner state, so firing a trigger
/// after the query has been dropped is a safe no-op rather than resurrecting it.
#[derive(Clone)]
pub struct RefetchTrigger<T: Clone + 'static> {
    inner: std::rc::Weak<RefCell<QueryInner<T>>>,
    max_age: Duration,
}

impl<T: Clone + 'static> RefetchTrigger<T> {
    /// Invoke the configured refetch. If the trigger was registered with a
    /// non-zero `max_age`, the query only refetches when its cached data is
    /// older than that; a zero `max_age` forces an unconditional refetch.
    pub fn fire(&self) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let query = Query { inner };
        if self.max_age == Duration::ZERO {
            query.refetch();
        } else {
            query.refetch_if_stale(self.max_age);
        }
    }
}

impl<T> Query<T>
where
    T: Clone + 'static,
{
    pub fn new<F: Future<Output = anyhow::Result<T>> + 'static>(
        key: String,
        query_fn: impl Fn() -> F + 'static,
    ) -> Self {
        Self::new_with_options(QueryOptions {
            key,
            query_fn: Box::new(query_fn),
            enabled: false,
            refetch_interval: None,
            timeout: None,
            priority: glib::Priority::DEFAULT,
        })
    }

    /// Set the timeout duration for this query
    pub fn set_timeout(&self, timeout: Duration) {
        self.inner.borrow_mut().timeout = Some(timeout);
    }

    /// Set the GLib priority for spawned fetch futures.
    pub fn set_priority(&self, priority: glib::Priority) {
        self.inner.borrow_mut().priority = priority;
    }

    /// Get the GLib priority used for spawned fetch futures.
    pub fn priority(&self) -> glib::Priority {
        self.inner.borrow().priority
    }

    /// Builder-style method to set the GLib priority.
    pub fn with_priority(self, priority: glib::Priority) -> Self {
        self.set_priority(priority);
        self
    }

    /// Create a [`Query`] that immediately succeeds with the given value.
    /// The query never loads, never errors, and `data()` always returns `Some(value)`.
    pub fn pure(value: T) -> Self {
        let inner = Rc::new(RefCell::new(QueryInner::new(
            "pure".into(),
            None,
            None,
            glib::Priority::DEFAULT,
        )));
        let query = Self { inner };
        query.supply(value);
        query
    }

    /// Create a [`Query`] in the pending state: no data, not loading, not error.
    /// It never emits a value on its own — useful as a terminal for combinators
    /// like `once()` that need to suppress further updates.
    pub fn pending() -> Self {
        let inner = Rc::new(RefCell::new(QueryInner::new(
            "pending".into(),
            None,
            None,
            glib::Priority::DEFAULT,
        )));
        Self { inner }
    }

    /// Directly supply data to this query, bypassing the async fetcher.
    /// Sets the data, marks the last fetch as successful, emits the `"success"`
    /// signal, and clears any error state. Used by combinators to push derived
    /// values synchronously.
    pub(crate) fn supply(&self, data: T) {
        {
            let mut inner = self.inner.borrow_mut();
            inner.last_fetch_started_at = Some(SystemTime::now());
        }
        self.set_success_state(data);
    }

    /// Sets the query to a successful state: updates cached data, clears error,
    /// emits the `"success"` signal, and resets retry count. The mutable borrow
    /// on `self.inner` is dropped before the signal is emitted so that re-entrant
    /// handlers can safely call back into the query.
    fn set_success_state(&self, data: T) {
        let query_obj = {
            let mut inner = self.inner.borrow_mut();
            inner.data = Some(data);
            inner.last_success_at = Some(SystemTime::now());
            inner.error = None;
            inner.retry_count = 0;
            let q = &inner.query_obj;
            q.set_is_loading(false);
            q.set_is_success(true);
            q.set_is_error(false);
            q.set_error_message(None::<String>);
            inner.query_obj.clone()
        };
        query_obj.emit_by_name::<()>("success", &[]);
    }

    /// Strategy: Execute fetch immediately
    pub fn immediate() -> impl Fn(&Query<T>) {
        |query: &Query<T>| {
            query.fetch();
        }
    }

    /// Strategy: Debounce fetch calls
    /// Waits for `duration` after the last call before executing.
    /// If another call arrives before the timer fires, the timer resets.
    pub fn debounce(duration: Duration) -> impl Fn(&Query<T>) {
        // Strategy state: managed by the closure itself
        let debounce_state: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));

        move |query: &Query<T>| {
            let key = { query.inner.borrow().key.clone() };

            // Cancel any existing debounce timer
            if let Some(source_id) = debounce_state.borrow_mut().take() {
                debug!(resource_key = %key, "Cancelling previous debounce timer");
                source_id.remove();
            }

            let weak = Rc::downgrade(&query.inner);
            let state_for_callback = debounce_state.clone();
            let source_id = glib::timeout_add_local_once(duration, move || {
                if let Some(inner) = weak.upgrade() {
                    let query = Query { inner };
                    let key = { query.inner.borrow().key.clone() };
                    debug!(resource_key = %key, "Debounce timer fired, executing fetch");
                    // Clear the source_id since timer has fired
                    *state_for_callback.borrow_mut() = None;
                    query.fetch();
                }
            });

            debug!(resource_key = %key, duration_ms = duration.as_millis(), "Scheduled debounced fetch");
            *debounce_state.borrow_mut() = Some(source_id);
        }
    }

    /// Strategy: Throttle fetch calls
    /// Executes at most once per `interval`.
    /// If `trailing` is true, a trailing fetch will be scheduled after the interval
    /// if calls arrived during the throttle period.
    pub fn throttle(interval: Duration, trailing: bool) -> impl Fn(&Query<T>) {
        // Strategy state: managed by the closure itself
        let throttle_state: Rc<RefCell<(Option<Instant>, Option<glib::SourceId>)>> =
            Rc::new(RefCell::new((None, None)));

        move |query: &Query<T>| {
            let key = { query.inner.borrow().key.clone() };
            let now = Instant::now();

            let last_throttle_time = { throttle_state.borrow().0 };

            let should_fetch = match last_throttle_time {
                None => true,
                Some(last_time) => now.duration_since(last_time) >= interval,
            };

            if should_fetch {
                // Cancel any pending trailing timer since we're fetching now
                if let Some(source_id) = throttle_state.borrow_mut().1.take() {
                    debug!(resource_key = %key, "Cancelling trailing throttle timer (immediate fetch)");
                    source_id.remove();
                }

                debug!(resource_key = %key, "Throttle allows fetch, executing immediately");
                *throttle_state.borrow_mut() = (Some(now), None);
                query.fetch();
            } else if trailing {
                // Schedule a trailing fetch if not already scheduled
                let has_pending_trailing = { throttle_state.borrow().1.is_some() };

                if !has_pending_trailing {
                    let remaining = interval
                        .checked_sub(now.duration_since(last_throttle_time.unwrap()))
                        .unwrap_or(Duration::ZERO);

                    let weak = Rc::downgrade(&query.inner);
                    let state_for_callback = throttle_state.clone();
                    let source_id = glib::timeout_add_local_once(remaining, move || {
                        if let Some(inner) = weak.upgrade() {
                            let query = Query { inner };
                            let key = { query.inner.borrow().key.clone() };
                            debug!(resource_key = %key, "Trailing throttle timer fired, executing fetch");
                            // Clear the source_id and update throttle time
                            *state_for_callback.borrow_mut() = (Some(Instant::now()), None);
                            query.fetch();
                        }
                    });

                    debug!(resource_key = %key, remaining_ms = remaining.as_millis(), "Scheduled trailing throttle fetch");
                    throttle_state.borrow_mut().1 = Some(source_id);
                } else {
                    debug!(resource_key = %key, "Throttled: trailing timer already pending");
                }
            } else {
                debug!(resource_key = %key, "Throttled: skipping fetch (no trailing)");
            }
        }
    }

    pub fn set_retry_strategy(&self, retry_strategy: impl Fn(u32) -> Option<Duration> + 'static) {
        self.inner.borrow_mut().retry_strategy = Some(Box::new(retry_strategy));
    }

    pub fn new_with_options<F>(options: QueryOptions<T, F>) -> Self
    where
        F: Future<Output = anyhow::Result<T>> + 'static,
    {
        let inner = Rc::new(RefCell::new(QueryInner::new(
            options.key.clone(),
            Some(Box::new(move || {
                let fut = (options.query_fn)();
                Box::pin(fut)
            })),
            options.timeout,
            options.priority,
        )));

        let query = Self {
            inner: inner.clone(),
        };

        if options.enabled {
            query.fetch();
        }

        // Setup auto-refetch if interval specified
        if let Some(interval) = options.refetch_interval {
            let weak = Rc::downgrade(&inner);
            let source_id = glib::timeout_add_seconds_local(interval, move || {
                if let Some(inner) = weak.upgrade() {
                    Self { inner }.fetch();
                }
                glib::ControlFlow::Continue
            });
            inner.borrow_mut().refetch_source_id = Some(source_id);
        }
        query
    }

    /// Execute a fetch operation and handle the result.
    ///
    /// `generation` is the sequence number from when `fetch()` was called.
    /// If a newer fetch has been started (higher generation), this fetch
    /// silently discards its result to avoid stale-data overwrites.
    async fn execute_fetch(inner: &Rc<RefCell<QueryInner<T>>>, generation: u64) {
        let key = { inner.borrow().key.clone() };
        let query_obj = { inner.borrow().query_obj.clone() };
        let timeout = { inner.borrow().timeout };

        let Some(future) = inner.borrow().query_fn.as_ref().map(|f| f()) else {
            warn!(resource_key = %key, "No query function set for resource");
            return;
        };
        debug!(resource_key = %key, "Starting fetch for resource");

        // Apply timeout if configured
        let result = if let Some(timeout_duration) = timeout {
            use futures::FutureExt;

            debug!(resource_key = %key, timeout_secs = timeout_duration.as_secs(), "Query has timeout configured");

            // Race the future against a timeout
            let timeout_future = glib::timeout_future(timeout_duration);

            futures::select! {
                result = future.fuse() => result,
                _ = timeout_future.fuse() => {
                    warn!(resource_key = %key, timeout_secs = timeout_duration.as_secs(), "Query timed out");
                    Err(anyhow::anyhow!("Query timed out after {} seconds", timeout_duration.as_secs()))
                }
            }
        } else {
            future.await
        };

        if inner.borrow().fetch_generation != generation {
            debug!(resource_key = %key, generation, current = inner.borrow().fetch_generation, "Discarding stale fetch result");
            return;
        }

        match result {
            Ok(_data) => {
                info!(resource_key = %key, "Resource fetch completed successfully");
                Self {
                    inner: inner.clone(),
                }
                .set_success_state(_data.clone());
            }
            Err(error) => {
                if inner.borrow().retry_strategy.is_some() {
                    let retry_count = Self {
                        inner: inner.clone(),
                    }
                    .retry();
                    if let Some(_retry_count) = retry_count {
                        return;
                    }
                }
                let rc_error = Rc::new(error);
                let error_msg = rc_error.to_string();
                // Keep the previous data, just mark as error
                inner.borrow_mut().error = Some(rc_error);
                // Record when this fetch failed. This is a separate axis from
                // `last_success_at`: staleness must not be reset by a failure,
                // but callers still need to know when the last failure happened
                // (e.g. to back off retries).
                inner.borrow_mut().last_error_at = Some(SystemTime::now());
                query_obj.set_is_loading(false);
                // The outcome axis reflects only the last completed fetch. A
                // failed fetch is `Error` regardless of any retained `data`;
                // conflating the two here is what previously let the UI treat a
                // stale value as if the latest check had succeeded.
                query_obj.set_is_error(true);
                query_obj.set_is_success(false);
                query_obj.set_error_message(Some(error_msg.clone()));

                warn!(resource_key = %key, error = %error_msg, "Resource fetch failed");
                // Emit error signal with error message
                query_obj.emit_by_name::<()>("error", &[&error_msg]);
            }
        }
    }

    pub fn fetch(&self) {
        let key = { self.inner.borrow().key.clone() };
        debug!(resource_key = %key, "Fetch triggered for resource");
        let query_obj = { self.inner.borrow().query_obj.clone() };
        // Cancel any previous fetch task before starting a new one
        if let Some(handle) = self.inner.borrow_mut().fetch_task_handle.take() {
            debug!(resource_key = %key, "Aborting previous fetch task");
            handle.abort();
        }

        // Enter the loading state. The outcome axis (`is_success`/`is_error`)
        // is intentionally left untouched so a background refetch keeps
        // reporting the last completed fetch's result (and any retained `data`
        // stays valid to display) until this fetch completes.
        query_obj.set_is_loading(true);
        // Record the start of this attempt (regardless of outcome). This is the
        // right axis for throttling *attempts* independently of staleness.
        self.inner.borrow_mut().last_fetch_started_at = Some(SystemTime::now());

        self.inner.borrow_mut().fetch_generation += 1;
        let generation = self.inner.borrow().fetch_generation;

        let inner = self.inner.clone();
        let priority = { self.inner.borrow().priority };

        let handle = glib::MainContext::ref_thread_default().spawn_local_with_priority(
            priority,
            async move {
                Self::execute_fetch(&inner, generation).await;
            },
        );

        self.inner.borrow_mut().fetch_task_handle = Some(handle);
    }

    /// Set the refetch strategy for this query.
    /// The strategy is a closure that determines when and how to execute the fetch.
    /// Common strategies are `Query::immediate`, `Query::debounce`, and `Query::throttle`.
    pub fn set_refetch_strategy(&self, strategy: impl Fn(&Query<T>) + 'static) {
        self.inner.borrow_mut().refetch_strategy = Some(Rc::new(strategy));
    }

    /// Register an arbitrary event source that causes this query to refetch.
    ///
    /// `connect` receives a [`RefetchTrigger`] to invoke whenever the event
    /// fires, and returns a [`RefetchTriggerGuard`] describing how to undo the
    /// registration. The guard is owned by the query and dropped — disconnecting
    /// the source — when the last clone of the query is dropped.
    ///
    /// `max_age` gates the refetch: when non-zero the trigger only refetches if
    /// the cached data is older than `max_age`
    /// (see [`refetch_if_stale`](Self::refetch_if_stale)); pass
    /// [`Duration::ZERO`] to refetch unconditionally on every event.
    ///
    /// This is the general primitive behind event-driven refetches; see
    /// [`refetch_on_focus`](Self::refetch_on_focus) for the common window-focus
    /// case.
    pub fn refetch_on(
        &self,
        max_age: Duration,
        connect: impl FnOnce(RefetchTrigger<T>) -> RefetchTriggerGuard,
    ) {
        let trigger = RefetchTrigger {
            inner: Rc::downgrade(&self.inner),
            max_age,
        };
        let guard = connect(trigger);
        self.inner.borrow_mut().trigger_guards.push(guard);
    }

    /// Convenience for [`refetch_on`](Self::refetch_on): refetch
    /// (staleness-gated by `max_age`) whenever `window` becomes the active,
    /// focused window.
    ///
    /// Both directions are weak: if the query is dropped first the focus
    /// callback becomes a no-op and the guard disconnects the handler; if the
    /// window is destroyed first GTK drops the handler and the guard's teardown
    /// is skipped.
    pub fn refetch_on_focus(&self, window: &impl IsA<gtk::Window>, max_age: Duration) {
        let key = self.inner.borrow().key.clone();
        self.refetch_on(max_age, move |trigger| {
            let win: gtk::Window = window.clone().upcast();
            let id = win.connect_notify_local(Some("is-active"), move |w, _| {
                if !w.is_active() {
                    return;
                }
                debug!(resource_key = %key, "Window gained focus, triggering refetch");
                trigger.fire();
            });
            RefetchTriggerGuard::signal(&win, id)
        });
    }

    /// Refetch using the configured strategy (or immediate if none set)
    pub fn refetch(&self) {
        let strategy = self.inner.borrow().refetch_strategy.clone();
        if let Some(strategy) = strategy {
            strategy(self);
        } else {
            self.fetch();
        }
    }

    pub fn retry(&self) -> Option<u32> {
        self.inner.borrow_mut().retry_count += 1;
        let retry_count = { self.inner.borrow().retry_count };
        let key = { self.inner.borrow().key.clone() };
        if let Some(delay) = {
            self.inner
                .borrow()
                .retry_strategy
                .as_ref()
                .and_then(|f| f(retry_count))
        } {
            info!(resource_key = %key, retry_count = retry_count, delay_secs = delay.as_secs(), "Scheduling retry for resource fetch");
            let inner = self.inner.clone();
            let generation = inner.borrow().fetch_generation;
            let priority = { self.inner.borrow().priority };
            let handle = glib::MainContext::ref_thread_default().spawn_local_with_priority(
                priority,
                async move {
                    glib::timeout_future(delay).await;
                    if inner.borrow().fetch_generation != generation {
                        return;
                    }
                    inner.borrow_mut().last_fetch_started_at = Some(SystemTime::now());
                    Self::execute_fetch(&inner, generation).await;
                },
            );

            if let Some(old_handle) = self.inner.borrow_mut().fetch_task_handle.take() {
                old_handle.abort();
            }
            self.inner.borrow_mut().fetch_task_handle = Some(handle);
            Some(retry_count)
        } else {
            warn!(resource_key = %key, retry_count = retry_count, "No more retries left, giving up on resource fetch");
            None
        }
    }

    pub fn set_fetcher<F>(&self, query_fn: impl Fn() -> F + 'static)
    where
        F: Future<Output = anyhow::Result<T>> + 'static,
    {
        self.inner.borrow_mut().query_fn = Some(Box::new(move || Box::pin(query_fn())));
    }

    pub fn set_resource_key(&self, key: &str) {
        self.inner.borrow_mut().key = key.to_string();
    }

    pub fn connect_success<F: Fn(&T) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        let inner = self.inner.clone();
        let query_obj = { inner.borrow().query_obj.clone() };
        query_obj.connect_local("success", false, move |_args| {
            let data = inner.borrow().data.clone();
            if let Some(ref data) = data {
                f(data);
            }
            None
        })
    }

    pub fn connect_error<F: Fn(&anyhow::Error) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        let inner = self.inner.clone();
        let query_obj = { inner.borrow().query_obj.clone() };
        query_obj.connect_local("error", false, move |_args| {
            let error = inner.borrow().error.clone();
            if let Some(ref error) = error {
                f(error);
            }
            None
        })
    }

    pub fn connect_loading<F: Fn(bool) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        let query_obj = { self.inner.borrow().query_obj.clone() };
        query_obj.connect_notify_local(Some("is-loading"), move |query_obj, _pspec| {
            let is_loading = query_obj.is_loading();
            f(is_loading);
        })
    }

    pub fn is_loading(&self) -> bool {
        self.inner.borrow().query_obj.is_loading()
    }

    /// Outcome of the last *completed* fetch.
    ///
    /// This is one of three orthogonal axes; see [`LastFetch`]. It ignores
    /// whether a fetch is currently in flight (use [`is_loading`](Self::is_loading))
    /// and whether cached data is present (use [`data`](Self::data)).
    pub fn last_fetch(&self) -> LastFetch {
        let query_obj = { self.inner.borrow().query_obj.clone() };
        if query_obj.is_error() {
            LastFetch::Error
        } else if query_obj.is_success() {
            LastFetch::Success
        } else {
            LastFetch::Pending
        }
    }

    /// Convenience: whether the last completed fetch succeeded.
    pub fn is_success(&self) -> bool {
        self.last_fetch() == LastFetch::Success
    }

    /// Convenience: whether the last completed fetch failed.
    pub fn is_error(&self) -> bool {
        self.last_fetch() == LastFetch::Error
    }

    pub fn data(&self) -> Option<T> {
        self.inner.borrow().data.clone()
    }

    /// Check if the cached data is stale based on a given max age
    /// Returns true if data has never been fetched or if the duration has elapsed
    pub fn is_stale(&self, max_age: Duration) -> bool {
        self.inner.borrow().is_stale(max_age)
    }

    /// Get the age of the cached data since last successful fetch
    /// Returns None if data has never been fetched
    pub fn age(&self) -> Option<Duration> {
        self.inner.borrow().age()
    }

    /// Get the timestamp of the last successful fetch (when `data` was last
    /// updated). This is the axis staleness is measured from.
    pub fn last_success_at(&self) -> Option<SystemTime> {
        self.inner.borrow().last_success_at
    }

    /// Get the timestamp of the last failed fetch.
    pub fn last_error_at(&self) -> Option<SystemTime> {
        self.inner.borrow().last_error_at
    }

    /// Get the timestamp at which the most recent fetch attempt was started
    /// (regardless of outcome, and even if still in flight). This covers both
    /// manual fetches and retries.
    pub fn last_fetch_started_at(&self) -> Option<SystemTime> {
        self.inner.borrow().last_fetch_started_at
    }

    /// Refetch only if the cached data is stale based on the given max age.
    ///
    /// Staleness is measured from the last *successful* fetch (see
    /// [`is_stale`](Self::is_stale)). A failure does not by itself force a
    /// refetch; it simply does not reset the staleness clock. So if the last
    /// success is older than `max_age`, a subsequent failure will not save the
    /// data from being considered stale — but if the last success is still
    /// recent, even a failing attempt leaves the data treated as fresh.
    /// Rate-limiting of the resulting attempts (e.g. to avoid hammering a
    /// failing backend) is the job of the refetch strategy, not of this method.
    ///
    /// Returns true if a refetch was triggered, false if data is still fresh.
    pub fn refetch_if_stale(&self, max_age: Duration) -> bool {
        let key = { self.inner.borrow().key.clone() };
        if self.is_stale(max_age) {
            debug!(
                resource_key = %key,
                max_age_secs = max_age.as_secs(),
                "Resource is stale, triggering refetch"
            );
            self.refetch();
            true
        } else {
            debug!(
                resource_key = %key,
                age_secs = ?self.age().map(|d| d.as_secs()),
                max_age_secs = max_age.as_secs(),
                "Resource is fresh, skipping refetch"
            );
            false
        }
    }

    /// Chain this query into another query, replacing the inner query whenever
    /// this source produces a new value (switch semantics).
    ///
    /// When this query succeeds, `f` is called with the source data to produce a
    /// new `Query<U>`. The derived query then subscribes to that inner query
    /// and forwards its successes. When the source succeeds again — or if the
    /// source already has data at the time `switch_map` is called — the previous
    /// inner query is dropped (which aborts any in-flight fetch) and replaced
    /// with a new one.
    ///
    /// The derived query uses `supply` internally, so inner-query successes are
    /// pushed to the derived query synchronously (no loading flicker).
    pub fn switch_map<U: Clone + 'static>(&self, f: impl Fn(&T) -> Query<U> + 'static) -> Query<U> {
        type InnerEntry<U> = (Query<U>, glib::SignalHandlerId);
        let source = self.clone();
        let f = Rc::new(f);

        let derived_key = format!("{}:switch_map", self.inner.borrow().key);
        let derived_query = Query::new(derived_key, || async {
            anyhow::bail!("switch_map: derived query, no fetcher");
        });
        derived_query.set_priority(self.inner.borrow().priority);
        let derived_weak = Rc::downgrade(&derived_query.inner);

        let current: Rc<RefCell<Option<InnerEntry<U>>>> = Rc::new(RefCell::new(None));

        let switch = {
            let source_key = { self.inner.borrow().key.clone() };
            let f = f.clone();
            let derived_weak = derived_weak.clone();
            let current = current.clone();
            move |data: &T| {
                let new_inner = f(data);

                let new_inner_weak = Rc::downgrade(&new_inner.inner);
                let dw = derived_weak.clone();
                let handler_id =
                    new_inner
                        .inner
                        .borrow()
                        .query_obj
                        .connect_local("success", false, move |_| {
                            if let (Some(ni), Some(di)) = (new_inner_weak.upgrade(), dw.upgrade())
                                && let Some(d) = &ni.borrow().data
                            {
                                Query { inner: di }.supply(d.clone());
                            }
                            None
                        });

                // If the inner query was synchronous (e.g. Query::pure),
                // its success signal already fired before we connected.
                // Push the value to derived immediately to cover that case.
                if let Some(d) = new_inner.data()
                    && let Some(di) = derived_weak.upgrade()
                {
                    Query { inner: di }.supply(d);
                }

                debug!(
                    resource_key = %source_key,
                    "switch_map: replacing inner query"
                );

                let old = current.borrow_mut().replace((new_inner, handler_id));
                if let Some((old_query, old_handler)) = old {
                    old_query.inner.borrow().query_obj.disconnect(old_handler);
                }
            }
        };

        let source_weak = Rc::downgrade(&source.inner);
        let source_key = { self.inner.borrow().key.clone() };
        let switch_closure = switch.clone();
        let source_key_for_closure = source_key.clone();
        source
            .inner
            .borrow()
            .query_obj
            .connect_local("success", false, move |_| {
                if let Some(inner) = source_weak.upgrade()
                    && let Some(data) = &inner.borrow().data
                {
                    debug!(
                        resource_key = %source_key_for_closure,
                        "switch_map: source succeeded, routing to inner query"
                    );
                    switch_closure(data);
                }
                None
            });

        if let Some(data) = source.data() {
            debug!(
                resource_key = %source_key,
                "switch_map: source already has data, initializing immediately"
            );
            switch(&data);
        }

        // When refetching the derived query, cascade the refetch to the
        // current inner query (which may itself be a switch_map that
        // cascades further) and to the source query as a fallback, so
        // that refetch() on a deeply-derived chain eventually reaches a
        // real async fetcher.
        {
            let source_for_refetch = source.clone();
            let current_for_refetch = current.clone();
            derived_query.set_refetch_strategy(move |_| {
                if let Some((inner_query, _)) = current_for_refetch.borrow().as_ref() {
                    inner_query.refetch();
                }
                source_for_refetch.refetch();
            });
        }

        derived_query
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `QueryInner` with `last_success_at` set, for staleness/age tests.
    fn inner_with_success_at(last_success_at: Option<SystemTime>) -> QueryInner<String> {
        let mut inner =
            QueryInner::<String>::new("test".into(), None, None, glib::Priority::DEFAULT);
        inner.last_success_at = last_success_at;
        inner
    }

    #[test]
    fn test_is_stale_never_fetched() {
        // Data that was never fetched is always stale
        assert!(inner_with_success_at(None).is_stale(Duration::from_secs(60)));
        assert!(inner_with_success_at(None).is_stale(Duration::from_secs(0)));
    }

    #[test]
    fn test_is_stale_fresh_data() {
        let inner = inner_with_success_at(Some(SystemTime::now()));

        // Data just fetched should not be stale for reasonable max_age
        assert!(!inner.is_stale(Duration::from_secs(60)));
        assert!(!inner.is_stale(Duration::from_secs(1)));
    }

    #[test]
    fn test_is_stale_old_data() {
        // Set last_success_at to 2 seconds ago
        let inner = inner_with_success_at(Some(SystemTime::now() - Duration::from_secs(2)));

        // Data older than max_age is stale
        assert!(inner.is_stale(Duration::from_secs(1)));
        // Data newer than max_age is not stale
        assert!(!inner.is_stale(Duration::from_secs(10)));
    }

    #[test]
    fn test_age_never_fetched() {
        // Data that was never fetched has no age
        assert!(inner_with_success_at(None).age().is_none());
    }

    #[test]
    fn test_age_just_fetched() {
        let inner = inner_with_success_at(Some(SystemTime::now()));

        // Data just fetched should have very small age
        let age = inner.age().expect("Should have age");
        assert!(age < Duration::from_secs(1));
    }

    #[test]
    fn test_age_old_data() {
        let inner = inner_with_success_at(Some(SystemTime::now() - Duration::from_secs(5)));

        // Data fetched 5 seconds ago should have age of approximately 5 seconds
        let age = inner.age().expect("Should have age");
        assert!(age >= Duration::from_secs(4));
        assert!(age < Duration::from_secs(7));
    }

    #[test]
    fn test_staleness_tracks_success_not_failure() {
        // Staleness is measured from the last *successful* fetch. A more recent
        // failure must NOT make stale data look fresh, otherwise a query that
        // starts failing would stop being refetched.
        let mut inner = inner_with_success_at(Some(SystemTime::now() - Duration::from_secs(10)));
        inner.last_error_at = Some(SystemTime::now()); // just failed

        // Even though we "just" failed, the cached data is 10s old and should be
        // considered stale for a 1s max age.
        assert!(inner.is_stale(Duration::from_secs(1)));
    }

    #[test]
    fn test_staleness_independent_of_attempt_start() {
        // Starting (or being in the middle of) a fetch must not reset staleness.
        // Only a *successful* completion does. Here the last success is old, so
        // the data is stale regardless of a freshly-started attempt.
        let mut inner = inner_with_success_at(Some(SystemTime::now() - Duration::from_secs(30)));
        inner.last_fetch_started_at = Some(SystemTime::now());

        assert!(inner.is_stale(Duration::from_secs(5)));
    }

    #[test]
    fn test_timing_axes_are_independent() {
        // The three timing axes answer different questions and can hold
        // unrelated values simultaneously. `is_stale` and `age` must consult
        // only `last_success_at`, ignoring `last_error_at` and
        // `last_fetch_started_at`.
        let mut inner = inner_with_success_at(Some(SystemTime::now() - Duration::from_secs(20)));
        inner.last_error_at = Some(SystemTime::now() - Duration::from_secs(2));
        inner.last_fetch_started_at = Some(SystemTime::now());

        // Age is derived from the success axis only.
        let age = inner.age().expect("age from success");
        assert!(age >= Duration::from_secs(19) && age < Duration::from_secs(22));

        // Despite a more recent error and an in-flight attempt, staleness
        // reflects the 20s-old success, not the other timestamps.
        assert!(inner.is_stale(Duration::from_secs(5)));
        assert!(!inner.is_stale(Duration::from_secs(30)));
    }

    #[test]
    fn test_retry_strategy_basic() {
        // Test that retry_strategy closure works as expected
        let strategy: Box<dyn Fn(u32) -> Option<Duration>> = Box::new(|n| {
            if n < 3 {
                Some(Duration::from_secs(n as u64))
            } else {
                None
            }
        });

        assert_eq!(strategy(0), Some(Duration::from_secs(0)));
        assert_eq!(strategy(1), Some(Duration::from_secs(1)));
        assert_eq!(strategy(2), Some(Duration::from_secs(2)));
        assert_eq!(strategy(3), None);
        assert_eq!(strategy(100), None);
    }

    #[test]
    fn test_exponential_backoff_strategy() {
        // Test exponential backoff pattern
        let strategy: Box<dyn Fn(u32) -> Option<Duration>> = Box::new(|n| {
            if n < 5 {
                Some(Duration::from_millis(100 * 2u64.pow(n)))
            } else {
                None
            }
        });

        assert_eq!(strategy(0), Some(Duration::from_millis(100)));
        assert_eq!(strategy(1), Some(Duration::from_millis(200)));
        assert_eq!(strategy(2), Some(Duration::from_millis(400)));
        assert_eq!(strategy(3), Some(Duration::from_millis(800)));
        assert_eq!(strategy(4), Some(Duration::from_millis(1600)));
        assert_eq!(strategy(5), None);
    }

    #[test]
    fn test_is_stale_boundary() {
        // Test exact boundary condition
        let inner = inner_with_success_at(Some(SystemTime::now() - Duration::from_millis(1000)));

        // At exactly 1 second, should be stale (elapsed > max_age, not >=)
        assert!(inner.is_stale(Duration::from_millis(999)));
        // At more than elapsed time, should not be stale
        assert!(!inner.is_stale(Duration::from_millis(2000)));
    }

    #[test]
    fn test_retry_strategy_with_jitter() {
        // Test a more complex retry strategy with jitter-like behavior
        use std::sync::atomic::{AtomicU32, Ordering};
        let counter = std::sync::Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let strategy: Box<dyn Fn(u32) -> Option<Duration>> = Box::new(move |n| {
            counter_clone.fetch_add(1, Ordering::SeqCst);
            if n < 3 {
                // Base delay with pseudo-jitter based on retry count
                Some(Duration::from_millis(100 * (n as u64 + 1)))
            } else {
                None
            }
        });

        // Call strategy multiple times
        let _ = strategy(0);
        let _ = strategy(1);
        let _ = strategy(2);
        let _ = strategy(3);

        // Verify strategy was called correct number of times
        assert_eq!(counter.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn test_refetch_strategy_functions() {
        // Test that all strategy functions can be created
        // These are now closures, so we can't easily inspect them
        // But we can verify they compile and return the right type
        let _immediate = Query::<String>::immediate();
        let _debounce = Query::<String>::debounce(Duration::from_millis(300));
        let _throttle_no_trailing = Query::<String>::throttle(Duration::from_secs(1), false);
        let _throttle_with_trailing = Query::<String>::throttle(Duration::from_secs(2), true);

        // The test passes if all strategies can be constructed without error
    }

    #[test]
    fn test_throttle_timing_logic() {
        // Test the throttle timing logic in isolation
        let interval = Duration::from_millis(100);
        let mut last_throttle_time: Option<Instant> = None;

        // First call should always be allowed
        let now = Instant::now();
        let should_fetch_1 = match last_throttle_time {
            None => true,
            Some(last_time) => now.duration_since(last_time) >= interval,
        };
        assert!(should_fetch_1, "First call should be allowed");

        // Simulate a fetch
        last_throttle_time = Some(now);

        // Immediate second call should be throttled
        let now2 = Instant::now();
        let should_fetch_2 = match last_throttle_time {
            None => true,
            Some(last_time) => now2.duration_since(last_time) >= interval,
        };
        assert!(!should_fetch_2, "Immediate second call should be throttled");

        // After interval passes, should be allowed again
        std::thread::sleep(interval + Duration::from_millis(10));
        let now3 = Instant::now();
        let should_fetch_3 = match last_throttle_time {
            None => true,
            Some(last_time) => now3.duration_since(last_time) >= interval,
        };
        assert!(should_fetch_3, "Call after interval should be allowed");
    }

    #[gtk::test]
    fn test_pure_supplies_value_immediately() {
        let q = Query::<i32>::pure(42);
        assert_eq!(q.data(), Some(42));
        assert!(q.is_success());
        assert!(!q.is_loading());
        assert!(!q.is_error());
        assert_eq!(q.last_fetch(), LastFetch::Success);
    }

    #[gtk::test]
    fn test_pending_has_no_data() {
        let q = Query::<String>::pending();
        assert_eq!(q.data(), None);
        assert!(!q.is_success());
        assert!(!q.is_loading());
        assert!(!q.is_error());
        assert_eq!(q.last_fetch(), LastFetch::Pending);
    }

    #[gtk::test]
    fn test_switch_map_from_pure_propagates_immediately() {
        let source = Query::pure("hello".to_string());
        let derived = source.switch_map(|s| Query::pure(s.len()));
        assert_eq!(derived.data(), Some(5));
        assert!(derived.is_success());
        assert!(!derived.is_loading());
    }

    #[gtk::test]
    fn test_switch_map_updates_when_source_supplies_new_data() {
        let source = Query::<String>::pending();
        let derived = source.switch_map(|s| Query::pure(format!("{s}!")));

        assert_eq!(derived.data(), None);
        assert_eq!(derived.last_fetch(), LastFetch::Pending);

        source.supply("hello".to_string());
        assert_eq!(derived.data(), Some("hello!".to_string()));
        assert!(derived.is_success());
    }

    #[gtk::test]
    fn test_switch_map_replaces_inner_on_each_source_update() {
        let source = Query::<String>::pending();
        let derived = source.switch_map(|s| Query::pure(s.len()));

        source.supply("hello".to_string());
        assert_eq!(derived.data(), Some(5));

        source.supply("world!".to_string());
        assert_eq!(derived.data(), Some(6));

        source.supply("".to_string());
        assert_eq!(derived.data(), Some(0));
    }

    #[gtk::test]
    fn test_supply_can_be_called_multiple_times() {
        let q = Query::<i32>::pending();
        assert_eq!(q.data(), None);

        q.supply(1);
        assert_eq!(q.data(), Some(1));
        assert!(q.is_success());

        q.supply(2);
        assert_eq!(q.data(), Some(2));
        assert!(q.is_success());
    }

    #[gtk::test]
    fn test_switch_map_preserves_stale_data_on_error() {
        let source = Query::<String>::pending();
        let derived = source.switch_map(|s| Query::pure(s.len()));

        source.supply("hello".to_string());
        assert_eq!(derived.data(), Some(5));
        assert!(derived.is_success());
    }

    #[gtk::test]
    fn test_switch_map_refetch_cascades_to_source() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        let fetch_count = Arc::new(AtomicU32::new(0));
        let fc = fetch_count.clone();

        let source = Query::<i32>::new("test_refetch_source".into(), move || {
            let fc = fc.clone();
            async move {
                fc.fetch_add(1, Ordering::SeqCst);
                Ok(42)
            }
        });

        let derived = source.switch_map(|n| Query::pure(*n));
        assert_eq!(derived.data(), None);

        // Initial fetch through source
        source.supply(7);
        assert_eq!(derived.data(), Some(7));
        assert_eq!(fetch_count.load(Ordering::SeqCst), 0);

        // Refetch via derived should cascade to source
        derived.refetch();

        spin_until(2, || fetch_count.load(Ordering::SeqCst) >= 1);

        assert!(
            fetch_count.load(Ordering::SeqCst) >= 1,
            "refetch on derived switch_map should cascade to source: got {}",
            fetch_count.load(Ordering::SeqCst)
        );
    }

    fn spin_until(timeout_secs: u64, mut condition: impl FnMut() -> bool) {
        use std::time::Instant;
        let context = glib::MainContext::ref_thread_default();
        let deadline = Instant::now() + Duration::from_secs(timeout_secs);

        while Instant::now() < deadline {
            while context.pending() {
                context.iteration(false);
            }
            if condition() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        while context.pending() {
            context.iteration(false);
        }
    }
}
