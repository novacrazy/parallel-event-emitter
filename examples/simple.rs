#![feature(box_syntax)]

#[macro_use]
extern crate trace_error;
extern crate parallel_event_emitter;
extern crate thread_id;
extern crate futures;

use futures::Future;
use parallel_event_emitter::*;

use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Default)]
struct MyType {
    value: AtomicUsize,
}

fn main() {
    let mut emitter = ParallelEventEmitter::new();

    let some_value = 1337;

    emitter.add_listener("test", move || {
        println!("Thread: {:<6} No arguments {}", thread_id::get(), some_value);

        Ok(())
    }).unwrap();

    emitter.add_listener_value("test", |arg: Option<i32>| {
        println!("Thread: {:<6} Expected i32: {:?}", thread_id::get(), arg);

        Ok(())
    }).unwrap();

    emitter.add_listener_value("test", |arg: Option<&str>| {
        println!("Thread: {:<6} Expected &str: {:?}", thread_id::get(), arg);

        Ok(())
    }).unwrap();

    for i in 0..4 {
        emitter.add_listener_sync("some_sync", move |arg: Option<&MyType>| {
            println!("Thread: {:<6} Expected sync {}: {:?}", thread_id::get(), i, arg.map(|arg| {
                arg.value.fetch_add(1, Ordering::Relaxed)
            }));

            Ok(())
        }).unwrap();
    }

    let a = emitter.emit("test");
    let b = emitter.emit_value("test", 42);
    let c = emitter.emit_value("test", "random string");

    let d = emitter.emit_value_sync("some_sync", MyType::default());

    println!("All events: {:?}", emitter.event_names().unwrap());

    let all = a.join4(b, c, d);

    assert_eq!(all.wait().unwrap(), (3, 3, 3, 4));
}
