use std::io::{Error, Result};

use futures::{executor::LocalPool, task::LocalSpawnExt};

thread_local! {
    static LOCAL_POOL: LocalPool = LocalPool::new();
}

/// Spawns a task that polls the given future with output () to completion.
pub fn spawn<Fut>(future: Fut) -> Result<()>
where
    Fut: Future<Output = ()> + 'static,
{
    LOCAL_POOL
        .with(|pool| pool.spawner().spawn_local(future))
        .map_err(|err| Error::other(err))
}
