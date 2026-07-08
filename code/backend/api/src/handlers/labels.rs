use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ErrorResponse};
use crate::format::parse_address;
use crate::state::{AppState, resolve_chain_id};

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub struct LabelRequest {
    #[schema(example = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")]
    pub address: String,
    #[schema(example = 1)]
    pub chain_id: Option<u32>,
    #[schema(example = "exchange")]
    pub category: String,
    #[schema(example = "Binance 14")]
    pub label_name: Option<String>,
    #[schema(example = "https://etherscan.io/address/0xd8dA…")]
    pub label_url: Option<String>,
    #[schema(example = "manual")]
    pub label_source: Option<String>,
    #[schema(example = "ofac")]
    pub sanction_list: Option<String>,
    #[schema(example = 30)]
    pub risk_score: Option<u8>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct LabelResponse {
    entity_id: String,
    addresses: Vec<String>,
    category: String,
    sanction_list: Option<String>,
    label_name: Option<String>,
    label_url: Option<String>,
    label_source: Option<String>,
    risk_score: u8,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct LabelsBulkResponse {
    upserted: usize,
    errors: Vec<String>,
}

fn parse_category(
    s: &str,
    sanction_list: Option<&str>,
) -> Result<domain::entity::EntityCategory, ApiError> {
    use domain::entity::{EntityCategory, SanctionList};
    Ok(match s {
        "exchange" => EntityCategory::Exchange,
        "mixer" => EntityCategory::Mixer,
        "bridge" => EntityCategory::Bridge,
        "defi" => EntityCategory::DefiProtocol,
        "scam" => EntityCategory::Scam,
        "gambling" => EntityCategory::Gambling,
        "darknet" => EntityCategory::Darknet,
        "mining" => EntityCategory::Mining,
        "unknown" => EntityCategory::Unknown,
        "sanctioned" => {
            let sl = match sanction_list.unwrap_or("ofac") {
                "ofac" => SanctionList::Ofac,
                "eu" => SanctionList::Eu,
                "un" => SanctionList::Un,
                other => SanctionList::Other(other.to_string()),
            };
            EntityCategory::Sanctioned { sanction_list: sl }
        }
        other => return Err(ApiError::bad_request(format!("unknown category: {other}"))),
    })
}

fn parse_source(s: Option<&str>) -> domain::entity::LabelSource {
    use domain::entity::LabelSource;
    match s.unwrap_or("manual") {
        "chainalysis" => LabelSource::Chainalysis,
        "internal" => LabelSource::Internal,
        "community" => LabelSource::Community,
        _ => LabelSource::Manual,
    }
}

fn default_risk_for(cat: &domain::entity::EntityCategory) -> u8 {
    use domain::entity::EntityCategory;
    match cat {
        EntityCategory::Sanctioned { .. } => 100,
        EntityCategory::Darknet => 95,
        EntityCategory::Mixer => 90,
        EntityCategory::Scam => 75,
        EntityCategory::Bridge => 40,
        EntityCategory::Exchange => 30,
        _ => 25,
    }
}

fn entity_to_dto(e: &domain::entity::Entity) -> LabelResponse {
    use domain::entity::EntityCategory;
    let (cat_s, sl) = match e.category() {
        EntityCategory::Exchange => ("exchange".to_string(), None),
        EntityCategory::Mixer => ("mixer".to_string(), None),
        EntityCategory::Bridge => ("bridge".to_string(), None),
        EntityCategory::DefiProtocol => ("defi".to_string(), None),
        EntityCategory::Scam => ("scam".to_string(), None),
        EntityCategory::Gambling => ("gambling".to_string(), None),
        EntityCategory::Darknet => ("darknet".to_string(), None),
        EntityCategory::Mining => ("mining".to_string(), None),
        EntityCategory::Unknown => ("unknown".to_string(), None),
        EntityCategory::Sanctioned { sanction_list } => (
            "sanctioned".to_string(),
            Some(match sanction_list {
                domain::entity::SanctionList::Ofac => "ofac".to_string(),
                domain::entity::SanctionList::Eu => "eu".to_string(),
                domain::entity::SanctionList::Un => "un".to_string(),
                domain::entity::SanctionList::Other(s) => s.clone(),
            }),
        ),
    };
    LabelResponse {
        entity_id: e.id().value().to_string(),
        addresses: e.addresses().iter().map(|a| a.canonical()).collect(),
        category: cat_s,
        sanction_list: sl,
        label_name: e.label().map(|l| l.name().to_string()),
        label_url: e.label().and_then(|l| l.url()).map(str::to_owned),
        label_source: e.label().map(|l| match l.source() {
            domain::entity::LabelSource::Manual => "manual".to_string(),
            domain::entity::LabelSource::Chainalysis => "chainalysis".to_string(),
            domain::entity::LabelSource::Internal => "internal".to_string(),
            domain::entity::LabelSource::Community => "community".to_string(),
        }),
        risk_score: e.risk_score().value(),
    }
}

pub(crate) async fn apply_label(
    state: &AppState,
    body: &LabelRequest,
) -> Result<domain::entity::Entity, ApiError> {
    use domain::entity::{Entity, EntityLabel, RiskScore};
    use domain::ports::EntityRepository;

    let chain = resolve_chain_id(state, body.chain_id);
    let addr = parse_address(&body.address, chain)?;
    let category = parse_category(&body.category, body.sanction_list.as_deref())?;
    let risk = RiskScore::new(
        body.risk_score
            .unwrap_or_else(|| default_risk_for(&category)),
    );

    let mut entity = state
        .entities()
        .find_by_address(&addr)
        .await
        .map_err(ApiError::Internal)?;

    if entity.is_none()
        && let Some(name) = body.label_name.as_deref()
    {
        entity = state
            .entities()
            .find_by_label(&category, name)
            .await
            .map_err(ApiError::Internal)?;
    }

    let mut entity = entity.unwrap_or_else(|| Entity::new(category.clone(), risk));

    if let Some(name) = body.label_name.clone() {
        entity.set_label(EntityLabel::new(
            name,
            body.label_url.clone(),
            parse_source(body.label_source.as_deref()),
        ));
    }
    entity.add_address(addr);

    state
        .entities()
        .save(&entity)
        .await
        .map_err(ApiError::Internal)?;

    Ok(entity)
}

#[utoipa::path(
    post, path = "/labels",
    description = "Tag an address with a category + label (admin only).\n\n\
                   ## What this does\n\n\
                   Resolves the target entity using the following priority:\n\n\
                   1. If the address is already attached to an entity, reuse that entity.\n\
                   2. Else, if `label_name` matches an existing `(category, label_name)` pair, \
                      append the address there.\n\
                   3. Else, create a brand-new entity with the supplied category + label.\n\n\
                   The resulting entity is then persisted with the address attached. This is \
                   how you build up multi-address entities (e.g. all of an exchange's hot \
                   wallets) by repeatedly labelling individual addresses with the same \
                   `label_name`.\n\n\
                   Writes go to PostgreSQL. The label is immediately visible to `/score`, \
                   `/sanctions`, and the trace-sink classifier.\n\n\
                   ## When to use it\n\n\
                   Use this to inject your own attribution data — internal investigations, \
                   curated allow-lists, or imports from third-party providers. For batch \
                   imports, prefer `POST /labels/bulk`.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl -X POST 'http://localhost:8080/labels' \\\n\
                     -H 'X-API-Version: 1' \\\n\
                     -H 'X-Admin-Api-Key: <admin-key>' \\\n\
                     -H 'Content-Type: application/json' \\\n\
                     -d '{\n\
                       \"address\": \"0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045\",\n\
                       \"chain_id\": 1,\n\
                       \"category\": \"exchange\",\n\
                       \"label_name\": \"Binance hot wallet 14\",\n\
                       \"label_source\": \"manual\"\n\
                     }'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   Requires the `X-Admin-Api-Key` header. `risk_score` is auto-defaulted from \
                   `category` when omitted — explicit overrides only matter for fine-tuning.",
    request_body = LabelRequest,
    responses(
        (status = 200, body = LabelResponse),
        (status = 400, body = ErrorResponse),
        (status = 401, body = ErrorResponse),
    ),
    tag = "Labels"
)]
pub async fn labels_set(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LabelRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let entity = apply_label(&state, &body).await?;
    Ok(Json(entity_to_dto(&entity)))
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PathAddrQuery {
    #[param(example = 1)]
    pub chain_id: Option<u32>,
}

#[utoipa::path(
    get, path = "/labels/{addr}",
    description = "Look up the entity attached to an address (admin only).\n\n\
                   ## What this does\n\n\
                   Returns the entity that contains the supplied address, including every sibling \
                   address attached to that same entity. Useful for discovering related wallets — \
                   labelling one of an exchange's hot wallets exposes the full set.\n\n\
                   This is a DB-only read. No chain RPC is touched.\n\n\
                   ## When to use it\n\n\
                   Call this to render the label / category / risk score for an address in a UI, \
                   or to expand a known address into the wider set of co-labelled siblings.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl 'http://localhost:8080/labels/0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045?chain_id=1' \\\n\
                     -H 'X-API-Version: 1' \\\n\
                     -H 'X-Admin-Api-Key: <admin-key>'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   Requires the `X-Admin-Api-Key` header. `chain_id` defaults to 1 (Ethereum \
                   mainnet); pass it explicitly for other chains so the path address is parsed \
                   with the right encoding.",
    params(
        ("addr" = String, Path, description = "Address in hex (EVM) or canonical chain form."),
        PathAddrQuery,
    ),
    responses(
        (status = 200, body = LabelResponse),
        (status = 400, body = ErrorResponse),
        (status = 401, body = ErrorResponse),
    ),
    tag = "Labels"
)]
pub async fn labels_get(
    State(state): State<Arc<AppState>>,
    Path(addr_param): Path<String>,
    Query(q): Query<PathAddrQuery>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::ports::EntityRepository;
    let chain = resolve_chain_id(&state, q.chain_id);
    let addr = parse_address(&addr_param, chain)?;
    let entity = state
        .entities()
        .find_by_address(&addr)
        .await
        .map_err(ApiError::Internal)?
        .ok_or_else(|| ApiError::bad_request("no label for this address".to_string()))?;
    Ok(Json(entity_to_dto(&entity)))
}

#[utoipa::path(
    delete, path = "/labels/{addr}",
    description = "Detach an address from its entity (admin only).\n\n\
                   ## What this does\n\n\
                   Removes the address from whatever entity currently holds it. The entity \
                   itself is preserved (with its remaining addresses) so other addresses in the \
                   same entity keep their label. If the address has no label attached, the call \
                   is a no-op.\n\n\
                   ## When to use it\n\n\
                   Use this when a labelling decision turns out to be wrong, or when an address \
                   is decommissioned. To delete an entity entirely, remove every address one by \
                   one — there is no separate \"delete entity\" endpoint.\n\n\
                   ## Example\n\n\
                   ```bash\n\
                   curl -X DELETE 'http://localhost:8080/labels/0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045?chain_id=1' \\\n\
                     -H 'X-API-Version: 1' \\\n\
                     -H 'X-Admin-Api-Key: <admin-key>'\n\
                   ```\n\n\
                   ## Notes\n\n\
                   Requires the `X-Admin-Api-Key` header. `chain_id` defaults to 1 (Ethereum \
                   mainnet); pass it explicitly for other chains.",
    params(
        ("addr" = String, Path, description = "Address in hex (EVM) or canonical chain form."),
        PathAddrQuery,
    ),
    responses(
        (status = 200),
        (status = 400, body = ErrorResponse),
        (status = 401, body = ErrorResponse),
    ),
    tag = "Labels"
)]
pub async fn labels_delete(
    State(state): State<Arc<AppState>>,
    Path(addr_param): Path<String>,
    Query(q): Query<PathAddrQuery>,
) -> Result<impl IntoResponse, ApiError> {
    use domain::entity::Entity;
    use domain::ports::EntityRepository;
    let chain = resolve_chain_id(&state, q.chain_id);
    let addr = parse_address(&addr_param, chain)?;
    let Some(entity) = state
        .entities()
        .find_by_address(&addr)
        .await
        .map_err(ApiError::Internal)?
    else {
        return Ok(Json(serde_json::json!({ "removed": false })));
    };

    let mut addresses = entity.addresses().clone();
    addresses.remove(&addr);
    let mut updated = Entity::from_parts(
        entity.id().clone(),
        entity.label().cloned(),
        entity.category().clone(),
        addresses,
        entity.risk_score(),
    );
    if let Some(label) = entity.label() {
        updated.set_label(label.clone());
    }
    state
        .entities()
        .save(&updated)
        .await
        .map_err(ApiError::Internal)?;
    Ok(Json(serde_json::json!({
        "removed": true,
        "entity_id": entity.id().value().to_string(),
        "remaining": updated.addresses().len(),
    })))
}

#[utoipa::path(
    post, path = "/labels/bulk",
    request_body = [LabelRequest],
    responses(
        (status = 200, body = LabelsBulkResponse),
    ),
    tag = "Labels"
)]
pub async fn labels_bulk(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Vec<LabelRequest>>,
) -> Result<impl IntoResponse, ApiError> {
    let mut upserted = 0usize;
    let mut errors: Vec<String> = Vec::new();
    for req in body {
        match apply_label(&state, &req).await {
            Ok(_) => upserted += 1,
            Err(e) => {
                let msg = match e {
                    ApiError::BadRequest(s) => s,
                    ApiError::Unauthorized => "unauthorized".into(),
                    ApiError::Internal(de) => de.to_string(),
                    ApiError::InternalMsg(s) => s,
                };
                errors.push(format!("{}: {msg}", req.address));
            }
        }
    }
    Ok(Json(LabelsBulkResponse { upserted, errors }))
}
