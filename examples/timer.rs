extern crate coio;

use std::sync::Arc;

use coio::Scheduler;

fn invoke_every_ms<F>(ms: u64, f: F)
    where F: Fn() + Sync + Send + 'static
{
    let f = Arc::new(f);
    coio::spawn(move|| {
        loop {
            coio::sleep_ms(ms);
            let f = f.clone();
            coio::spawn(move|| (*f)());
        }
    });
}

fn invoke_after_ms<F>(ms: u64, f: F)
    where F: FnOnce() + Send + 'static
{
    coio::spawn(move|| {
        coio::sleep_ms(ms);
        f();
    });
}

fn main() {
    Scheduler::with_workers(2)
        .run(|| {
            coio::spawn(|| {
                for i in 0..10 {
                    println!("Qua  {}", i);
                    coio::sleep_ms(2000);
                }
            });

            invoke_every_ms(1000, || println!("Purr :P"));
            invoke_after_ms(10000, || println!("Tadaaaaaaa"));
        }).unwrap();
}
