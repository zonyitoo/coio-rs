use std::sync::Arc;

use super::CoreWrapper;
use super::super::{ContinueWith, Core};

pub struct PlainCoreInner<To, Eo> {
    result: Option<Result<To, Eo>>,
    next: Option<Arc<ContinueWith<To, Eo>>>,
}

pub type PlainCore<To, Eo> = CoreWrapper<PlainCoreInner<To, Eo>>;

impl<To, Eo> PlainCore<To, Eo> {
    pub fn new(result: Option<Result<To, Eo>>) -> Arc<PlainCore<To, Eo>> {
        PlainCore::wrap(PlainCoreInner {
            result: result,
            next: None,
        })
    }
}

impl<To, Eo> ContinueWith<To, Eo> for PlainCore<To, Eo> {
    fn continue_with(&self, val: Result<To, Eo>) {
        continue_with_impl!(self, val)
    }
}

impl<To, Eo> Core<To, Eo> for PlainCore<To, Eo> {
    fn set_next(&self, core: Arc<ContinueWith<To, Eo>>) {
        set_next_impl!(self, core)
    }

    fn settle(&self, val: Result<To, Eo>) {
        settle_impl!(self, val)
    }
}
