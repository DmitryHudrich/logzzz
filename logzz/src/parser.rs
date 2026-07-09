use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::records::{AccountRecord, ParseIssue, ParseReport, RawRecord};

pub fn parse_file(path: &Path) -> ParseReport {
    let parser = Parser::new();

    match fs::read_to_string(path) {
        Ok(content) => parser.parse_text(&content, &path.display().to_string()),
        Err(err) => ParseReport {
            records: vec![],
            issues: vec![ParseIssue {
                source_file: path.display().to_string(),
                message: format!("failed to read file: {err}"),
                raw_lines: vec![],
            }],
        },
    }
}

#[derive(Debug)]
pub struct Parser {
    pub kv_re: Regex,
    pub url_re: Regex,
}

impl Parser {
    pub fn new() -> Self {
        Self {
            kv_re: Regex::new(r"(?iu)^\s*([[:alnum:]_а-яё /\.\-]+?)\s*[:=\-]\s*(.*?)\s*$").unwrap(),
            url_re: Regex::new(r"(?i)\bhttps?://\S+").unwrap(),
        }
    }

    pub fn parse_text(&self, input: &str, source_file: &str) -> ParseReport {
        let mut report = ParseReport::default();

        let lines: Vec<&str> = input.lines().collect();
        let hash = {
            let mut hasher = Sha256::new();
            hasher.update(input);
            hex::encode(hasher.finalize())
        };

        let start_idx = lines
            .iter()
            .position(|line| self.is_probably_record_line(line))
            .unwrap_or(lines.len());

        let mut current = RawRecord::default();
        let mut noise_streak = 0usize;

        for line in &lines[start_idx..] {
            let trimmed = line.trim();

            if trimmed.is_empty() {
                if !current.fields.is_empty() {
                    noise_streak += 1;
                    if noise_streak >= 2 {
                        self.flush_current(&mut current, source_file, &hash, &mut report);
                        noise_streak = 0;
                    }
                }
                continue;
            }

            if self.is_separator_or_banner_line(trimmed) {
                if !current.fields.is_empty() {
                    self.flush_current(&mut current, source_file, &hash, &mut report);
                }
                noise_streak = 0;
                continue;
            }

            if let Some((key, value)) = self.parse_kv_line(trimmed) {
                let canonical_key = normalize_key(&key);

                if !current.fields.is_empty() && current.fields.contains_key(&canonical_key) {
                    self.flush_current(&mut current, source_file, &hash, &mut report);
                }

                current.raw_lines.push(trimmed.to_string());
                current.fields.insert(canonical_key, value);
                noise_streak = 0;
                continue;
            }
        }

        if !current.fields.is_empty() {
            self.flush_current(&mut current, source_file, &hash, &mut report);
        }

        report
    }

    pub fn flush_current(
        &self,
        current: &mut RawRecord,
        source_file: &str,
        source_file_hash: &str,
        report: &mut ParseReport,
    ) {
        let raw = std::mem::take(current).into_parts();

        match self.raw_to_account(raw.0, source_file, source_file_hash) {
            Ok(record) => report.records.push(record),
            Err(message) => report.issues.push(ParseIssue {
                source_file: source_file.to_string(),
                message,
                raw_lines: raw.1,
            }),
        }
    }

    pub fn raw_to_account(
        &self,
        fields: HashMap<String, String>,
        source_file: &str,
        source_file_hash: &str,
    ) -> Result<AccountRecord, String> {
        let mut extra = fields;

        let url = extra.remove("url");
        let username = extra
            .remove("username")
            .or_else(|| extra.remove("email"))
            .or_else(|| extra.remove("login"));
        let password = extra.remove("password");

        if url.is_none() && username.is_none() && password.is_none() {
            return Err("record does not contain useful fields".to_string());
        }

        Ok(AccountRecord {
            url,
            username,
            password,
            source_file: source_file.to_string(),
            file_hash: source_file_hash.to_string(),
            extra,
        })
    }

    pub fn parse_kv_line(&self, line: &str) -> Option<(String, String)> {
        let caps = self.kv_re.captures(line)?;
        let key = caps.get(1)?.as_str().trim();
        let value = caps.get(2)?.as_str().trim();

        if key.is_empty() || value.is_empty() {
            return None;
        }

        if self.looks_like_banner_text(key, value) {
            return None;
        }

        Some((key.to_string(), value.to_string()))
    }

    pub fn extract_url_only(&self, line: &str) -> Option<String> {
        if self.looks_like_separator(line) {
            return None;
        }

        let m = self.url_re.find(line)?;
        Some(m.as_str().to_string())
    }

    pub fn is_probably_record_line(&self, line: &str) -> bool {
        let line = line.trim();
        if line.is_empty() {
            return false;
        }

        self.parse_kv_line(line).is_some() || self.extract_url_only(line).is_some()
    }

    pub fn is_separator_or_banner_line(&self, line: &str) -> bool {
        self.looks_like_separator(line) || self.looks_like_banner_line(line)
    }

    pub fn looks_like_separator(&self, line: &str) -> bool {
        let t = line.trim();

        if t.len() >= 3 && t.chars().all(|c| "=|-|_|*|#|~|.".contains(c)) {
            return true;
        }

        let non_alnum = t.chars().filter(|c| !c.is_alphanumeric()).count();
        let total = t.chars().count().max(1);

        total >= 8 && non_alnum * 100 / total >= 85
    }

    pub fn looks_like_banner_line(&self, line: &str) -> bool {
        let t = line.trim();

        if t.is_empty() {
            return false;
        }

        let upper = t.to_uppercase();

        if upper.contains("JOIN OUR CHANNEL") {
            return true;
        }

        let alnum = t.chars().filter(|c| c.is_alphanumeric()).count();
        let non_alnum = t
            .chars()
            .filter(|c| !c.is_alphanumeric() && !c.is_whitespace())
            .count();
        let total = t.chars().count().max(1);

        if total >= 20 && non_alnum * 100 / total >= 50 && alnum * 100 / total <= 35 {
            return true;
        }

        let url_count = self.url_re.find_iter(t).count();
        if url_count >= 2 {
            return true;
        }

        false
    }

    pub fn looks_like_banner_text(&self, key: &str, value: &str) -> bool {
        let s = format!("{key} {value}").to_uppercase();
        s.contains("JOIN OUR CHANNEL")
    }
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize_key(key: &str) -> String {
    fn canonicalize_key(key: &str) -> String {
        key.trim()
            .to_lowercase()
            .replace('ё', "е")
            .replace([' ', '_', '-', '.', '/'], "")
    }
    let k = canonicalize_key(key);

    match k.as_str() {
        "url" | "host" | "site" | "link" | "website" | "web" | "адрес" | "сайт" => {
            "url".to_string()
        }

        "username" | "user" | "login" | "nickname" | "nick" | "логин" | "пользователь" => {
            "username".to_string()
        }

        "password" | "pass" | "pwd" | "пароль" => "password".to_string(),

        "email" | "mail" | "почта" => "email".to_string(),

        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_key_maps_synonyms_and_cyrillic() {
        assert_eq!(normalize_key("URL"), "url");
        assert_eq!(normalize_key("Host"), "url");
        assert_eq!(normalize_key("сайт"), "url");
        assert_eq!(normalize_key("Login"), "username");
        assert_eq!(normalize_key("логин"), "username");
        assert_eq!(normalize_key("Pass"), "password");
        assert_eq!(normalize_key("E-Mail"), "email");
        assert_eq!(normalize_key("Ёж"), "еж");
        assert_eq!(normalize_key("Some Other Field"), "someotherfield");
    }

    #[test]
    fn looks_like_separator_detects_repeated_symbols_and_high_noise_ratio() {
        let parser = Parser::new();
        assert!(parser.looks_like_separator("-----------------"));
        assert!(parser.looks_like_separator("================"));
        assert!(parser.looks_like_separator("**###**###**###**"));
        assert!(!parser.looks_like_separator("URL: https://example.com"));
        assert!(!parser.looks_like_separator("hi"));
    }

    #[test]
    fn looks_like_banner_line_detects_promo_text_and_multi_url_spam() {
        let parser = Parser::new();
        assert!(parser.looks_like_banner_line("!!! JOIN OUR CHANNEL @stealer_logs !!!"));
        assert!(parser.looks_like_banner_line(
            "visit https://a.example.com and https://b.example.com for more logs"
        ));
        assert!(!parser.looks_like_banner_line("Username: user@example.com"));
    }

    #[test]
    fn parse_kv_line_extracts_key_value_and_rejects_banner_lines() {
        let parser = Parser::new();
        assert_eq!(
            parser.parse_kv_line("URL: https://example.com"),
            Some(("URL".to_string(), "https://example.com".to_string()))
        );
        assert_eq!(parser.parse_kv_line("Login: bob"), Some(("Login".to_string(), "bob".to_string())));
        assert_eq!(parser.parse_kv_line("Join our channel: @spam"), None);
        assert_eq!(parser.parse_kv_line("just some text"), None);
    }

    #[test]
    fn parse_text_splits_multiple_records_separated_by_dashes() {
        let parser = Parser::new();
        let input = "\
SOFT: StealerName
URL: https://example.com
Login: alice
Password: hunter2
-----------------------------------------
URL: https://another.example.com
Username: bob
Password: correcthorse
";
        let report = parser.parse_text(input, "sample.txt");

        assert_eq!(report.records.len(), 2);
        assert_eq!(report.records[0].url.as_deref(), Some("https://example.com"));
        assert_eq!(report.records[0].username.as_deref(), Some("alice"));
        assert_eq!(report.records[0].password.as_deref(), Some("hunter2"));
        assert_eq!(
            report.records[1].url.as_deref(),
            Some("https://another.example.com")
        );
        assert_eq!(report.records[1].username.as_deref(), Some("bob"));
    }

    #[test]
    fn parse_text_skips_leading_banner_noise() {
        let parser = Parser::new();
        let input = "\
!!! JOIN OUR CHANNEL @stealer_logs !!!
################################
URL: https://example.com
Username: alice
Password: hunter2
";
        let report = parser.parse_text(input, "sample.txt");

        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].username.as_deref(), Some("alice"));
    }
}
