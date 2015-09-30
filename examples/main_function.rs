extern crate coio;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use coio::Scheduler;

fn main() {
    let counter = Arc::new(AtomicUsize::new(0));
    let cloned_counter = counter.clone();

    let result = Scheduler::with_workers(1).run(move|| {
        // Spawn a new coroutine
        Scheduler::spawn(move|| {
            struct Guard(Arc<AtomicUsize>);

            impl Drop for Guard {
                fn drop(&mut self) {
                    self.0.store(1, Ordering::SeqCst);
                }
            }

            let _guard = Guard(cloned_counter);

            coio::sleep_ms(10_000);
            println!("Not going to run this line");
        });

        // Exit right now, which will cause the coroutine to be destroyed.
        panic!("Exit right now!!");
    });

    assert!(result.is_err() && counter.load(Ordering::SeqCst) == 1);
}
