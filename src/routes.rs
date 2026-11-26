use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{IntoParams, Modify, OpenApi, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::arena::{
    CodingLeaderboard, CodingLeaderboardQuery, CodingRunInput, DecideTrialInput, Leaderboard,
    LeaderboardQuery, OverrideCandidateInput, OverrideCandidateResponse, OverrideCodingRunInput,
    OverrideCodingRunResponse, PanelSelection, RecordRunInput, RecordedCodingRunOutput,
    RecordedRunOutput, RecordedShadowBoutOutput, ReplaceRosterRequest, RosterListResponse,
    SelectPanelInput, ShadowBoutInput, ShadowLeaderboard, ShadowLeaderboardQuery, StartTrialInput,
    Trial, TrialsListResponse, decide_trial, get_coding_leaderboard, get_leaderboard,
    get_shadow_leaderboard, list_roster, list_trials, override_candidate, override_coding_run,
    record_coding_run, record_run, record_shadow_bout, replace_roster, select_panel, start_trial,
};
use crate::auth::{
    ApiToken, AuthenticatedUser, CreatedApiToken, SESSION_COOKIE_NAME, SessionUser, User,
    build_clear_session_cookie, build_session_cookie, create_session, extract_session_cookie,
    hash_password, normalize_and_validate_email, normalize_display_name, random_hex_secret,
    register_user_with_session, security_middleware, sha256_bytes, validate_password,
    verify_password,
};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub secure_cookies: bool,
    pub allow_open_registration: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct ErrorBody {
    pub error: String,
}

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }

    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: msg.into(),
        }
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: msg.into(),
        }
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: msg.into(),
        }
    }

    pub fn too_many_requests(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: msg.into(),
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HealthStatus {
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub registration_open: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateTokenRequest {
    pub name: String,
}

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "sessionCookie",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(SESSION_COOKIE_NAME))),
        );
        components.add_security_scheme(
            "bearerToken",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("pat_<uuid>_<secret>")
                    .build(),
            ),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Model Arena API",
        version = "0.1.0",
        description = "Self-hosted homelab web portal and MCP backend."
    ),
    modifiers(&SecurityAddon),
    tags(
        (name = "system", description = "Health and bootstrap metadata"),
        (name = "auth", description = "Email + password household authentication"),
        (name = "users", description = "Household member directory"),
        (name = "tokens", description = "Personal access tokens for local MCP clients"),
        (name = "arena", description = "Model Arena rosters, challenger trials, and subagent panel selection")
    )
)]
pub struct ApiDoc;

pub fn build_openapi_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(healthz))
        .routes(routes!(get_app_info))
        .routes(routes!(register))
        .routes(routes!(login))
        .routes(routes!(logout))
        .routes(routes!(me))
        .routes(routes!(list_users))
        .routes(routes!(list_tokens, create_token))
        .routes(routes!(delete_token))
        .routes(routes!(list_roster_handler))
        .routes(routes!(replace_roster_handler))
        .routes(routes!(list_trials_handler, start_trial_handler))
        .routes(routes!(decide_trial_handler))
        .routes(routes!(select_panel_handler))
        .routes(routes!(record_run_handler))
        .routes(routes!(override_candidate_handler))
        .routes(routes!(get_leaderboard_handler))
        .routes(routes!(record_coding_run_handler))
        .routes(routes!(override_coding_run_handler))
        .routes(routes!(get_coding_leaderboard_handler))
        .routes(routes!(record_shadow_bout_handler))
        .routes(routes!(get_shadow_leaderboard_handler))
}

pub fn openapi_spec() -> utoipa::openapi::OpenApi {
    let (_, spec) = build_openapi_router().split_for_parts();
    spec
}

pub fn build_router(state: AppState) -> Router {
    let (router, _) = build_openapi_router().split_for_parts();
    router
        .layer(middleware::from_fn(security_middleware))
        .with_state(state)
}

#[utoipa::path(
    get,
    path = "/healthz",
    tag = "system",
    responses(
        (status = 200, description = "Server and database are healthy", body = HealthStatus),
        (status = 503, description = "Database is unreachable", body = HealthStatus)
    )
)]
async fn healthz(State(state): State<AppState>) -> Response {
    let check = tokio::time::timeout(
        Duration::from_secs(2),
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&state.pool),
    )
    .await;

    match check {
        Ok(Ok(1)) => (
            StatusCode::OK,
            Json(HealthStatus {
                status: "ok".to_string(),
            }),
        )
            .into_response(),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthStatus {
                status: "unavailable".to_string(),
            }),
        )
            .into_response(),
    }
}

async fn any_user_exists(pool: &PgPool) -> Result<bool, ApiError> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users)")
        .fetch_one(pool)
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to check existing users");
            ApiError::internal("failed to load app state")
        })
}

#[utoipa::path(
    get,
    path = "/api/app",
    tag = "system",
    responses(
        (status = 200, description = "Application bootstrap settings", body = AppInfo),
        (status = 500, description = "Internal error", body = ErrorBody)
    )
)]
async fn get_app_info(State(state): State<AppState>) -> Result<Json<AppInfo>, ApiError> {
    let registration_open = if state.allow_open_registration {
        true
    } else {
        !any_user_exists(&state.pool).await?
    };

    Ok(Json(AppInfo { registration_open }))
}

#[utoipa::path(
    post,
    path = "/api/auth/register",
    tag = "auth",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "Account created and signed in", body = User),
        (status = 400, description = "Invalid email, password, or name", body = ErrorBody),
        (status = 403, description = "Registration is closed", body = ErrorBody),
        (status = 409, description = "Email already registered", body = ErrorBody),
        (status = 429, description = "Authentication queue busy", body = ErrorBody)
    )
)]
async fn register(
    State(state): State<AppState>,
    Json(payload): Json<RegisterRequest>,
) -> Result<Response, ApiError> {
    let email = normalize_and_validate_email(&payload.email)?;
    validate_password(&payload.password)?;
    let name = normalize_display_name(payload.name.as_deref(), &email)?;

    // Reject closed registration before scheduling Argon2 work; the advisory-locked
    // check inside `register_user_with_session` remains authoritative for first-user races.
    if !state.allow_open_registration && any_user_exists(&state.pool).await? {
        return Err(ApiError::forbidden("registration is closed"));
    }

    let password_hash = hash_password(payload.password).await?;
    let (user, raw_session) = register_user_with_session(
        &state.pool,
        state.allow_open_registration,
        &email,
        &password_hash,
        &name,
    )
    .await?;

    let mut headers = HeaderMap::new();
    headers.insert(
        SET_COOKIE,
        build_session_cookie(&raw_session, state.secure_cookies),
    );
    Ok((StatusCode::CREATED, headers, Json(user)).into_response())
}

#[derive(sqlx::FromRow)]
struct LoginUserRow {
    id: Uuid,
    email: String,
    name: String,
    created_at: chrono::DateTime<chrono::Utc>,
    password_hash: String,
}

#[utoipa::path(
    post,
    path = "/api/auth/login",
    tag = "auth",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Signed in", body = User),
        (status = 400, description = "Malformed credentials", body = ErrorBody),
        (status = 401, description = "Invalid email or password", body = ErrorBody),
        (status = 429, description = "Authentication queue busy", body = ErrorBody)
    )
)]
async fn login(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let email = normalize_and_validate_email(&payload.email)?;
    if payload.password.is_empty() || payload.password.len() > 128 {
        // Still run constant-time dummy hash verification so password length
        // boundaries don't leak account existence.
        let _ = verify_password("invalid-length-probe".to_string(), None).await;
        return Err(ApiError::unauthorized("invalid email or password"));
    }

    let row = sqlx::query_as::<_, LoginUserRow>(
        "SELECT id, email, name, created_at, password_hash
         FROM users
         WHERE lower(email) = $1",
    )
    .bind(&email)
    .fetch_optional(&state.pool)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to query user during login");
        ApiError::internal("failed to verify credentials")
    })?;

    let stored_hash = row.as_ref().map(|r| r.password_hash.clone());
    let valid = verify_password(payload.password, stored_hash).await?;
    let Some(row) = row.filter(|_| valid) else {
        return Err(ApiError::unauthorized("invalid email or password"));
    };

    let raw_session = create_session(&state.pool, row.id).await?;
    let user = User {
        id: row.id,
        email: row.email,
        name: row.name,
        created_at: row.created_at,
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        SET_COOKIE,
        build_session_cookie(&raw_session, state.secure_cookies),
    );
    Ok((StatusCode::OK, headers, Json(user)).into_response())
}

#[utoipa::path(
    post,
    path = "/api/auth/logout",
    tag = "auth",
    responses(
        (status = 204, description = "Signed out"),
        (status = 403, description = "Cross-origin or non-browser request rejected", body = ErrorBody),
        (status = 500, description = "Internal error", body = ErrorBody)
    )
)]
async fn logout(
    State(state): State<AppState>,
    req_headers: HeaderMap,
) -> Result<Response, ApiError> {
    if req_headers.contains_key(AUTHORIZATION) {
        return Err(ApiError::forbidden(
            "this endpoint requires a browser session",
        ));
    }

    if req_headers.contains_key(COOKIE)
        && let Some(raw_token) = extract_session_cookie(&req_headers)
    {
        let token_hash = sha256_bytes(&raw_token);
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1")
            .bind(token_hash)
            .execute(&state.pool)
            .await
            .map_err(|err| {
                tracing::error!(?err, "failed to delete session on logout");
                ApiError::internal("failed to sign out")
            })?;
    }

    let mut headers = HeaderMap::new();
    headers.insert(SET_COOKIE, build_clear_session_cookie(state.secure_cookies));
    Ok((StatusCode::NO_CONTENT, headers).into_response())
}

#[utoipa::path(
    get,
    path = "/api/auth/me",
    tag = "auth",
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Current authenticated household user", body = User),
        (status = 401, description = "Not signed in", body = ErrorBody)
    )
)]
async fn me(AuthenticatedUser(user): AuthenticatedUser) -> Json<User> {
    Json(user)
}

#[utoipa::path(
    get,
    path = "/api/users",
    tag = "users",
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Household members in this workspace", body = [User]),
        (status = 401, description = "Not signed in", body = ErrorBody)
    )
)]
async fn list_users(
    State(state): State<AppState>,
    AuthenticatedUser(_user): AuthenticatedUser,
) -> Result<Json<Vec<User>>, ApiError> {
    let users = sqlx::query_as::<_, User>(
        "SELECT id, email, name, created_at
         FROM users
         ORDER BY created_at ASC, id ASC",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to list household users");
        ApiError::internal("failed to load household users")
    })?;

    Ok(Json(users))
}

#[utoipa::path(
    get,
    path = "/api/tokens",
    tag = "tokens",
    security(("sessionCookie" = [])),
    responses(
        (status = 200, description = "Personal access tokens for the current user", body = [ApiToken]),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Browser session required", body = ErrorBody)
    )
)]
async fn list_tokens(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
) -> Result<Json<Vec<ApiToken>>, ApiError> {
    let tokens = sqlx::query_as::<_, ApiToken>(
        "SELECT id, name, last4, last_used_at, created_at
         FROM api_tokens
         WHERE user_id = $1
         ORDER BY created_at DESC",
    )
    .bind(user.id)
    .fetch_all(&state.pool)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to list API tokens");
        ApiError::internal("failed to load API tokens")
    })?;

    Ok(Json(tokens))
}

#[utoipa::path(
    post,
    path = "/api/tokens",
    tag = "tokens",
    security(("sessionCookie" = [])),
    request_body = CreateTokenRequest,
    responses(
        (status = 201, description = "Personal access token minted (plaintext returned once)", body = CreatedApiToken),
        (status = 400, description = "Invalid token name", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Browser session required", body = ErrorBody)
    )
)]
async fn create_token(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Json(payload): Json<CreateTokenRequest>,
) -> Result<(StatusCode, Json<CreatedApiToken>), ApiError> {
    let name = payload.name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(ApiError::bad_request(
            "token name must be between 1 and 80 characters",
        ));
    }

    let id = Uuid::new_v4();
    let secret = random_hex_secret();
    let secret_hash = sha256_bytes(&secret);
    let last4 = secret[secret.len() - 4..].to_string();
    let plaintext = format!("pat_{id}_{secret}");

    let created_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "INSERT INTO api_tokens (id, user_id, name, secret_hash, last4)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING created_at",
    )
    .bind(id)
    .bind(user.id)
    .bind(name)
    .bind(secret_hash)
    .bind(&last4)
    .fetch_one(&state.pool)
    .await
    .map_err(|err| {
        tracing::error!(?err, "failed to insert API token");
        ApiError::internal("failed to create API token")
    })?;

    Ok((
        StatusCode::CREATED,
        Json(CreatedApiToken {
            id,
            name: name.to_string(),
            token: plaintext,
            last4,
            created_at,
        }),
    ))
}

#[utoipa::path(
    delete,
    path = "/api/tokens/{id}",
    tag = "tokens",
    params(
        ("id" = Uuid, Path, description = "Personal access token ID")
    ),
    security(("sessionCookie" = [])),
    responses(
        (status = 204, description = "Token revoked"),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 403, description = "Browser session required", body = ErrorBody),
        (status = 404, description = "Token not found", body = ErrorBody)
    )
)]
async fn delete_token(
    State(state): State<AppState>,
    SessionUser(user): SessionUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let result = sqlx::query("DELETE FROM api_tokens WHERE id = $1 AND user_id = $2")
        .bind(id)
        .bind(user.id)
        .execute(&state.pool)
        .await
        .map_err(|err| {
            tracing::error!(?err, "failed to delete API token");
            ApiError::internal("failed to revoke API token")
        })?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("token not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListRosterQuery {
    /// Optional filter: adversarial-review or plan-tournament
    pub skill: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/roster",
    tag = "arena",
    params(ListRosterQuery),
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Configured lineage champions and backups", body = RosterListResponse),
        (status = 400, description = "Invalid skill filter", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody)
    )
)]
async fn list_roster_handler(
    State(state): State<AppState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(query): Query<ListRosterQuery>,
) -> Result<Json<RosterListResponse>, ApiError> {
    let roster = list_roster(&state.pool, user.id, query.skill.as_deref()).await?;
    Ok(Json(RosterListResponse { roster }))
}

#[utoipa::path(
    put,
    path = "/api/roster/{skill}",
    tag = "arena",
    params(
        ("skill" = String, Path, description = "Skill whose roster slots are replaced (adversarial-review or plan-tournament)")
    ),
    request_body = ReplaceRosterRequest,
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Updated roster for the skill", body = RosterListResponse),
        (status = 400, description = "Invalid skill or slot payload", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 409, description = "Open trial blocks changing or disabling an active champion slot", body = ErrorBody)
    )
)]
async fn replace_roster_handler(
    State(state): State<AppState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(skill): Path<String>,
    Json(payload): Json<ReplaceRosterRequest>,
) -> Result<Json<RosterListResponse>, ApiError> {
    let roster = replace_roster(&state.pool, user.id, &skill, payload.slots).await?;
    Ok(Json(RosterListResponse { roster }))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct ListTrialsQuery {
    /// Optional status filter: active, completed, promoted, retired
    pub status: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/trials",
    tag = "arena",
    params(ListTrialsQuery),
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Challenger trials with live completed run counts", body = TrialsListResponse),
        (status = 400, description = "Invalid status filter", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody)
    )
)]
async fn list_trials_handler(
    State(state): State<AppState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(query): Query<ListTrialsQuery>,
) -> Result<Json<TrialsListResponse>, ApiError> {
    let trials = list_trials(&state.pool, user.id, query.status.as_deref()).await?;
    Ok(Json(TrialsListResponse { trials }))
}

#[utoipa::path(
    post,
    path = "/api/trials",
    tag = "arena",
    request_body = StartTrialInput,
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Started challenger trial", body = Trial),
        (status = 400, description = "Invalid trial parameters", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Lineage slot not found in roster", body = ErrorBody),
        (status = 409, description = "Open trial already exists on this slot", body = ErrorBody)
    )
)]
async fn start_trial_handler(
    State(state): State<AppState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Json(payload): Json<StartTrialInput>,
) -> Result<Json<Trial>, ApiError> {
    let trial = start_trial(&state.pool, user.id, payload).await?;
    Ok(Json(trial))
}

#[utoipa::path(
    post,
    path = "/api/trials/{id}/decide",
    tag = "arena",
    params(
        ("id" = i64, Path, description = "Trial ID")
    ),
    request_body = DecideTrialInput,
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Resolved challenger trial", body = Trial),
        (status = 400, description = "Invalid decision", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Trial not found", body = ErrorBody),
        (status = 409, description = "Trial already decided or roster incumbent changed", body = ErrorBody)
    )
)]
async fn decide_trial_handler(
    State(state): State<AppState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<i64>,
    Json(payload): Json<DecideTrialInput>,
) -> Result<Json<Trial>, ApiError> {
    let trial = decide_trial(&state.pool, user.id, id, payload).await?;
    Ok(Json(trial))
}

#[utoipa::path(
    post,
    path = "/api/panel/select",
    tag = "arena",
    request_body = SelectPanelInput,
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Selected subagent panel and prompt lenses", body = PanelSelection),
        (status = 400, description = "Invalid skill or artifactKind", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody)
    )
)]
async fn select_panel_handler(
    State(state): State<AppState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Json(payload): Json<SelectPanelInput>,
) -> Result<Json<PanelSelection>, ApiError> {
    let selection = select_panel(&state.pool, user.id, payload).await?;
    Ok(Json(selection))
}

#[utoipa::path(
    post,
    path = "/api/runs",
    tag = "arena",
    request_body = RecordRunInput,
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Recorded skill run idempotently by runKey", body = RecordedRunOutput),
        (status = 400, description = "Invalid run payload", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody)
    )
)]
async fn record_run_handler(
    State(state): State<AppState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Json(payload): Json<RecordRunInput>,
) -> Result<Json<RecordedRunOutput>, ApiError> {
    let out = record_run(&state.pool, user.id, payload).await?;
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/api/candidates/{id}/override",
    tag = "arena",
    params(
        ("id" = i64, Path, description = "Candidate ID")
    ),
    request_body = OverrideCandidateInput,
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Updated candidate override", body = OverrideCandidateResponse),
        (status = 400, description = "Invalid humanOverride value", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Candidate not found", body = ErrorBody)
    )
)]
async fn override_candidate_handler(
    State(state): State<AppState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<i64>,
    Json(payload): Json<OverrideCandidateInput>,
) -> Result<Json<OverrideCandidateResponse>, ApiError> {
    let out = override_candidate(&state.pool, user.id, id, payload).await?;
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/api/leaderboard",
    tag = "arena",
    params(LeaderboardQuery),
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Empirical model scorecards, timing percentiles, matrices, and trial comparisons", body = Leaderboard),
        (status = 400, description = "Invalid filter parameters", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody)
    )
)]
async fn get_leaderboard_handler(
    State(state): State<AppState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(query): Query<LeaderboardQuery>,
) -> Result<Json<Leaderboard>, ApiError> {
    let lb = get_leaderboard(&state.pool, user.id, query).await?;
    Ok(Json(lb))
}

#[utoipa::path(
    post,
    path = "/api/coding-runs",
    tag = "arena",
    request_body = CodingRunInput,
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Recorded single-model coding run idempotently by runKey", body = RecordedCodingRunOutput),
        (status = 400, description = "Invalid coding run payload", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody)
    )
)]
async fn record_coding_run_handler(
    State(state): State<AppState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Json(payload): Json<CodingRunInput>,
) -> Result<Json<RecordedCodingRunOutput>, ApiError> {
    let out = record_coding_run(&state.pool, user.id, payload).await?;
    Ok(Json(out))
}

#[utoipa::path(
    post,
    path = "/api/coding-runs/{id}/override",
    tag = "arena",
    params(
        ("id" = i64, Path, description = "Coding run ID")
    ),
    request_body = OverrideCodingRunInput,
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Updated coding run override", body = OverrideCodingRunResponse),
        (status = 400, description = "Invalid humanOverride value", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody),
        (status = 404, description = "Coding run not found", body = ErrorBody)
    )
)]
async fn override_coding_run_handler(
    State(state): State<AppState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Path(id): Path<i64>,
    Json(payload): Json<OverrideCodingRunInput>,
) -> Result<Json<OverrideCodingRunResponse>, ApiError> {
    let out = override_coding_run(&state.pool, user.id, id, payload).await?;
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/api/coding-leaderboard",
    tag = "arena",
    params(CodingLeaderboardQuery),
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Multi-axis coding model profiles and diff-size / task-kind slice cells", body = CodingLeaderboard),
        (status = 400, description = "Invalid filter parameters", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody)
    )
)]
async fn get_coding_leaderboard_handler(
    State(state): State<AppState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(query): Query<CodingLeaderboardQuery>,
) -> Result<Json<CodingLeaderboard>, ApiError> {
    let lb = get_coding_leaderboard(&state.pool, user.id, query).await?;
    Ok(Json(lb))
}

#[utoipa::path(
    post,
    path = "/api/shadow-bouts",
    tag = "arena",
    request_body = ShadowBoutInput,
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Recorded paired Champion vs Challenger shadow bout idempotently by boutKey", body = RecordedShadowBoutOutput),
        (status = 400, description = "Invalid shadow bout payload", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody)
    )
)]
async fn record_shadow_bout_handler(
    State(state): State<AppState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Json(payload): Json<ShadowBoutInput>,
) -> Result<Json<RecordedShadowBoutOutput>, ApiError> {
    let out = record_shadow_bout(&state.pool, user.id, payload).await?;
    Ok(Json(out))
}

#[utoipa::path(
    get,
    path = "/api/shadow-leaderboard",
    tag = "arena",
    params(ShadowLeaderboardQuery),
    security(
        ("sessionCookie" = []),
        ("bearerToken" = [])
    ),
    responses(
        (status = 200, description = "Paired coding and orchestration shadow bout scorecards, head-to-head pairs, and recent bouts", body = ShadowLeaderboard),
        (status = 400, description = "Invalid filter parameters", body = ErrorBody),
        (status = 401, description = "Not signed in", body = ErrorBody)
    )
)]
async fn get_shadow_leaderboard_handler(
    State(state): State<AppState>,
    AuthenticatedUser(user): AuthenticatedUser,
    Query(query): Query<ShadowLeaderboardQuery>,
) -> Result<Json<ShadowLeaderboard>, ApiError> {
    let lb = get_shadow_leaderboard(&state.pool, user.id, query).await?;
    Ok(Json(lb))
}
