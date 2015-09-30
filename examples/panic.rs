extern crate coio;

use coio::Scheduler;

fn main() {
    Scheduler::with_workers(1)
        .run(|| {
            let handle = Scheduler::spawn(|| {
                panic!("Panicked inside");
            });

            assert!(handle.join().is_err());
        }).unwrap();
}
