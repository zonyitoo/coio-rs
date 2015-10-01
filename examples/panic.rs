extern crate coio;

use coio::Scheduler;

fn main() {
    Scheduler::new().run(|| {
        let handle = Scheduler::spawn(|| {
            panic!("Panicked inside");
        });

        assert!(handle.join().is_err());
    }).unwrap();
}
