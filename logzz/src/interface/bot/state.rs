use dashmap::DashMap;
use std::collections::HashSet;
use std::sync::Arc;

pub use crate::application::search::PAGE_SIZE;
use crate::application::search::SearchService;

#[derive(Clone)]
pub struct Session {
    pub query: String,
    pub search_type: String,
    pub tags: Vec<String>,
    pub page: usize,
    pub has_next: bool,
}

pub type SessionStore = Arc<DashMap<(i64, u32), Session>>;

#[derive(Clone)]
pub struct BotState {
    pub search: SearchService,
    pub results_dir: String,
    pub input_dir: String,
    pub archive_dir: String,
    pub sessions: SessionStore,
    pub allowed_user_ids: Arc<HashSet<i64>>,
}

impl BotState {
    pub fn new(
        search: SearchService,
        results_dir: String,
        input_dir: String,
        archive_dir: String,
        allowed_user_ids: Vec<i64>,
    ) -> Self {
        Self {
            search,
            results_dir,
            input_dir,
            archive_dir,
            sessions: Arc::new(DashMap::new()),
            allowed_user_ids: Arc::new(allowed_user_ids.into_iter().collect()),
        }
    }

    pub fn is_user_allowed(&self, user_id: i64) -> bool {
        self.allowed_user_ids.is_empty() || self.allowed_user_ids.contains(&user_id)
    }
}
