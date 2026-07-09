use clickhouse::Client;
use dashmap::DashMap;
use std::collections::HashSet;
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
    pub allowed_user_ids: Arc<HashSet<i64>>,
}

impl BotState {
    pub fn new(
        client: Arc<Client>,
        results_dir: String,
        input_dir: String,
        archive_dir: String,
        allowed_user_ids: Vec<i64>,
    ) -> Self {
        Self {
            client,
            results_dir,
            input_dir,
            archive_dir,
            sessions: Arc::new(DashMap::new()),
            allowed_user_ids: Arc::new(allowed_user_ids.into_iter().collect()),
        }
    }

    /// An empty allowlist means access control is not configured and every
    /// user is allowed; this keeps the bot usable out of the box while
    /// `LOGZZ_TELEGRAM__ALLOWED_USER_IDS` is documented as strongly recommended.
    pub fn is_user_allowed(&self, user_id: i64) -> bool {
        self.allowed_user_ids.is_empty() || self.allowed_user_ids.contains(&user_id)
    }
}
