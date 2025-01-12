use std::sync::Arc;

use sqlx::PgPool;
use verifier_dal::Connection;

use crate::implementations::layers::Layer;

pub struct VerifierPoolsLayer {
    pub pool: Arc<PgPool>,
}

impl VerifierPoolsLayer {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool: Arc::new(pool),
        }
    }

    pub async fn access_storage<'a, F, Ret>(&self, callback: F) -> Ret
    where
        F: FnOnce(&mut Connection<'_>) -> Ret,
    {
        let mut conn = self.pool.acquire().await.unwrap();
        let mut storage = Connection::new(&mut conn);
        callback(&mut storage)
    }
}

impl Layer for VerifierPoolsLayer {
    fn layer_name(&self) -> &'static str {
        "verifier_pools"
    }
}
