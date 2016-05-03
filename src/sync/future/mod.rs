// Most parts of this module use a certain pattern to describe template parameters.
// This pattern is "[TE][ioc]" and is described as follows:
//
// T => The Ok(T) inside the stored or returned Result<T, _>
// E => The Err(E) inside the stored or returned Result<_, E>
//
// i => The i(nput) type from the previous for the current Core/Promise/Future
// o => The o(utput) type of the current Core/Promise/Future
// c => The c(ontinuation) type of the current Core/Promise/Future
//      (with which it is being continued using `.then()` or `.catch()`)

mod cores;
mod future;
mod promise;

pub use self::future::*;
pub use self::promise::*;

use std::sync::Arc;

use self::cores::PlainCore;

/// A `Core` is the shared data between a Promise and a Future
///
/// Your `Core` needs to implement 2 methods:
/// * `set_next`: A Future is passed which must be resolved with
///    the result of the current one after it has settled.
/// * `settle`: Sets the result of the Future. This is usually called
///    by a `Promise` to settle a `Future`.
pub trait Core<To, Eo> {
    fn set_next(&self, core: Arc<ContinueWith<To, Eo>>);
    fn settle(&self, val: Result<To, Eo>);
}

/// This trait serves as the counterpart for `Core::set_next`
///
/// When a `Core` is settled it will call `continue_with` on the "next" `Core`.
/// Thus we achieve support for continuation with Futures.
// TODO: Splitting this method into resolve/reject seems to be 5% faster.
pub trait ContinueWith<Ti, Ei> {
    fn continue_with(&self, val: Result<Ti, Ei>);
}

pub fn make<To, Eo>() -> (Promise<To, Eo>, Future<To, Eo>)
    where To: 'static,
          Eo: 'static
{
    let core = PlainCore::new(None);
    (Promise::with_core(core.clone()), Future::with_core(core))
}
