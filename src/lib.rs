//! Parallel Event Emitter
//!
//! Implementation of an event emitter that invokes event listener callbacks concurrently in a configurable thread pool,
//! using `Future`s to notify callers of success or errors.
//!
//! Because all values must be transferred across thread boundaries, all types `T` must be `Send`.
//!
//! Additionally, all types `T` must be `Any`, so `T: 'static`.
//!
//! ### Usage
//!
//! ```toml
//! [dependencies]
//! futures = "0.1"
//! parallel-event-emitter = "0.1.0"
//! ```
//!
//! ```rust
//! extern crate futures;
//! extern crate parallel_event_emitter;
//!
//! use futures::Future;
//! use parallel_event_emitter::*;
//!
//! fn main() {
//!     let mut emitter = ParallelEventEmitter::new();
//!
//!     emitter.add_listener("some event", || {
//!         println!("Hello, World!");
//!
//!         Ok(())
//!     }).unwrap();
//!
//!     assert_eq!(1, emitter.emit("some event").wait().unwrap());
//! }
//! ```
//!
//! ### `Trace<E>` type
//!
//! This crate depends on the [`trace-error`](https://crates.io/crates/trace-error) crate to have simple and lightweight backtraces on all error `Result`s.
//!
//! If you choose not to use that, which is fine by me, simply call `.into_error()` on all `Trace<E>` values to get the real error.
//!
//! ### `impl Trait` feature
//!
//! Instead of having all the `emit*` methods returning a boxed `Future` (`BoxFuture`),
//! the Cargo feature **`conservative_impl_trait`** can be given to enable `impl Future` return types on
//! all the `emit*` methods.
//!
//! ```toml
//! [dependencies.parallel-event-emitter]
//! version = "0.1.0"
//! features = ["conservative_impl_trait"]
//! ```

#![cfg_attr(feature = "conservative_impl_trait", feature(conservative_impl_trait))]
#![deny(missing_docs)]
#![allow(unknown_lints)]

extern crate fnv;

#[macro_use]
extern crate trace_error;
extern crate futures;
extern crate futures_cpupool;

use std::any::Any;
use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicUsize, Ordering};

use fnv::FnvHashMap;
use std::collections::hash_map::Entry;

use futures::{future, Future};
use futures_cpupool::CpuPool;

#[cfg(not(feature = "conservative_impl_trait"))]
use futures::BoxFuture;

use trace_error::Trace;

pub mod error;

pub use self::error::*;

// This allows callbacks to take both values and references depending on the situation
enum ArcCowish {
    Owned(Box<Any>),
    Borrowed(Arc<Box<Any>>),
}

/// This handler callback takes the listener id and the argument,
/// and returns `Ok(true)` if the listener callback was invoked correctly.
type SyncCallback = Box<FnMut(u64, Option<ArcCowish>) -> EventResult<bool>>;

// Stores the listener callback and its ID value
//
// The odd order of locks and `Arc`s is due to needing to access the `id` field without locking
struct SyncEventListener {
    id: u64,
    cb: RwLock<SyncCallback>,
}

unsafe impl Send for SyncEventListener {}

unsafe impl Sync for SyncEventListener {}

impl SyncEventListener {
    /// Create a new `SyncEventListener` from an id and callback
    fn new(id: u64, cb: SyncCallback) -> Arc<SyncEventListener> {
        Arc::new(SyncEventListener { id: id, cb: RwLock::new(cb) })
    }
}

type SyncEventListenerLock = Arc<SyncEventListener>;

type SyncListenersLock = Arc<RwLock<Vec<SyncEventListenerLock>>>;

/// Pooled Event Emitter
///
/// Listeners added to the emitter will be invoked in a thread pool concurrently.
pub struct ParallelEventEmitter {
    inner: Arc<Inner>,
}

/// Inner structure that can be referenced from withing the threadpool and so forth
struct Inner {
    events: RwLock<FnvHashMap<String, SyncListenersLock>>,
    counter: AtomicUsize,
    pool: CpuPool,
}

unsafe impl Send for Inner {}

unsafe impl Sync for Inner {}

impl Default for ParallelEventEmitter {
    fn default() -> ParallelEventEmitter {
        ParallelEventEmitter::new()
    }
}

/// Helper function to map listener results with
#[allow(inline_always)]
#[inline(always)]
fn ran(_: ()) -> bool {
    true
}

impl ParallelEventEmitter {
    /// Creates a new `ParallelEventEmitter` with the default `CpuPool`
    pub fn new() -> ParallelEventEmitter {
        ParallelEventEmitter::with_pool(CpuPool::new_num_cpus())
    }

    /// Creates a new `ParallelEventEmitter` with an already existing `CpuPool` instance.
    ///
    /// This allows for custom thread preferences and lifecycle hooks.
    pub fn with_pool(pool: CpuPool) -> ParallelEventEmitter {
        ParallelEventEmitter {
            inner: Arc::new(Inner {
                events: RwLock::new(FnvHashMap::default()),
                counter: AtomicUsize::new(0),
                pool: pool,
            })
        }
    }

    fn add_listener_impl(&mut self, event: String, cb: SyncCallback) -> EventResult<u64> {
        match try_throw!(self.inner.events.write()).entry(event) {
            Entry::Occupied(listeners_lock) => {
                let mut listeners = try_throw!(listeners_lock.get().write());

                let id = self.inner.counter.fetch_add(1, Ordering::Relaxed) as u64;

                listeners.push(SyncEventListener::new(id, cb));

                Ok(id)
            },
            Entry::Vacant(vacant) => {
                let mut listeners = Vec::with_capacity(1);

                let id = self.inner.counter.fetch_add(1, Ordering::Relaxed) as u64;

                listeners.push(SyncEventListener::new(id, cb));

                vacant.insert(Arc::new(RwLock::new(listeners)));

                Ok(id)
            }
        }
    }

    /// Add a simple listener callback that does not accept any arguments
    ///
    /// The return value of this is a unique ID for that listener, which can later be used to remove it if desired.
    #[inline]
    pub fn add_listener<F, E: Into<String>>(&mut self, event: E, cb: F) -> EventResult<u64> where F: Fn() -> EventResult<()> + 'static {
        self.add_listener_impl(event.into(), Box::new(move |_, _| -> EventResult<bool> { cb().map(ran) }))
    }

    /// Like `add_listener`, but the listener will be removed from the event emitter after a single invocation.
    pub fn once<F, E: Into<String>>(&mut self, event: E, cb: F) -> EventResult<u64> where F: Fn() -> EventResult<()> + 'static {
        let event = event.into();

        // A weak reference is used so that the self-reference from with the listener table doesn't create a circular reference
        let inner_weak = Arc::downgrade(&self.inner);

        self.add_listener_impl(event.clone(), Box::new(move |id, _| -> EventResult<bool> {
            let inner = inner_weak.upgrade().expect("Listener invoked after owning ParallelEventEmitter was dropped");

            // Perform the removal before the callback is invoked, so in case it panics or takes a long time to complete it will have already been removed.
            {
                match try_throw!(inner.events.write()).entry(event.clone()) {
                    Entry::Occupied(listeners_lock) => {
                        let mut listeners = try_throw!(listeners_lock.get().write());

                        if let Ok(index) = listeners.binary_search_by_key(&id, |listener| listener.id) {
                            listeners.remove(index);
                        } else {
                            // If the listener has already been removed in the short time between emitting and this,
                            // just forget we were here and return ok.
                            return Ok(false);
                        }
                    }
                    Entry::Vacant(_) => {
                        // If the listener has already been removed in the short time between emitting and this,
                        // just forget we were here and return ok.
                        return Ok(false);
                    }
                }
            }

            cb().map(ran)
        }))
    }

    /// Add a listener that can accept a value passed via `emit_value`, or `emit_value_sync` if `T` is `Clone`
    ///
    /// If no value or an incompatible value was passed to `emit*`, `None` is passed.
    ///
    /// The return value of this is a unique ID for that listener, which can later be used to remove it if desired.
    pub fn add_listener_value<T, F, E: Into<String>>(&mut self, event: E, cb: F) -> EventResult<u64> where T: Any + Clone + Send, F: Fn(Option<T>) -> EventResult<()> + 'static {
        self.add_listener_impl(event.into(), Box::new(move |_, arg: Option<ArcCowish>| -> EventResult<bool> {
            if let Some(arg) = arg {
                match arg {
                    ArcCowish::Borrowed(value) => {
                        // If the value is borrowed, but T is Clone, we can clone a unique value
                        if let Some(value) = value.downcast_ref::<T>() {
                            return cb(Some(value.clone())).map(ran);
                        }
                    }
                    ArcCowish::Owned(value) => {
                        // If it's owned, we just dereference it directly.
                        if let Ok(value) = value.downcast::<T>() {
                            return cb(Some(*value)).map(ran);
                        }
                    }
                }
            }

            cb(None).map(ran)
        }))
    }

    /// Like `add_listener_value`, but the listener will be removed from the event emitter after a single invocation.
    pub fn once_value<T, F, E: Into<String>>(&mut self, event: E, cb: F) -> EventResult<u64> where T: Any + Clone + Send, F: Fn(Option<T>) -> EventResult<()> + 'static {
        let event = event.into();

        // A weak reference is used so that the self-reference from with the listener table doesn't create a circular reference
        let inner_weak = Arc::downgrade(&self.inner);

        self.add_listener_impl(event.clone(), Box::new(move |id, arg| -> EventResult<bool> {
            let inner = inner_weak.upgrade().expect("Listener invoked after owning ParallelEventEmitter was dropped");

            // Perform the removal before the callback is invoked, so in case it panics or takes a long time to complete it will have already been removed.
            {
                match try_throw!(inner.events.write()).entry(event.clone()) {
                    Entry::Occupied(listeners_lock) => {
                        let mut listeners = try_throw!(listeners_lock.get().write());

                        if let Ok(index) = listeners.binary_search_by_key(&id, |listener| listener.id) {
                            listeners.remove(index);
                        } else {
                            // If the listener has already been removed in the short time between emitting and this,
                            // just forget we were here and return ok.
                            return Ok(false);
                        }
                    }
                    Entry::Vacant(_) => {
                        // If the listener has already been removed in the short time between emitting and this,
                        // just forget we were here and return ok.
                        return Ok(false);
                    }
                }
            }

            if let Some(arg) = arg {
                match arg {
                    ArcCowish::Borrowed(value) => {
                        // If the value is borrowed, but T is Clone, we can clone a unique value
                        if let Some(value) = value.downcast_ref::<T>() {
                            return cb(Some(value.clone())).map(ran);
                        }
                    }
                    ArcCowish::Owned(value) => {
                        // If it's owned, we just dereference it directly.
                        if let Ok(value) = value.downcast::<T>() {
                            return cb(Some(*value)).map(ran);
                        }
                    }
                }
            }

            cb(None).map(ran)
        }))
    }

    /// Variation of `add_listener_value` that accepts `Sync` types,
    /// where intermediate copies on `emit*` are unnecessary.
    ///
    /// This will attempt to use a reference to the original
    /// value passed to `emit_value_sync`. If a value of `T` was passed via `emit_value`,
    /// the callback will be invoked with the `Clone`d copy.
    ///
    /// There is nothing statically forcing the use of this instead of `add_listener_value`,
    /// but it is here just in case your type `T` is `Sync` but might not implement `Clone`,
    /// or if you want to avoid cloning values all over the place.
    ///
    /// The return value of this is a unique ID for that listener, which can later be used to remove it if desired.
    pub fn add_listener_sync<T, F, E: Into<String>>(&mut self, event: E, cb: F) -> EventResult<u64> where T: Any + Send + Sync, F: Fn(Option<&T>) -> EventResult<()> + 'static {
        self.add_listener_impl(event.into(), Box::new(move |_, arg: Option<ArcCowish>| -> EventResult<bool> {
            if let Some(arg) = arg {
                match arg {
                    ArcCowish::Borrowed(value) => {
                        // If the value is borrowed, return return a reference to the local copy
                        if let Some(value) = value.downcast_ref::<T>() {
                            return cb(Some(&*value)).map(ran);
                        }
                    }
                    ArcCowish::Owned(value) => {
                        // If it's owned, do the same thing, although it'll reference the original copy
                        if let Some(value) = value.downcast_ref::<T>() {
                            return cb(Some(&*value)).map(ran);
                        }
                    }
                }
            }

            cb(None).map(ran)
        }))
    }

    /// Like `add_listener_sync`, but the listener will be removed from the event emitter after a single invocation.
    pub fn once_sync<T, F, E: Into<String>>(&mut self, event: E, cb: F) -> EventResult<u64> where T: Any + Send + Sync, F: Fn(Option<&T>) -> EventResult<()> + 'static {
        let event = event.into();

        // A weak reference is used so that the self-reference from with the listener table doesn't create a circular reference
        let inner_weak = Arc::downgrade(&self.inner);

        self.add_listener_impl(event.clone(), Box::new(move |id, arg| -> EventResult<bool> {
            let inner = inner_weak.upgrade().expect("Listener invoked after owning ParallelEventEmitter was dropped");

            // Perform the removal before the callback is invoked, so in case it panics or takes a long time to complete it will have already been removed.
            {
                match try_throw!(inner.events.write()).entry(event.clone()) {
                    Entry::Occupied(listeners_lock) => {
                        let mut listeners = try_throw!(listeners_lock.get().write());

                        if let Ok(index) = listeners.binary_search_by_key(&id, |listener| listener.id) {
                            listeners.remove(index);
                        } else {
                            // If the listener has already been removed in the short time between emitting and this,
                            // just forget we were here and return ok.
                            return Ok(false);
                        }
                    }
                    Entry::Vacant(_) => {
                        // If the listener has already been removed in the short time between emitting and this,
                        // just forget we were here and return ok.
                        return Ok(false);
                    }
                }
            }

            if let Some(arg) = arg {
                match arg {
                    ArcCowish::Borrowed(value) => {
                        // If the value is borrowed, return return a reference to the local copy
                        if let Some(value) = value.downcast_ref::<T>() {
                            return cb(Some(&*value)).map(ran);
                        }
                    }
                    ArcCowish::Owned(value) => {
                        // If it's owned, do the same thing, although it'll reference the original copy
                        if let Some(value) = value.downcast_ref::<T>() {
                            return cb(Some(&*value)).map(ran);
                        }
                    }
                }
            }

            cb(None).map(ran)
        }))
    }

    /// Removes a listener with the given ID and associated with the given event.
    ///
    /// If the listener was not found (either doesn't exist or the wrong event given) `Ok(false)` is returned.
    ///
    /// If the listener was removed, `Ok(true)` is returned.
    pub fn remove_listener<E: Into<String>>(&mut self, event: E, id: u64) -> EventResult<bool> {
        if let Some(listeners_lock) = try_throw!(self.inner.events.read()).get(&event.into()) {
            let mut listeners = try_throw!(listeners_lock.write());

            if let Ok(index) = listeners.binary_search_by_key(&id, |listener| listener.id) {
                listeners.remove(index);

                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Exhaustively searches through ALL events for a listener with the given ID.
    ///
    /// `Ok(false)` is returned if it was not found.
    pub fn remove_any_listener(&mut self, id: u64) -> EventResult<bool> {
        for (_, listeners_lock) in try_throw!(self.inner.events.read()).iter() {
            let mut listeners = try_throw!(listeners_lock.write());

            let index = listeners.binary_search_by_key(&id, |listener| listener.id);

            if let Ok(index) = index {
                listeners.remove(index);

                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Emit an event, invoking all the listeners for that event in the thread pool concurrently.
    ///
    /// The `Future` returned by `emit` resolves to the number of listeners invoked,
    /// and any errors should be forwarded up.
    #[cfg(feature = "conservative_impl_trait")]
    pub fn emit<E: Into<String>>(&mut self, event: E) -> impl Future<Item = usize, Error = Trace<EventError>> {
        let event = event.into();
        let inner = self.inner.clone();

        self.inner.pool.spawn_fn(move || {
            if let Some(listeners_lock) = try_throw!(inner.events.read()).get(&event) {
                let listeners = try_throw!(listeners_lock.read());

                // Don't bother if there aren't any listeners to invoke anyway
                if listeners.len() > 0 {
                    let mut listener_futures = Vec::with_capacity(listeners.len());

                    for listener in listeners.iter() {
                        // Clone a local copy of the listener that can be sent to the spawn
                        let listener = listener.clone();

                        let listener_future = inner.pool.spawn_fn(move || -> EventResult<bool> {
                            let mut cb_guard = try_throw!(listener.cb.write());

                            // Force a mutable reference to the callback
                            (&mut *cb_guard)(listener.id, None)
                        });

                        listener_futures.push(listener_future);
                    }

                    return Ok(future::join_all(listener_futures)
                        .map(|executed: Vec<bool>| executed.iter().filter(|ran| **ran).count()).boxed());
                }
            }

            Ok(futures::finished(0).boxed())
        }).flatten()
    }

    /// Emit an event, invoking all the listeners for that event in the thread pool concurrently.
    ///
    /// The `Future` returned by `emit` resolves to the number of listeners invoked,
    /// and any errors should be forwarded up.
    #[cfg(not(feature = "conservative_impl_trait"))]
    pub fn emit<E: Into<String>>(&mut self, event: E) -> BoxFuture<usize, Trace<EventError>> {
        let event = event.into();
        let inner = self.inner.clone();

        self.inner.pool.spawn_fn(move || {
            if let Some(listeners_lock) = try_throw!(inner.events.read()).get(&event) {
                let listeners = try_throw!(listeners_lock.read());

                // Don't bother if there aren't any listeners to invoke anyway
                if listeners.len() > 0 {
                    let mut listener_futures = Vec::with_capacity(listeners.len());

                    for listener in listeners.iter() {
                        // Clone a local copy of the listener that can be sent to the spawn
                        let listener = listener.clone();

                        let listener_future = inner.pool.spawn_fn(move || -> EventResult<bool> {
                            let mut cb_guard = try_throw!(listener.cb.write());

                            // Force a mutable reference to the callback
                            (&mut *cb_guard)(listener.id, None)
                        });

                        listener_futures.push(listener_future);
                    }

                    return Ok(future::join_all(listener_futures)
                        .map(|executed: Vec<bool>| executed.iter().filter(|ran| **ran).count()).boxed());
                }
            }

            Ok(futures::finished(0).boxed())
        }).flatten().boxed()
    }

    /// Emit an event, invoking all the listeners for that event in the thread pool concurrently.
    ///
    /// A copy of the value will be passed to every listener.
    ///
    /// The `Future` returned by `emit_value` resolves to the number of listeners invoked,
    /// and any errors should be forwarded up.
    #[cfg(feature = "conservative_impl_trait")]
    pub fn emit_value<T, E: Into<String>>(&mut self, event: E, value: T) -> impl Future<Item = usize, Error = Trace<EventError>> where T: Any + Clone + Send {
        let event = event.into();
        let inner = self.inner.clone();

        self.inner.pool.spawn_fn(move || {
            if let Some(listeners_lock) = try_throw!(inner.events.read()).get(&event) {
                let listeners = try_throw!(listeners_lock.read());

                // Don't bother if there aren't any listeners to invoke anyway
                if listeners.len() > 0 {
                    let mut listener_futures = Vec::with_capacity(listeners.len());

                    for listener in listeners.iter() {
                        // Clone a local copy of the listener that can be sent to the spawn
                        let listener = listener.clone();

                        // Clone a local copy of value that can be sent to the listener
                        let value = value.clone();

                        let listener_future = inner.pool.spawn_fn(move || -> EventResult<bool> {
                            let mut cb_guard = try_throw!(listener.cb.write());

                            // Force a mutable reference to the callback
                            (&mut *cb_guard)(listener.id, Some(ArcCowish::Owned(Box::new(value))))
                        });

                        listener_futures.push(listener_future);
                    }

                    return Ok(future::join_all(listener_futures)
                        .map(|executed: Vec<bool>| executed.iter().filter(|ran| **ran).count()).boxed());
                }
            }

            Ok(futures::finished(0).boxed())
        }).flatten()
    }

    /// Emit an event, invoking all the listeners for that event in the thread pool concurrently.
    ///
    /// A copy of the value will be passed to every listener.
    ///
    /// The `Future` returned by `emit_value` resolves to the number of listeners invoked,
    /// and any errors should be forwarded up.
    #[cfg(not(feature = "conservative_impl_trait"))]
    pub fn emit_value<T, E: Into<String>>(&mut self, event: E, value: T) -> BoxFuture<usize, Trace<EventError>> where T: Any + Clone + Send {
        let event = event.into();
        let inner = self.inner.clone();

        self.inner.pool.spawn_fn(move || {
            if let Some(listeners_lock) = try_throw!(inner.events.read()).get(&event) {
                let listeners = try_throw!(listeners_lock.read());

                // Don't bother if there aren't any listeners to invoke anyway
                if listeners.len() > 0 {
                    let mut listener_futures = Vec::with_capacity(listeners.len());

                    for listener in listeners.iter() {
                        // Clone a local copy of the listener that can be sent to the spawn
                        let listener = listener.clone();

                        // Clone a local copy of value that can be sent to the listener
                        let value = value.clone();

                        let listener_future = inner.pool.spawn_fn(move || -> EventResult<bool> {
                            let mut cb_guard = try_throw!(listener.cb.write());

                            // Force a mutable reference to the callback
                            (&mut *cb_guard)(listener.id, Some(ArcCowish::Owned(Box::new(value))))
                        });

                        listener_futures.push(listener_future);
                    }

                    return Ok(future::join_all(listener_futures)
                        .map(|executed: Vec<bool>| executed.iter().filter(|ran| **ran).count()).boxed());
                }
            }

            Ok(futures::finished(0).boxed())
        }).flatten().boxed()
    }

    /// Variation of `emit_value` for `Sync` types, where intermediate copies are unnecessary.
    ///
    /// All listeners receive a reference to the same value.
    ///
    /// The `Future` returned by `emit_value_sync` resolves to the number of listeners invoked,
    /// and any errors should be forwarded up.
    #[cfg(feature = "conservative_impl_trait")]
    pub fn emit_value_sync<T, E: Into<String>>(&mut self, event: E, value: T) -> impl Future<Item = usize, Error = Trace<EventError>> where T: Any + Send + Sync {
        let event = event.into();
        let inner = self.inner.clone();

        self.inner.pool.spawn_fn(move || {
            if let Some(listeners_lock) = try_throw!(inner.events.read()).get(&event) {
                let listeners = try_throw!(listeners_lock.read());

                // Don't bother if there aren't any listeners to invoke anyway
                if listeners.len() > 0 {
                    let mut listener_futures = Vec::with_capacity(listeners.len());

                    // We know T is Send, and Box<Any> is really just Box<T>, so it is Send as well
                    #[derive(Clone)]
                    struct SendWrapper {
                        inner: Arc<Box<Any>>
                    }

                    unsafe impl Send for SendWrapper {}

                    // Use let binding to coerce value into Any
                    let wrapper = SendWrapper { inner: Arc::new(Box::new(value)) };

                    for listener in listeners.iter() {
                        // Clone a local copy of the listener that can be sent to the spawn
                        let listener = listener.clone();

                        let wrapper = wrapper.clone();

                        let listener_future = inner.pool.spawn_fn(move || -> EventResult<bool> {
                            let mut cb_guard = try_throw!(listener.cb.write());

                            // Force a mutable reference to the callback
                            (&mut *cb_guard)(listener.id, Some(ArcCowish::Borrowed(wrapper.inner)))
                        });

                        listener_futures.push(listener_future);
                    }

                    return Ok(future::join_all(listener_futures)
                        .map(|executed: Vec<bool>| executed.iter().filter(|ran| **ran).count()).boxed());
                }
            }

            Ok(futures::finished(0).boxed())
        }).flatten()
    }

    /// Variation of `emit_value` for `Sync` types, where intermediate copies are unnecessary.
    ///
    /// All listeners receive a reference to the same value.
    ///
    /// The `Future` returned by `emit_value_sync` resolves to the number of listeners invoked,
    /// and any errors should be forwarded up.
    #[cfg(not(feature = "conservative_impl_trait"))]
    pub fn emit_value_sync<T, E: Into<String>>(&mut self, event: E, value: T) -> BoxFuture<usize, Trace<EventError>> where T: Any + Send + Sync {
        let event = event.into();
        let inner = self.inner.clone();

        self.inner.pool.spawn_fn(move || {
            if let Some(listeners_lock) = try_throw!(inner.events.read()).get(&event) {
                let listeners = try_throw!(listeners_lock.read());

                // Don't bother if there aren't any listeners to invoke anyway
                if listeners.len() > 0 {
                    let mut listener_futures = Vec::with_capacity(listeners.len());

                    // We know T is Send, and Box<Any> is really just Box<T>, so it is Send as well
                    #[derive(Clone)]
                    struct SendWrapper {
                        inner: Arc<Box<Any>>
                    }

                    unsafe impl Send for SendWrapper {}

                    // Use let binding to coerce value into Any
                    let wrapper = SendWrapper { inner: Arc::new(Box::new(value)) };

                    for listener in listeners.iter() {
                        // Clone a local copy of the listener that can be sent to the spawn
                        let listener = listener.clone();

                        let wrapper = wrapper.clone();

                        let listener_future = inner.pool.spawn_fn(move || -> EventResult<bool> {
                            let mut cb_guard = try_throw!(listener.cb.write());

                            // Force a mutable reference to the callback
                            (&mut *cb_guard)(listener.id, Some(ArcCowish::Borrowed(wrapper.inner)))
                        });

                        listener_futures.push(listener_future);
                    }

                    return Ok(future::join_all(listener_futures)
                        .map(|executed: Vec<bool>| executed.iter().filter(|ran| **ran).count()).boxed());
                }
            }

            Ok(futures::finished(0).boxed())
        }).flatten().boxed()
    }
}