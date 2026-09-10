use eyre::Result;
use std::collections::HashMap;
use std::sync::Arc;

use crate::domain::CredentialMatch;
use crate::domain::repository::{
    CredentialRepository, GroupedCredential, SearchQuery, SearchType,
};

pub const PAGE_SIZE: usize = 50;

#[derive(Debug, Clone)]
pub struct SearchPage {
    pub records: Vec<CredentialMatch>,
    pub page: usize,
    pub has_next: bool,
    pub total_unique: u64,
}

#[derive(Clone)]
pub struct SearchService {
    repo: Arc<dyn CredentialRepository>,
    page_size: usize,
}

impl SearchService {
    pub fn new(repo: Arc<dyn CredentialRepository>) -> Self {
        Self {
            repo,
            page_size: PAGE_SIZE,
        }
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    pub async fn search(
        &self,
        term: &str,
        search_type: SearchType,
        tags: &[String],
        page: usize,
    ) -> Result<SearchPage> {
        let offset = page * self.page_size;
        let query = SearchQuery {
            term: term.to_string(),
            search_type,
            tags: tags.to_vec(),
            limit: self.page_size + 1,
            offset,
        };

        let mut grouped = self.repo.search_grouped(&query).await?;
        let has_next = grouped.len() > self.page_size;
        if has_next {
            grouped.truncate(self.page_size);
        }

        let mut all_hashes: Vec<String> = grouped
            .iter()
            .flat_map(|g| g.file_hashes.iter().cloned())
            .collect();
        all_hashes.sort();
        all_hashes.dedup();

        let hash_to_paths = self
            .repo
            .paths_for_hashes(&all_hashes)
            .await
            .unwrap_or_default();

        let mut records: Vec<CredentialMatch> = grouped
            .iter()
            .map(|g| build_match(g, &hash_to_paths))
            .collect();

        let keys: Vec<String> = records.iter().map(|r| r.cred_key.clone()).collect();
        let key_to_tags = self.repo.tags_for_keys(&keys).await.unwrap_or_default();
        for record in &mut records {
            if let Some(tags) = key_to_tags.get(&record.cred_key) {
                record.tags = tags.clone();
            }
        }

        let total_unique = self.repo.count_grouped(&query).await.unwrap_or(0);

        Ok(SearchPage {
            records,
            page,
            has_next,
            total_unique,
        })
    }
}

fn build_match(
    row: &GroupedCredential,
    hash_to_paths: &HashMap<String, Vec<String>>,
) -> CredentialMatch {
    let mut all_paths: Vec<String> = row
        .file_hashes
        .iter()
        .flat_map(|h| hash_to_paths.get(h).cloned().unwrap_or_default())
        .collect();

    for p in &row.source_files {
        if !all_paths.contains(p) {
            all_paths.push(p.clone());
        }
    }

    all_paths.sort();
    all_paths.dedup();

    let primary_path = all_paths.first().cloned().unwrap_or_default();
    let cred_key = crate::domain::credential_key(&row.url, &row.username, &row.password);

    CredentialMatch {
        cred_key,
        url: row.url.clone(),
        username: row.username.clone(),
        password: row.password.clone(),
        extra_json: row.extra_json.clone(),
        primary_path,
        all_paths,
        tags: Vec::new(),
    }
}
