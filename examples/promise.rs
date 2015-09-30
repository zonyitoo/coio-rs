extern crate coio;

use coio::promise::Promise;
use coio::Scheduler;

fn main() {
    Scheduler::with_workers(4)
        .run(|| {
        coio::spawn(|| {
            let r = Promise::spawn(|| {
                if true {
                    Ok(1.23)
                } else {
                    Err("Final error")
                }
            }).then(|res| {
                assert_eq!(res, 1.23);
                Ok(34)
            }, |err| {
                assert_eq!(err, "Final error");
                if true { Ok(35) } else { Err(44u64)}
            }).sync();

            assert_eq!(r, Ok(34));
        });
    }).unwrap();
}
