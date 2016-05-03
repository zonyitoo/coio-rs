use std::sync::Arc;

use super::Core;
use super::cores::*;
use super::Promise;

pub struct Future<To, Eo>(Arc<Core<To, Eo>>);
unsafe impl<To, Eo> Send for Future<To, Eo> {}

impl<To, Eo> Future<To, Eo>
    where To: 'static,
          Eo: 'static
{
    pub fn with_core(core: Arc<Core<To, Eo>>) -> Future<To, Eo> {
        Future(core)
    }

    pub fn with_value(result: Result<To, Eo>) -> Future<To, Eo> {
        Future(PlainCore::new(Some(result)))
    }

    pub fn resolved(val: To) -> Future<To, Eo> {
        Self::with_value(Ok(val))
    }

    pub fn rejected(val: Eo) -> Future<To, Eo> {
        Self::with_value(Err(val))
    }

    pub fn then<Tc, F>(&self, func: F) -> Future<Tc, Eo>
        where Tc: 'static,
              F: FnOnce(Promise<Tc, Eo>, To) + 'static
    {
        let core = ThenCore::<Tc, Eo, To, F>::new(func);
        self.0.set_next(core.clone());
        Future(core)
    }

    pub fn catch<Ec, F>(&self, func: F) -> Future<To, Ec>
        where Ec: 'static,
              F: FnOnce(Promise<To, Ec>, Eo) + 'static
    {
        let core = CatchCore::<To, Ec, Eo, F>::new(func);
        self.0.set_next(core.clone());
        Future(core)
    }

    pub fn await(&self) -> Result<To, Eo> {
        let core = AwaitCore::<To, Eo>::new();
        self.0.set_next(core.clone());

        loop {
            if let Some(result) = core.await() {
                return result;
            }
        }
    }
}
