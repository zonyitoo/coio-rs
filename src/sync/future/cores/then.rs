// Copyright 2015 The coio Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::marker::PhantomData;
use std::sync::{Arc, Weak};

use super::CoreWrapper;
use super::super::{ContinueWith, Core};
use super::super::Promise;

pub struct ThenCoreInner<To, Eo, Ti, F>
    where To: 'static,
          Eo: 'static,
          Ti: 'static,
          F: FnOnce(Promise<To, Eo>, Ti) + 'static
{
    result: Option<Result<To, Eo>>,
    next: Option<Arc<ContinueWith<To, Eo>>>,
    handler: Option<(F, Weak<Core<To, Eo>>)>,
    _ti: PhantomData<Ti>,
}

pub type ThenCore<To, Eo, Ti, F> = CoreWrapper<ThenCoreInner<To, Eo, Ti, F>>;

impl<To, Eo, Ti, F> ThenCore<To, Eo, Ti, F>
    where To: 'static,
          Eo: 'static,
          Ti: 'static,
          F: FnOnce(Promise<To, Eo>, Ti) + 'static
{
    pub fn new(func: F) -> Arc<ThenCore<To, Eo, Ti, F>> {
        let core = ThenCore::wrap(ThenCoreInner {
            result: None,
            next: None,
            handler: None,
            _ti: PhantomData,
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

impl<To, Eo, Ti, F> ContinueWith<Ti, Eo> for ThenCore<To, Eo, Ti, F>
    where To: 'static,
          Eo: 'static,
          Ti: 'static,
          F: FnOnce(Promise<To, Eo>, Ti) + 'static
{
    fn continue_with(&self, val: Result<Ti, Eo>) {
        match val {
            Ok(val) => invoke_handler_impl!(self, val),
            Err(val) => continue_with_impl!(self, Err(val)),
        }
    }
}

impl<To, Eo, Ti, F> Core<To, Eo> for ThenCore<To, Eo, Ti, F>
    where To: 'static,
          Eo: 'static,
          Ti: 'static,
          F: FnOnce(Promise<To, Eo>, Ti) + 'static
{
    fn set_next(&self, core: Arc<ContinueWith<To, Eo>>) {
        set_next_impl!(self, core)
    }

    fn settle(&self, val: Result<To, Eo>) {
        settle_impl!(self, val)
    }
}
