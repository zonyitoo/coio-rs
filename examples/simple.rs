extern crate coio;

use coio::Scheduler;

fn main() {
    Scheduler::new()
        .run(|| {
            for _ in 0..10 {
                println!("Heil Hydra");
                Scheduler::sched();
            }
        }).unwrap();
}
