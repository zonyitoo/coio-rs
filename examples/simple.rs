extern crate coio;

use coio::Scheduler;

fn main() {
    Scheduler::with_workers(1)
        .run(|| {
            for _ in 0..10 {
                println!("Heil Hydra");
                Scheduler::sched();
            }
        }).unwrap();
}
