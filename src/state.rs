use crate::services::execution::ExecutionBackends;
use crate::services::inference::InferenceClient;
use crate::services::task_runner::TaskRunner;
use qdrant_client::Qdrant;
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub qdrant: Arc<Qdrant>,
    pub inference: InferenceClient,
    pub runner: Arc<TaskRunner>,
    pub backends: ExecutionBackends,
}

impl AppState {
    pub fn new(db: PgPool, qdrant: Arc<Qdrant>) -> Self {
        let inference = InferenceClient::from_env();
        let backends = ExecutionBackends::from_env(db.clone(), qdrant.clone(), inference.clone());
        let runner = Arc::new(TaskRunner::new(
            db.clone(),
            qdrant.clone(),
            inference.clone(),
            backends.clone(),
        ));
        Self {
            db,
            qdrant,
            inference,
            runner,
            backends,
        }
    }
}
