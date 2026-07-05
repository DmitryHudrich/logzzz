use tower_http::cors::{Any, CorsLayer};

use crate::config::{
    AppConfig, ChainConfigFile, ChainSourceSpec, CorsConfig, EthSourceKind, TelemetryConfig,
};
use domain::chain::ChainId;
use infra::fetch_wallet_api::MoralisEvmSource;
use infra::{
    AlchemyEvmSource, BigQueryEvmSource, ChainSources, ChainSourcesBuilder, EtherscanEvmSource,
    RoutedChains, RoutedEvmSource, TronGridSource,
};

pub async fn build_sources_from_chains(
    cfg: &AppConfig,
    http_client: &reqwest::Client,
) -> anyhow::Result<ChainSources> {
    let mut sources = ChainSources::builder();
    let mut seen_ids: std::collections::HashSet<u32> = std::collections::HashSet::new();

    for entry in cfg.chains() {
        if !seen_ids.insert(entry.id) {
            anyhow::bail!(
                "chains: duplicate chain id {} — every entry must have a unique id",
                entry.id
            );
        }
        let chain = ChainId::new(entry.id);
        let name = entry.name.as_deref().unwrap_or("");
        register_chain_entry(&mut sources, entry, chain, name, cfg, http_client).await?;
    }

    Ok(sources.build())
}

async fn register_chain_entry(
    sources: &mut ChainSourcesBuilder,
    entry: &ChainConfigFile,
    chain: ChainId,
    name: &str,
    cfg: &AppConfig,
    http_client: &reqwest::Client,
) -> anyhow::Result<()> {
    match &entry.source {
        ChainSourceSpec::Etherscan { base_url } => {
            let es_cfg = cfg.etherscan().cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "chains[id={}]: source=etherscan but `etherscan` section is missing",
                    chain.value()
                )
            })?;
            if !es_cfg.has_any_key() {
                anyhow::bail!(
                    "chains[id={}]: source=etherscan but etherscan has no api keys",
                    chain.value()
                );
            }
            let mut domain_cfg = es_cfg.into_domain();
            if let Some(url) = base_url {
                domain_cfg = domain_cfg.with_base_url(url.clone());
            }
            let src = EtherscanEvmSource::new(chain, http_client.clone(), domain_cfg).await;
            *sources = std::mem::take(sources).register(src);
            tracing::info!(
                chain = chain.value(),
                name,
                source = "etherscan",
                "registered chain source"
            );
        }
        ChainSourceSpec::Alchemy { base_url } => {
            let al_cfg = cfg.alchemy().cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "chains[id={}]: source=alchemy but `alchemy` section is missing",
                    chain.value()
                )
            })?;
            if !al_cfg.has_any_key() {
                anyhow::bail!(
                    "chains[id={}]: source=alchemy but alchemy has no api keys",
                    chain.value()
                );
            }
            let mut domain_cfg = al_cfg.into_domain();
            if let Some(url) = base_url {
                domain_cfg = domain_cfg.with_base_url(url.clone());
            }
            let src = AlchemyEvmSource::new(chain, http_client.clone(), domain_cfg);
            *sources = std::mem::take(sources).register(src);
            tracing::info!(
                chain = chain.value(),
                name,
                source = "alchemy",
                "registered chain source"
            );
        }
        ChainSourceSpec::Moralis => {
            if !cfg.moralis().has_any_key() {
                anyhow::bail!(
                    "chains[id={}]: source=moralis but moralis has no api keys",
                    chain.value()
                );
            }
            let src = MoralisEvmSource::new(
                chain,
                http_client.clone(),
                cfg.moralis().clone().into_domain(),
            )
            .await;
            *sources = std::mem::take(sources).register(src);
            tracing::info!(
                chain = chain.value(),
                name,
                source = "moralis",
                "registered chain source"
            );
        }
        ChainSourceSpec::Bigquery {
            transactions_table,
            token_transfers_table,
        } => {
            let bq_cfg = cfg.bigquery().cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "chains[id={}]: source=bigquery but `bigquery` section is missing",
                    chain.value()
                )
            })?;
            let mut domain_cfg = bq_cfg.into_domain();
            if let Some(t) = transactions_table {
                domain_cfg = domain_cfg.with_transactions_table(t.clone());
            }
            if let Some(t) = token_transfers_table {
                domain_cfg = domain_cfg.with_token_transfers_table(Some(t.clone()));
            }
            let src = BigQueryEvmSource::new(chain, http_client.clone(), domain_cfg).await?;
            *sources = std::mem::take(sources).register(src);
            tracing::info!(
                chain = chain.value(),
                name,
                source = "bigquery",
                "registered chain source"
            );
        }
        ChainSourceSpec::Trongrid { base_url } => {
            let mut tron_cfg = cfg.trongrid().clone().into_domain();
            if let Some(url) = base_url {
                tron_cfg = tron_cfg.with_base_url(url.clone());
            }
            let src = TronGridSource::new(http_client.clone(), tron_cfg);
            *sources = std::mem::take(sources).register(src);
            tracing::info!(
                chain = chain.value(),
                name,
                source = "trongrid",
                "registered chain source"
            );
        }
        ChainSourceSpec::Routed {
            transfers,
            is_contract,
            latest_block,
            fetch_block,
            source_cooldown_secs,
        } => {
            let cooldown = std::time::Duration::from_secs(source_cooldown_secs.unwrap_or(10));
            let mut builder = RoutedEvmSource::builder(chain)
                .source_cooldown(cooldown)
                .chains(RoutedChains {
                    transfers: transfers.clone(),
                    is_contract: is_contract.clone(),
                    latest_block: latest_block.clone(),
                    fetch_block: fetch_block.clone(),
                });
            let referenced = entry.source.referenced_providers();
            for provider in &referenced {
                match provider.as_str() {
                    "etherscan" => {
                        let es_cfg = cfg.etherscan().cloned().ok_or_else(|| {
                            anyhow::anyhow!(
                                "chains[id={}]: routed source references 'etherscan' but the `etherscan` section is missing",
                                chain.value()
                            )
                        })?;
                        if !es_cfg.has_any_key() {
                            anyhow::bail!(
                                "chains[id={}]: routed references 'etherscan' but etherscan has no api keys",
                                chain.value()
                            );
                        }
                        let src = EtherscanEvmSource::new(
                            chain,
                            http_client.clone(),
                            es_cfg.into_domain(),
                        )
                        .await;
                        builder = builder.register("etherscan", src);
                    }
                    "alchemy" => {
                        let al_cfg = cfg.alchemy().cloned().ok_or_else(|| {
                            anyhow::anyhow!(
                                "chains[id={}]: routed source references 'alchemy' but the `alchemy` section is missing",
                                chain.value()
                            )
                        })?;
                        if !al_cfg.has_any_key() {
                            anyhow::bail!(
                                "chains[id={}]: routed references 'alchemy' but alchemy has no api keys",
                                chain.value()
                            );
                        }
                        let src = AlchemyEvmSource::new(
                            chain,
                            http_client.clone(),
                            al_cfg.into_domain(),
                        );
                        builder = builder.register("alchemy", src);
                    }
                    "bigquery" => {
                        let bq_cfg = cfg.bigquery().cloned().ok_or_else(|| {
                            anyhow::anyhow!(
                                "chains[id={}]: routed source references 'bigquery' but the `bigquery` section is missing",
                                chain.value()
                            )
                        })?;
                        let src = BigQueryEvmSource::new(
                            chain,
                            http_client.clone(),
                            bq_cfg.into_domain(),
                        )
                        .await?;
                        builder = builder.register("bigquery", src);
                    }
                    "moralis" => {
                        if !cfg.moralis().has_any_key() {
                            anyhow::bail!(
                                "chains[id={}]: routed references 'moralis' but moralis has no api keys",
                                chain.value()
                            );
                        }
                        let src = MoralisEvmSource::new(
                            chain,
                            http_client.clone(),
                            cfg.moralis().clone().into_domain(),
                        )
                        .await;
                        builder = builder.register("moralis", src);
                    }
                    other => anyhow::bail!(
                        "chains[id={}]: routed references unknown provider '{other}' — \
                         supported: etherscan, alchemy, bigquery, moralis",
                        chain.value()
                    ),
                }
            }
            let router = builder.build()?;
            *sources = std::mem::take(sources).register(router);
            tracing::info!(
                chain = chain.value(),
                name,
                source = "routed",
                providers = ?referenced,
                "registered chain source"
            );
        }
    }
    Ok(())
}

pub async fn build_sources_legacy(
    cfg: &AppConfig,
    http_client: &reqwest::Client,
) -> anyhow::Result<ChainSources> {
    let mut sources = ChainSources::builder();

    match cfg.ethereum().source() {
        EthSourceKind::Moralis => {
            if cfg.moralis().has_any_key() {
                let eth_source = MoralisEvmSource::new(
                    ChainId::ETH,
                    http_client.clone(),
                    cfg.moralis().clone().into_domain(),
                )
                .await;
                sources = sources.register(eth_source);
                tracing::info!(chain = "eth", source = "moralis", "registered ETH source");
            } else {
                tracing::warn!(
                    "moralis: no api keys (api_key / api_keys empty) — Ethereum chain disabled"
                );
            }
        }
        EthSourceKind::Bigquery => {
            let bq_cfg = cfg.bigquery().cloned().ok_or_else(|| {
                anyhow::anyhow!("ethereum.source=bigquery but `bigquery` section is missing in config")
            })?;
            let eth_source = BigQueryEvmSource::new(
                ChainId::ETH,
                http_client.clone(),
                bq_cfg.into_domain(),
            )
            .await?;
            sources = sources.register(eth_source);
            tracing::info!(chain = "eth", source = "bigquery", "registered ETH source");
        }
        EthSourceKind::Etherscan => {
            let es_cfg = cfg.etherscan().cloned().ok_or_else(|| {
                anyhow::anyhow!("ethereum.source=etherscan but `etherscan` section is missing in config")
            })?;
            if !es_cfg.has_any_key() {
                anyhow::bail!(
                    "ethereum.source=etherscan but no api keys configured — \
                     set etherscan.api_key, etherscan.api_keys, or pass --etherscan-api-key"
                );
            }
            let eth_source = EtherscanEvmSource::new(
                ChainId::ETH,
                http_client.clone(),
                es_cfg.into_domain(),
            )
            .await;
            sources = sources.register(eth_source);
            tracing::info!(chain = "eth", source = "etherscan", "registered ETH source");
        }
        EthSourceKind::Routed => {
            let routed_cfg = cfg.routed().cloned().ok_or_else(|| {
                anyhow::anyhow!("ethereum.source=routed but `routed` section is missing in config")
            })?;
            let referenced = routed_cfg.referenced_sources();
            let mut builder = RoutedEvmSource::builder(ChainId::ETH)
                .source_cooldown(routed_cfg.source_cooldown())
                .chains(RoutedChains {
                    transfers: routed_cfg.transfers.clone(),
                    is_contract: routed_cfg.is_contract.clone(),
                    latest_block: routed_cfg.latest_block.clone(),
                    fetch_block: routed_cfg.fetch_block.clone(),
                });
            for name in &referenced {
                match name.as_str() {
                    "etherscan" => {
                        let es_cfg = cfg.etherscan().cloned().ok_or_else(|| {
                            anyhow::anyhow!(
                                "routed chain references 'etherscan' but the `etherscan` section is missing"
                            )
                        })?;
                        if !es_cfg.has_any_key() {
                            anyhow::bail!(
                                "routed.* references 'etherscan' but etherscan has no api keys"
                            );
                        }
                        let src = EtherscanEvmSource::new(
                            ChainId::ETH,
                            http_client.clone(),
                            es_cfg.into_domain(),
                        )
                        .await;
                        builder = builder.register("etherscan", src);
                    }
                    "alchemy" => {
                        let al_cfg = cfg.alchemy().cloned().ok_or_else(|| {
                            anyhow::anyhow!(
                                "routed chain references 'alchemy' but the `alchemy` section is missing"
                            )
                        })?;
                        if !al_cfg.has_any_key() {
                            anyhow::bail!(
                                "routed.* references 'alchemy' but alchemy has no api keys — \
                                 set alchemy.api_key / alchemy.api_keys or pass --alchemy-api-key"
                            );
                        }
                        let src = AlchemyEvmSource::new(
                            ChainId::ETH,
                            http_client.clone(),
                            al_cfg.into_domain(),
                        );
                        builder = builder.register("alchemy", src);
                    }
                    "bigquery" => {
                        let bq_cfg = cfg.bigquery().cloned().ok_or_else(|| {
                            anyhow::anyhow!(
                                "routed chain references 'bigquery' but the `bigquery` section is missing"
                            )
                        })?;
                        let src = BigQueryEvmSource::new(
                            ChainId::ETH,
                            http_client.clone(),
                            bq_cfg.into_domain(),
                        )
                        .await?;
                        builder = builder.register("bigquery", src);
                    }
                    "moralis" => {
                        if !cfg.moralis().has_any_key() {
                            anyhow::bail!(
                                "routed.* references 'moralis' but moralis has no api keys — \
                                 set moralis.api_key / moralis.api_keys or pass --moralis-api-key"
                            );
                        }
                        let src = MoralisEvmSource::new(
                            ChainId::ETH,
                            http_client.clone(),
                            cfg.moralis().clone().into_domain(),
                        )
                        .await;
                        builder = builder.register("moralis", src);
                    }
                    other => anyhow::bail!(
                        "routed chain references unknown source '{other}' — \
                         supported: etherscan, alchemy, bigquery, moralis"
                    ),
                }
            }
            let router = builder.build()?;
            sources = sources.register(router);
            tracing::info!(
                chain = "eth",
                source = "routed",
                inner = ?referenced,
                "registered ETH source"
            );
        }
    }

    if cfg.trongrid().enabled() {
        let tron_source =
            TronGridSource::new(http_client.clone(), cfg.trongrid().clone().into_domain());
        sources = sources.register(tron_source);
        tracing::info!(chain = "tron", "registered TronGrid source");
    }

    Ok(sources.build())
}

pub fn init_tracer(
    cfg: &TelemetryConfig,
) -> anyhow::Result<opentelemetry_sdk::trace::SdkTracerProvider> {
    use opentelemetry::KeyValue;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::Resource;
    use opentelemetry_sdk::trace::{BatchConfigBuilder, BatchSpanProcessor, SdkTracerProvider};

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(cfg.otlp_endpoint())
        .build()?;

    let resource = Resource::builder_empty()
        .with_attribute(KeyValue::new(
            "service.name",
            cfg.service_name().to_string(),
        ))
        .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
        .build();

    let processor = BatchSpanProcessor::builder(exporter)
        .with_batch_config(
            BatchConfigBuilder::default()
                .with_scheduled_delay(std::time::Duration::from_secs(1))
                .build(),
        )
        .build();

    let provider = SdkTracerProvider::builder()
        .with_span_processor(processor)
        .with_resource(resource)
        .build();

    opentelemetry::global::set_tracer_provider(provider.clone());
    Ok(provider)
}

pub fn build_cors_layer(cfg: &CorsConfig) -> anyhow::Result<CorsLayer> {
    use anyhow::Context;
    use axum::http::{HeaderName, HeaderValue, Method};

    let is_star = |v: &[String]| v.is_empty() || v.iter().any(|s| s == "*");
    let creds = cfg.allow_credentials();

    let mut layer = CorsLayer::new();

    if is_star(cfg.allow_origins()) {
        anyhow::ensure!(
            !creds,
            "cors: allow_credentials=true is incompatible with allow_origins=[\"*\"]"
        );
        layer = layer.allow_origin(Any);
    } else {
        let origins = cfg
            .allow_origins()
            .iter()
            .map(|s| HeaderValue::from_str(s))
            .collect::<Result<Vec<_>, _>>()
            .context("cors.allow_origins: invalid origin")?;
        layer = layer.allow_origin(origins);
    }

    if is_star(cfg.allow_methods()) {
        anyhow::ensure!(
            !creds,
            "cors: allow_credentials=true is incompatible with allow_methods=[\"*\"]"
        );
        layer = layer.allow_methods(Any);
    } else {
        let methods = cfg
            .allow_methods()
            .iter()
            .map(|s| Method::from_bytes(s.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .context("cors.allow_methods: invalid method")?;
        layer = layer.allow_methods(methods);
    }

    if is_star(cfg.allow_headers()) {
        anyhow::ensure!(
            !creds,
            "cors: allow_credentials=true is incompatible with allow_headers=[\"*\"]"
        );
        layer = layer.allow_headers(Any);
    } else {
        let headers = cfg
            .allow_headers()
            .iter()
            .map(|s| HeaderName::from_bytes(s.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .context("cors.allow_headers: invalid header name")?;
        layer = layer.allow_headers(headers);
    }

    if !cfg.expose_headers().is_empty() {
        let expose = cfg
            .expose_headers()
            .iter()
            .map(|s| HeaderName::from_bytes(s.as_bytes()))
            .collect::<Result<Vec<_>, _>>()
            .context("cors.expose_headers: invalid header name")?;
        layer = layer.expose_headers(expose);
    }

    if creds {
        layer = layer.allow_credentials(true);
    }

    if let Some(age) = cfg.max_age() {
        layer = layer.max_age(age);
    }

    Ok(layer)
}
