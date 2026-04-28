use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct RawRecord {
    pub fields: HashMap<String, String>,
    pub raw_lines: Vec<String>,
}

impl RawRecord {
    pub fn into_parts(self) -> (HashMap<String, String>, Vec<String>) {
        (self.fields, self.raw_lines)
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct AccountRecord {
    pub url: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub source_file: String,
    pub file_hash: String,
    pub extra: HashMap<String, String>,
}

impl AccountRecord {
    pub fn file_hash(&self) -> &str {
        &self.file_hash
    }

    pub fn url(&self) -> Option<&String> {
        self.url.as_ref()
    }

    pub fn username(&self) -> Option<&String> {
        self.username.as_ref()
    }

    pub fn password(&self) -> Option<&String> {
        self.password.as_ref()
    }

    pub fn source_file(&self) -> &str {
        &self.source_file
    }

    pub fn extra(&self) -> &HashMap<String, String> {
        &self.extra
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ParseIssue {
    pub source_file: String,
    pub message: String,
    pub raw_lines: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct ParseReport {
    pub records: Vec<AccountRecord>,
    pub issues: Vec<ParseIssue>,
}

impl ParseReport {
    pub fn records(&self) -> &[AccountRecord] {
        &self.records
    }

    pub fn issues(&self) -> &[ParseIssue] {
        &self.issues
    }
}
