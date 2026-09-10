use serde::Serialize;
use tokio::sync::broadcast;

use crate::application::ingest::ImportCycleStats;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppEvent {
    Import {
        ts: u64,
        records_inserted: usize,
        files_parsed: usize,
        files_skipped: usize,
        archives_extracted: usize,
        issues_found: usize,
    },
    ArchiveQueued {
        ts: u64,
        archive: String,
    },
    ArchiveExtracting {
        ts: u64,
        archive: String,
    },
    ArchiveDone {
        ts: u64,
        archive: String,
        files_extracted: usize,
        files_parsed: usize,
        records_inserted: usize,
        issues_found: usize,
    },
    ArchiveNeedsPassword {
        ts: u64,
        archive: String,
    },
    ArchiveFailed {
        ts: u64,
        archive: String,
        error: String,
    },
    Problem {
        ts: u64,
        message: String,
    },
}

impl AppEvent {
    pub fn import(stats: &ImportCycleStats) -> Self {
        AppEvent::Import {
            ts: now_ms(),
            records_inserted: stats.records_inserted,
            files_parsed: stats.files_parsed,
            files_skipped: stats.files_skipped,
            archives_extracted: stats.archives_extracted,
            issues_found: stats.issues_found,
        }
    }

    pub fn queued(archive: impl Into<String>) -> Self {
        AppEvent::ArchiveQueued {
            ts: now_ms(),
            archive: archive.into(),
        }
    }

    pub fn extracting(archive: impl Into<String>) -> Self {
        AppEvent::ArchiveExtracting {
            ts: now_ms(),
            archive: archive.into(),
        }
    }

    pub fn archive_done(
        archive: impl Into<String>,
        files_extracted: usize,
        files_parsed: usize,
        records_inserted: usize,
        issues_found: usize,
    ) -> Self {
        AppEvent::ArchiveDone {
            ts: now_ms(),
            archive: archive.into(),
            files_extracted,
            files_parsed,
            records_inserted,
            issues_found,
        }
    }

    pub fn needs_password(archive: impl Into<String>) -> Self {
        AppEvent::ArchiveNeedsPassword {
            ts: now_ms(),
            archive: archive.into(),
        }
    }

    pub fn failed(archive: impl Into<String>, error: impl Into<String>) -> Self {
        AppEvent::ArchiveFailed {
            ts: now_ms(),
            archive: archive.into(),
            error: error.into(),
        }
    }

    pub fn problem(message: impl Into<String>) -> Self {
        AppEvent::Problem {
            ts: now_ms(),
            message: message.into(),
        }
    }
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<AppEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.tx.subscribe()
    }

    pub fn emit(&self, event: AppEvent) {
        let _ = self.tx.send(event);
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(256)
    }
}
