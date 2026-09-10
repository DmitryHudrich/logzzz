use std::sync::Arc;
use tracing::{info, warn};

use crate::domain::source::{ArchiveInbox, LogSource};

pub struct SourceScheduler {
    sources: Vec<Arc<dyn LogSource>>,
    inbox: Arc<dyn ArchiveInbox>,
}

impl SourceScheduler {
    pub fn new(inbox: Arc<dyn ArchiveInbox>) -> Self {
        Self {
            sources: Vec::new(),
            inbox,
        }
    }

    pub fn register(&mut self, source: Arc<dyn LogSource>) {
        info!(source = %source.name(), "registered log source");
        self.sources.push(source);
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn source_names(&self) -> Vec<String> {
        self.sources.iter().map(|s| s.name().to_string()).collect()
    }

    pub async fn poll_all(&self) -> usize {
        let mut total = 0usize;
        for source in &self.sources {
            match source.poll(self.inbox.as_ref()).await {
                Ok(count) => {
                    if count > 0 {
                        info!(source = %source.name(), deposited = count, "log source deposited archives");
                    }
                    total += count;
                }
                Err(error) => {
                    warn!(source = %source.name(), error = %error, "log source poll failed");
                }
            }
        }
        total
    }
}
