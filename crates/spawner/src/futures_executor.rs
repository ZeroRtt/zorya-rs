use std::{
    io::{Error, Result},
    sync::OnceLock,
};

use futures::{executor::ThreadPool, task::SpawnExt};

static THREAD_POOL: OnceLock<ThreadPool> = OnceLock::new();

/// Spawns a task that polls the given future with output () to completion.
#[must_use]
pub fn spawn<Fut>(future: Fut) -> Result<()>
where
    Fut: Future<Output = ()> + Send + 'static,
{
    THREAD_POOL
        .get_or_init(|| {
            let cpus = num_cpus::get();
            ThreadPool::builder()
                .pool_size(if cpus < 10 { 10 } else { cpus })
                .create()
                .unwrap()
        })
        .spawn(future)
        .map_err(|err| Error::other(err))
}
