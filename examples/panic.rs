extern crate coio;

use coio::Scheduler;

fn main() {
    let handle = Scheduler::spawn(|| {
        panic!("Panicked inside");
    });

    Scheduler::run(1);

    assert!(handle.join().is_err());
}
