use std::sync::Arc;

use super::super::Spinlock;

pub struct CoreWrapper<T> {
    inner: Spinlock<T>,
}

impl<T> CoreWrapper<T> {
    #[inline(always)]
    fn wrap(inner: T) -> Arc<CoreWrapper<T>> {
        Arc::new(CoreWrapper {
            inner: Spinlock::new(inner),
        })
    }
}

macro_rules! continue_with_impl {
    ($this:ident, $val:expr) => {{
        let mut inner = $this.inner.lock();

        if let Some(ref next) = inner.next {
            next.continue_with($val);
            return;
        }

        inner.result = Some($val);
    }}
}

macro_rules! invoke_handler_impl {
    ($this:ident, $val:expr) => {{
        let (handler, p) = {
            let mut inner = $this.inner.lock();
            let (handler, p) = inner.handler.take().expect("handler missing");
            let p = Promise::with_core(p.upgrade().unwrap());

            (handler, p)
        };

        handler(p, $val);
    }}
}

macro_rules! set_next_impl {
    ($this:ident, $next:expr) => {{
        let mut inner = $this.inner.lock();
        assert!(inner.next.is_none());

        match inner.result.take() {
            Some(val) => $next.continue_with(val),
            None => {}
        }

        inner.next = Some($next);
    }}
}

macro_rules! settle_impl {
    ($this:ident, $val:expr) => {{
        let mut inner = $this.inner.lock();


        if let Some(ref next) = inner.next {
            next.continue_with($val);
            return;
        }

        inner.result = Some($val);
    }}
}

mod await;
mod catch;
mod plain;
mod then;

pub use self::await::*;
pub use self::catch::*;
pub use self::plain::*;
pub use self::then::*;
