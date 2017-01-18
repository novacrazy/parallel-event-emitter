#![feature(box_syntax)]

#[macro_use]
extern crate trace_error;
extern crate parallel_event_emitter;
extern crate thread_id;
extern crate futures;

use futures::Future;
use parallel_event_emitter::*;

use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Hash, PartialEq, Eq, Clone)]
enum MyEvents {
    EventA,
    EventB,
}

#[derive(Debug, Default)]
struct MyType {
    value: AtomicUsize,
}

fn main() {
    let mut emitter = ParallelEventEmitter::<MyEvents>::new();

    let some_value = 1337;

    emitter.add_listener(MyEvents::EventA, move || {
        println!("Thread: {:<6} No arguments {}", thread_id::get(), some_value);

        Ok(())
    }).unwrap();

    emitter.add_listener_value(MyEvents::EventA, |arg: Option<i32>| {
        println!("Thread: {:<6} Expected i32: {:?}", thread_id::get(), arg);

        Ok(())
    }).unwrap();

    emitter.add_listener_value(MyEvents::EventA, |arg: Option<&str>| {
        println!("Thread: {:<6} Expected &str: {:?}", thread_id::get(), arg);

        Ok(())
    }).unwrap();

    for i in 0..4 {
        emitter.add_listener_sync(MyEvents::EventB, move |arg: Option<&MyType>| {
            println!("Thread: {:<6} Expected sync {}: {:?}", thread_id::get(), i, arg.map(|arg| {
                arg.value.fetch_add(1, Ordering::Relaxed)
            }));

            Ok(())
        }).unwrap();
    }

    let a = emitter.emit(MyEvents::EventA);
    let b = emitter.emit_value(MyEvents::EventA, 42);
    let c = emitter.emit_value(MyEvents::EventA, "random string");

    let d = emitter.emit_value_sync(MyEvents::EventB, MyType::default());

    println!("All events: {:?}", emitter.event_names().unwrap());

    let all = a.join4(b, c, d);

    assert_eq!(all.wait().unwrap(), (3, 3, 3, 4));
}
