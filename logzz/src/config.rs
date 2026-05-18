use clap::Parser;
use eyre::{Result, eyre};
use serde::Deserialize;
use std::env;
use std::fs;

use crate::archive::{default_archive_dir, sanitize_filename};

const DEFAULT_CONFIG_PATH: &str = "config.yaml";
const DEFAULT_POLL_INTERVAL_SECS: u64 = 5;
const DEFAULT_MIGRATIONS_DIR: &str = "./migrations";
const DEFAULT_INPUT_DIR: &str = "./.local/input";
const DEFAULT_RESULTS_DIR: &str = "./.local/reports";
const DEFAULT_DOWNLOADER_DIR: &str = "./.local/downloader";
const DEFAULT_DOWNLOADER_REST_LISTEN_ADDR: &str = "127.0.0.1:8090";

#[derive(Debug, Parser)]
#[command(name = "logzz")]
pub struct Cli {
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    pub config: String,
    #[arg(long)]
    pub clickhouse_url: Option<String>,
    #[arg(long)]
    pub clickhouse_user: Option<String>,
    #[arg(long)]
    pub clickhouse_password: Option<String>,
    #[arg(long)]
    pub clickhouse_database: Option<String>,
    #[arg(long)]
    pub app_database: Option<String>,
    #[arg(long)]
    pub migrations_dir: Option<String>,
    #[arg(long)]
    pub input_dir: Option<String>,
    #[arg(long)]
    pub archive_dir: Option<String>,
    #[arg(long)]
    pub poll_interval_secs: Option<u64>,
    #[arg(long)]
    pub telegram_token: Option<String>,
    #[arg(long)]
    pub results_dir: Option<String>,
    #[arg(long)]
    pub max_results: Option<usize>,
    #[arg(long)]
    pub proxy: Option<String>,
}

#[derive(Debug, Parser)]
#[command(name = "downloader")]
pub struct DownloaderCli {
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    pub config: String,
    #[arg(value_name = "PEER_NAME")]
    pub peer_name: Option<String>,
    #[arg(long)]
    pub archive_dir: Option<String>,
    #[arg(long)]
    pub state_file: Option<String>,
    #[arg(long)]
    pub poll_interval_secs: Option<u64>,
    #[arg(long)]
    pub session_file: Option<String>,
    #[arg(long)]
    pub rest_listen_addr: Option<String>,
    #[arg(long)]
    pub api_id: Option<i32>,
    #[arg(long)]
    pub api_hash: Option<String>,
    #[arg(long)]
    pub proxy: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub clickhouse: ClickhouseConfig,
    pub migrations_dir: String,
    pub input_dir: String,
    pub archive_dir: String,
    pub poll_interval_secs: u64,
    pub telegram: TelegramConfig,
    pub socks_proxy: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TelegramConfig {
    pub token: String,
    pub results_dir: String,
    pub max_results: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClickhouseConfig {
    pub url: String,
    pub user: String,
    pub password: String,
    pub database: String,
    pub app_database: String,
}

#[derive(Debug, Clone)]
pub struct DownloaderConfig {
    pub peer_name: String,
    pub archive_dir: String,
    pub state_file: String,
    pub poll_interval_secs: u64,
    pub session_file: String,
    pub rest_listen_addr: String,
    pub api_id: i32,
    pub api_hash: String,
    pub socks_proxy: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FileConfig {
    pub clickhouse: Option<PartialClickhouseConfig>,
    pub migrations_dir: Option<String>,
    pub input_dir: Option<String>,
    pub archive_dir: Option<String>,
    pub poll_interval_secs: Option<u64>,
    pub telegram: Option<PartialTelegramConfig>,
    pub downloader: Option<PartialDownloaderConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PartialClickhouseConfig {
    pub url: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub database: Option<String>,
    pub app_database: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PartialTelegramConfig {
    pub token: Option<String>,
    pub results_dir: Option<String>,
    pub max_results: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PartialDownloaderConfig {
    pub peer_name: Option<String>,
    pub archive_dir: Option<String>,
    pub state_file: Option<String>,
    pub poll_interval_secs: Option<u64>,
    pub session_file: Option<String>,
    pub rest_listen_addr: Option<String>,
    pub api_id: Option<i32>,
    pub api_hash: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct LogzzEnv {
    clickhouse_url: Option<String>,
    clickhouse_user: Option<String>,
    clickhouse_password: Option<String>,
    clickhouse_database: Option<String>,
    app_database: Option<String>,
    migrations_dir: Option<String>,
    input_dir: Option<String>,
    archive_dir: Option<String>,
    poll_interval_secs: Option<u64>,
    telegram_token: Option<String>,
    results_dir: Option<String>,
    max_results: Option<usize>,
    socks: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct DownloaderEnv {
    peer_name: Option<String>,
    archive_dir: Option<String>,
    state_file: Option<String>,
    poll_interval_secs: Option<u64>,
    session_file: Option<String>,
    rest_listen_addr: Option<String>,
    api_id: Option<i32>,
    api_hash: Option<String>,
    socks: Option<String>,
}

pub fn load_config(cli: &Cli) -> Result<AppConfig> {
    let file = read_yaml_config(&cli.config)?;
    Ok(build_app_config(file, cli, LogzzEnv::from_env())?)
}

pub fn load_downloader_config(cli: &DownloaderCli) -> Result<DownloaderConfig> {
    let file = read_yaml_config(&cli.config)?;
    Ok(build_downloader_config(
        file,
        cli,
        DownloaderEnv::from_env(),
    )?)
}

fn read_yaml_config(path: &str) -> Result<FileConfig> {
    match fs::read(path) {
        Ok(raw) => Ok(serde_yaml::from_slice(&raw)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(FileConfig::default()),
        Err(error) => Err(error.into()),
    }
}

fn build_app_config(file: FileConfig, cli: &Cli, env: LogzzEnv) -> Result<AppConfig> {
    let file_clickhouse = file.clickhouse.unwrap_or_default();
    let file_telegram = file.telegram.unwrap_or_default();

    let clickhouse = ClickhouseConfig {
        url: pick_required(
            "clickhouse.url",
            [
                env.clickhouse_url,
                cli.clickhouse_url.clone(),
                file_clickhouse.url,
            ],
        )?,
        user: pick_required(
            "clickhouse.user",
            [
                env.clickhouse_user,
                cli.clickhouse_user.clone(),
                file_clickhouse.user,
            ],
        )?,
        password: pick_required(
            "clickhouse.password",
            [
                env.clickhouse_password,
                cli.clickhouse_password.clone(),
                file_clickhouse.password,
            ],
        )?,
        database: pick_required(
            "clickhouse.database",
            [
                env.clickhouse_database,
                cli.clickhouse_database.clone(),
                file_clickhouse.database,
            ],
        )?,
        app_database: pick_required(
            "clickhouse.app_database",
            [
                env.app_database,
                cli.app_database.clone(),
                file_clickhouse.app_database,
            ],
        )?,
    };

    Ok(AppConfig {
        socks_proxy: pick_first([Some(env.socks), Some(cli.proxy.clone())], || None),
        clickhouse,
        migrations_dir: pick_required(
            "migrations_dir",
            [
                env.migrations_dir,
                cli.migrations_dir.clone(),
                file.migrations_dir,
            ],
        )
        .unwrap_or_else(|_| DEFAULT_MIGRATIONS_DIR.to_string()),
        input_dir: pick_first(
            [env.input_dir, cli.input_dir.clone(), file.input_dir],
            || DEFAULT_INPUT_DIR.to_string(),
        ),
        archive_dir: pick_first(
            [env.archive_dir, cli.archive_dir.clone(), file.archive_dir],
            default_archive_dir,
        ),
        poll_interval_secs: pick_first_value(
            [
                env.poll_interval_secs,
                cli.poll_interval_secs,
                file.poll_interval_secs,
            ],
            DEFAULT_POLL_INTERVAL_SECS,
        ),
        telegram: TelegramConfig {
            token: pick_first(
                [
                    env.telegram_token,
                    cli.telegram_token.clone(),
                    file_telegram.token,
                ],
                String::new,
            ),
            results_dir: pick_first(
                [
                    env.results_dir,
                    cli.results_dir.clone(),
                    file_telegram.results_dir,
                ],
                || DEFAULT_RESULTS_DIR.to_string(),
            ),
            max_results: pick_first_value(
                [env.max_results, cli.max_results, file_telegram.max_results],
                50,
            ),
        },
    })
}

fn build_downloader_config(
    file: FileConfig,
    cli: &DownloaderCli,
    env: DownloaderEnv,
) -> Result<DownloaderConfig> {
    let file_downloader = file.downloader.unwrap_or_default();
    let inherited_archive_dir = file.archive_dir.clone();
    let peer_name = pick_required(
        "downloader.peer_name",
        [
            env.peer_name,
            cli.peer_name.clone(),
            file_downloader.peer_name.clone(),
        ],
    )?;
    let archive_dir = pick_first(
        [
            env.archive_dir,
            cli.archive_dir.clone(),
            file_downloader.archive_dir,
            inherited_archive_dir,
        ],
        default_archive_dir,
    );

    Ok(DownloaderConfig {
        socks_proxy: pick_first([Some(env.socks), Some(cli.proxy.clone())], || None),
        peer_name: peer_name.clone(),
        archive_dir,
        state_file: pick_first(
            [
                env.state_file,
                cli.state_file.clone(),
                file_downloader.state_file,
            ],
            || default_downloader_state_file(&peer_name),
        ),
        poll_interval_secs: pick_first_value(
            [
                env.poll_interval_secs,
                cli.poll_interval_secs,
                file_downloader.poll_interval_secs,
            ],
            DEFAULT_POLL_INTERVAL_SECS,
        ),
        session_file: pick_first(
            [
                env.session_file,
                cli.session_file.clone(),
                file_downloader.session_file,
            ],
            default_downloader_session_file,
        ),
        rest_listen_addr: pick_first(
            [
                env.rest_listen_addr,
                cli.rest_listen_addr.clone(),
                file_downloader.rest_listen_addr,
            ],
            || DEFAULT_DOWNLOADER_REST_LISTEN_ADDR.to_string(),
        ),
        api_id: pick_required(
            "downloader.api_id",
            [env.api_id, cli.api_id, file_downloader.api_id],
        )?,
        api_hash: pick_required(
            "downloader.api_hash",
            [env.api_hash, cli.api_hash.clone(), file_downloader.api_hash],
        )?,
    })
}

impl LogzzEnv {
    fn from_env() -> Self {
        Self {
            clickhouse_url: get_env_string("LOGZZ_CLICKHOUSE__URL"),
            clickhouse_user: get_env_string("LOGZZ_CLICKHOUSE__USER"),
            clickhouse_password: get_env_string("LOGZZ_CLICKHOUSE__PASSWORD"),
            clickhouse_database: get_env_string("LOGZZ_CLICKHOUSE__DATABASE"),
            app_database: get_env_string("LOGZZ_CLICKHOUSE__APP_DATABASE"),
            migrations_dir: get_env_string("LOGZZ_MIGRATIONS_DIR"),
            input_dir: get_env_string("LOGZZ_INPUT_DIR"),
            archive_dir: get_env_string("LOGZZ_ARCHIVE_DIR"),
            poll_interval_secs: get_env_parse("LOGZZ_POLL_INTERVAL_SECS"),
            telegram_token: get_env_string("LOGZZ_TELEGRAM__TOKEN"),
            results_dir: get_env_string("LOGZZ_TELEGRAM__RESULTS_DIR"),
            max_results: get_env_parse("LOGZZ_TELEGRAM__MAX_RESULTS"),
            socks: get_env_string("LOGZZ_TELEGRAM__SOCKS"),
        }
    }
}

impl DownloaderEnv {
    fn from_env() -> Self {
        Self {
            peer_name: first_env_string(&["DOWNLOADER_PEER_NAME"]),
            archive_dir: first_env_string(&["DOWNLOADER_ARCHIVE_DIR", "DOWNLOAD_FOLDER"]),
            state_file: first_env_string(&["DOWNLOADER_STATE_FILE", "DOWNLOAD_STATE_FILE"]),
            poll_interval_secs: first_env_parse(&[
                "DOWNLOADER_POLL_INTERVAL_SECS",
                "DOWNLOAD_POLL_INTERVAL_SECS",
            ]),
            session_file: first_env_string(&["DOWNLOADER_SESSION_FILE"]),
            rest_listen_addr: first_env_string(&["DOWNLOADER_REST_LISTEN_ADDR"]),
            api_id: first_env_parse(&["DOWNLOADER_API_ID", "TG_ID"]),
            api_hash: first_env_string(&["DOWNLOADER_API_HASH", "TG_HASH"]),
            socks: get_env_string("DOWNLOADER_TELEGRAM__SOCKS"),
        }
    }
}

fn default_downloader_state_file(peer_name: &str) -> String {
    format!(
        "{}/downloader-{}.state.json",
        DEFAULT_DOWNLOADER_DIR,
        sanitize_filename(peer_name)
    )
}

fn default_downloader_session_file() -> String {
    format!("{DEFAULT_DOWNLOADER_DIR}/downloader.session")
}

fn pick_required<T, const N: usize>(field: &str, values: [Option<T>; N]) -> Result<T> {
    values
        .into_iter()
        .flatten()
        .next()
        .ok_or_else(|| eyre!("missing required config field `{field}`"))
}

fn pick_first<T, F, const N: usize>(values: [Option<T>; N], default: F) -> T
where
    F: FnOnce() -> T,
{
    values.into_iter().flatten().next().unwrap_or_else(default)
}

fn pick_first_value<T: Copy, const N: usize>(values: [Option<T>; N], default: T) -> T {
    values.into_iter().flatten().next().unwrap_or(default)
}

fn get_env_string(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.is_empty())
}

fn get_env_parse<T>(key: &str) -> Option<T>
where
    T: std::str::FromStr,
{
    env::var(key).ok()?.parse().ok()
}

fn first_env_string(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| get_env_string(key))
}

fn first_env_parse<T>(keys: &[&str]) -> Option<T>
where
    T: std::str::FromStr,
{
    keys.iter().find_map(|key| get_env_parse(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_file_config() -> FileConfig {
        FileConfig {
            clickhouse: Some(PartialClickhouseConfig {
                url: Some("http://yaml:8123".to_string()),
                user: Some("yaml-user".to_string()),
                password: Some("yaml-pass".to_string()),
                database: Some("yaml-db".to_string()),
                app_database: Some("yaml-app-db".to_string()),
            }),
            migrations_dir: Some("./yaml-migrations".to_string()),
            input_dir: Some("./yaml-input".to_string()),
            archive_dir: Some("./yaml-archives".to_string()),
            poll_interval_secs: Some(11),
            telegram: Some(PartialTelegramConfig {
                token: Some("yaml-token".to_string()),
                results_dir: Some("./yaml-results".to_string()),
                max_results: Some(13),
            }),
            downloader: Some(PartialDownloaderConfig {
                peer_name: Some("yaml-peer".to_string()),
                archive_dir: Some("./yaml-downloader-archives".to_string()),
                state_file: Some("./yaml-state.json".to_string()),
                poll_interval_secs: Some(17),
                session_file: Some("./yaml.session".to_string()),
                rest_listen_addr: Some("127.0.0.1:19090".to_string()),
                api_id: Some(100),
                api_hash: Some("yaml-hash".to_string()),
            }),
        }
    }

    #[test]
    fn logzz_uses_yaml_then_cli_then_env_precedence() {
        let cli = Cli {
            config: DEFAULT_CONFIG_PATH.to_string(),
            clickhouse_url: Some("http://cli:8123".to_string()),
            clickhouse_user: None,
            clickhouse_password: None,
            clickhouse_database: None,
            app_database: None,
            migrations_dir: Some("./cli-migrations".to_string()),
            input_dir: None,
            archive_dir: Some("./cli-archives".to_string()),
            poll_interval_secs: Some(19),
            telegram_token: Some("cli-token".to_string()),
            results_dir: None,
            max_results: Some(23),
            proxy: None,
        };
        let env = LogzzEnv {
            clickhouse_url: Some("http://env:8123".to_string()),
            archive_dir: Some("./env-archives".to_string()),
            poll_interval_secs: Some(29),
            telegram_token: Some("env-token".to_string()),
            ..LogzzEnv::default()
        };

        let cfg = build_app_config(sample_file_config(), &cli, env).unwrap();

        assert_eq!(cfg.clickhouse.url, "http://env:8123");
        assert_eq!(cfg.clickhouse.user, "yaml-user");
        assert_eq!(cfg.migrations_dir, "./cli-migrations");
        assert_eq!(cfg.archive_dir, "./env-archives");
        assert_eq!(cfg.poll_interval_secs, 29);
        assert_eq!(cfg.telegram.token, "env-token");
        assert_eq!(cfg.telegram.results_dir, "./yaml-results");
        assert_eq!(cfg.telegram.max_results, 23);
    }

    #[test]
    fn downloader_inherits_archive_dir_and_uses_env_priority() {
        let cli = DownloaderCli {
            config: DEFAULT_CONFIG_PATH.to_string(),
            peer_name: Some("cli-peer".to_string()),
            archive_dir: Some("./cli-archives".to_string()),
            state_file: None,
            poll_interval_secs: Some(31),
            session_file: None,
            rest_listen_addr: Some("127.0.0.1:29090".to_string()),
            api_id: None,
            api_hash: None,
            proxy: None,
        };
        let env = DownloaderEnv {
            archive_dir: Some("./env-archives".to_string()),
            rest_listen_addr: Some("127.0.0.1:39090".to_string()),
            api_id: Some(200),
            api_hash: Some("env-hash".to_string()),
            ..DownloaderEnv::default()
        };

        let cfg = build_downloader_config(sample_file_config(), &cli, env).unwrap();

        assert_eq!(cfg.peer_name, "cli-peer");
        assert_eq!(cfg.archive_dir, "./env-archives");
        assert_eq!(cfg.poll_interval_secs, 31);
        assert_eq!(cfg.state_file, "./yaml-state.json");
        assert_eq!(cfg.session_file, "./yaml.session");
        assert_eq!(cfg.rest_listen_addr, "127.0.0.1:39090");
        assert_eq!(cfg.api_id, 200);
        assert_eq!(cfg.api_hash, "env-hash");
    }

    #[test]
    fn downloader_falls_back_to_shared_archive_dir() {
        let cli = DownloaderCli {
            config: DEFAULT_CONFIG_PATH.to_string(),
            peer_name: Some("peer".to_string()),
            archive_dir: None,
            state_file: None,
            poll_interval_secs: None,
            session_file: None,
            rest_listen_addr: None,
            api_id: Some(1),
            api_hash: Some("hash".to_string()),
            proxy: None,
        };
        let mut file = sample_file_config();
        file.downloader = Some(PartialDownloaderConfig {
            archive_dir: None,
            ..PartialDownloaderConfig::default()
        });

        let cfg = build_downloader_config(file, &cli, DownloaderEnv::default()).unwrap();

        assert_eq!(cfg.archive_dir, "./yaml-archives");
    }

    #[test]
    fn app_uses_runtime_defaults_for_paths() {
        let cfg = build_app_config(
            FileConfig {
                clickhouse: Some(PartialClickhouseConfig {
                    url: Some("http://yaml:8123".to_string()),
                    user: Some("yaml-user".to_string()),
                    password: Some("yaml-pass".to_string()),
                    database: Some("yaml-db".to_string()),
                    app_database: Some("yaml-app-db".to_string()),
                }),
                ..FileConfig::default()
            },
            &Cli {
                proxy: None,
                config: DEFAULT_CONFIG_PATH.to_string(),
                clickhouse_url: None,
                clickhouse_user: None,
                clickhouse_password: None,
                clickhouse_database: None,
                app_database: None,
                migrations_dir: None,
                input_dir: None,
                archive_dir: None,
                poll_interval_secs: None,
                telegram_token: None,
                results_dir: None,
                max_results: None,
            },
            LogzzEnv::default(),
        )
        .unwrap();

        assert_eq!(cfg.migrations_dir, DEFAULT_MIGRATIONS_DIR);
        assert_eq!(cfg.input_dir, DEFAULT_INPUT_DIR);
        assert_eq!(cfg.archive_dir, "./.local/archives");
        assert_eq!(cfg.telegram.results_dir, DEFAULT_RESULTS_DIR);
    }

    #[test]
    fn downloader_uses_runtime_defaults_for_state_files() {
        let cfg = build_downloader_config(
            FileConfig::default(),
            &DownloaderCli {
                config: DEFAULT_CONFIG_PATH.to_string(),
                peer_name: Some("peer/name".to_string()),
                archive_dir: None,
                state_file: None,
                poll_interval_secs: None,
                session_file: None,
                rest_listen_addr: None,
                api_id: Some(1),
                api_hash: Some("hash".to_string()),
                proxy: None,
            },
            DownloaderEnv::default(),
        )
        .unwrap();

        assert_eq!(cfg.archive_dir, "./.local/archives");
        assert_eq!(
            cfg.state_file,
            "./.local/downloader/downloader-peer_name.state.json"
        );
        assert_eq!(cfg.session_file, "./.local/downloader/downloader.session");
        assert_eq!(cfg.rest_listen_addr, "127.0.0.1:8090");
    }
}
