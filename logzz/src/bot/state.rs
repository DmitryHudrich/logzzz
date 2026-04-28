use clickhouse::Client;
use dashmap::DashMap;
use std::sync::Arc;

pub const PAGE_SIZE: usize = 50;
pub const FETCH_LIMIT: usize = PAGE_SIZE;

#[derive(Clone)]
pub struct Session {
    pub query: String,
    pub search_type: String,
    pub page: usize,
    pub has_next: bool,
}

pub type SessionStore = Arc<DashMap<(i64, u32), Session>>;

#[derive(Clone)]
pub struct BotState {
    pub client: Arc<Client>,
    pub results_dir: String,
    pub input_dir: String,
    pub archive_dir: String,
    pub sessions: SessionStore,
}

impl BotState {
    pub fn new(
        client: Arc<Client>,
        results_dir: String,
        input_dir: String,
        archive_dir: String,
    ) -> Self {
        Self {
            client,
            results_dir,
            input_dir,
            archive_dir,
            sessions: Arc::new(DashMap::new()),
        }
    }
}
