Parallel Event Emitter
======================

Implementation of an event emitter that invokes event listener callbacks concurrently in a configurable thread pool,
using `Future`s to notify callers of success or errors.

Because all values must be transferred across thread boundaries, all types `T` must be `Send`.

Additionally, all types `T` must be `Any`, so `T: 'static`.

## Usage

```toml
[dependencies]
futures = "0.1"
parallel-event-emitter = "0.1.1"
```

```rust
extern crate futures;
extern crate parallel_event_emitter;

use futures::Future;
use parallel_event_emitter::*;

fn main() {
    let mut emitter = ParallelEventEmitter::new();

    emitter.add_listener("some event", || {
        println!("Hello, World!");

        Ok(())
    }).unwrap();

    assert_eq!(1, emitter.emit("some event").wait().unwrap());
}
```

## `Trace<E>` type

This crate depends on the [`trace-error`](https://crates.io/crates/trace-error) crate to have simple and lightweight backtraces on all error `Result`s.

If you choose not to use that, which is fine by me, simply call `.into_error()` on all `Trace<E>` values to get the real error.

## `impl Trait` feature

Instead of having all the `emit*` methods returning a boxed `Future` (`BoxFuture`),
the Cargo feature **`conservative_impl_trait`** can be given to enable `impl Future` return types on
all the `emit*` methods.

```toml
[dependencies.parallel-event-emitter]
version = "0.1.1"
features = ["default", "conservative_impl_trait"] # And maybe integer_atomics
```

## Larger `ListenerId`s

Although the `ListenerId` type itself is `u64`,
the atomic counter underneath is restricted to `AtomicUsize` by default.

To enable true 64-bit counters, use the `integer_atomics` feature for the crate

```toml
[dependencies.parallel-event-emitter]
version = "0.1.1"
features = ["default", "integer_atomics"] # And maybe conservative_impl_trait
```