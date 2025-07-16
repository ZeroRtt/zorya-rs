use std::{
    io::{Error, Result},
    sync::OnceLock,
};

use futures::{executor::ThreadPool, task::SpawnExt};

static THREAD_POOL: OnceLock<ThreadPool> = OnceLock::new();

/// Spawns a task that polls the given future with output () to completion.
pub fn spawn<Fut>(future: Fut) -> Result<()>
where
    Fut: Future<Output = ()> + Send + 'static,
{
    THREAD_POOL
        .get_or_init(|| {
            ThreadPool::builder()
                .pool_size(num_cpus::get())
                .create()
                .unwrap()
        })
        .spawn(future)
        .map_err(|err| Error::other(err))
}
