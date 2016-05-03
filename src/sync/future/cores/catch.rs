use std::marker::PhantomData;
use std::sync::{Arc, Weak};

use super::CoreWrapper;
use super::super::{ContinueWith, Core};
use super::super::Promise;

pub struct CatchCoreInner<To, Eo, Ei, F>
    where To: 'static,
          Eo: 'static,
          Ei: 'static,
          F: FnOnce(Promise<To, Eo>, Ei) + 'static
{
    result: Option<Result<To, Eo>>,
    next: Option<Arc<ContinueWith<To, Eo>>>,
    handler: Option<(F, Weak<Core<To, Eo>>)>,
    _ei: PhantomData<Ei>,
}

pub type CatchCore<To, Eo, Ei, F> = CoreWrapper<CatchCoreInner<To, Eo, Ei, F>>;

impl<To, Eo, Ei, F> CatchCore<To, Eo, Ei, F>
    where To: 'static,
          Eo: 'static,
          Ei: 'static,
          F: FnOnce(Promise<To, Eo>, Ei) + 'static
{
    pub fn new(func: F) -> Arc<CatchCore<To, Eo, Ei, F>> {
        let core = CatchCore::wrap(CatchCoreInner {
            result: None,
            next: None,
            handler: None,
            _ei: PhantomData,
        });

        let weak_core = {
            // TODO: without clone()
            let core = core.clone() as Arc<Core<To, Eo>>;
            Arc::downgrade(&core)
        };

        {
            let mut inner = core.inner.lock();
            inner.handler = Some((func, weak_core));
        }

        core
    }
}

impl<To, Eo, Ei, F> ContinueWith<To, Ei> for CatchCore<To, Eo, Ei, F>
    where To: 'static,
          Eo: 'static,
          Ei: 'static,
          F: FnOnce(Promise<To, Eo>, Ei) + 'static
{
    fn continue_with(&self, val: Result<To, Ei>) {
        match val {
            Err(val) => invoke_handler_impl!(self, val),
            Ok(val) => continue_with_impl!(self, Ok(val)),
        }
    }
}

impl<To, Eo, Ei, F> Core<To, Eo> for CatchCore<To, Eo, Ei, F>
    where To: 'static,
          Eo: 'static,
          Ei: 'static,
          F: FnOnce(Promise<To, Eo>, Ei) + 'static
{
    fn set_next(&self, core: Arc<ContinueWith<To, Eo>>) {
        set_next_impl!(self, core)
    }

    fn settle(&self, val: Result<To, Eo>) {
        settle_impl!(self, val)
    }
}
