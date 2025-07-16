//! n3 task asynchronous task spawner facade.

#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(feature = "futures-executor")]
#[cfg_attr(docsrs, doc(cfg(feature = "futures-executor")))]
mod futures_executor;

#[cfg(feature = "futures-executor")]
pub use futures_executor::*;

#[cfg(feature = "futures-executor-local")]
#[cfg_attr(docsrs, doc(cfg(feature = "futures-executor-local")))]
mod futures_executor_local;

#[cfg(feature = "futures-executor-local")]
pub use futures_executor_local::*;
