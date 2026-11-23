use std::collections::BTreeMap;
use std::net::TcpListener as StdTcpListener;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use app::apiclient::{ClientConfig, mint_token_via_login};
use app::db;
use app::routes::{self, AppState, build_router};
use reqwest::StatusCode;
use reqwest::header::{AUTHORIZATION, COOKIE, ORIGIN};
use serde_json::{Value, json};
use sqlx::{Connection, Executor, PgConnection, PgPool};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::Command;
use uuid::Uuid;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn openapi_spec_matches_committed_file() {
    let mut generated = routes::openapi_spec()
        .to_pretty_json()
        .expect("generate openapi json");
    generated.push('\n');
    let committed_path = repo_root().join("docs/openapi.json");
    let committed = std::fs::read_to_string(&committed_path).expect("read docs/openapi.json");
    assert_eq!(
        generated, committed,
        "docs/openapi.json is out of date; run `make spec`"
    );
}

#[test]
fn bun_lockfile_uses_default_registry_resolution() {
    let lock_path = repo_root().join("web/bun.lock");
    let lock = std::fs::read_to_string(&lock_path).expect("read web/bun.lock");
    assert!(
        !lock.contains("https://"),
        "web/bun.lock pins explicit tarball URLs; strip private registry URLs to \"\" so CI and Docker can resolve packages"
    );
}

#[test]
fn publish_plan_script_matrix() {
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let other_sha = "fedcba9876543210fedcba9876543210fedcba98";

    struct Case {
        name: &'static str,
        env: Vec<(&'static str, &'static str)>,
        want_publish: &'static str,
        want_promote: &'static str,
        want_ref: &'static str,
        want_sha_tag: &'static str,
        want_version: &'static str,
    }

    let cases = vec![
        Case {
            name: "green push CI at main tip publishes and promotes",
            env: vec![
                ("EVENT_NAME", "workflow_run"),
                ("REPOSITORY", "robert-crandall/model-arena-rust"),
                ("RUN_EVENT", "push"),
                ("RUN_CONCLUSION", "success"),
                ("RUN_HEAD_SHA", sha),
                ("BRANCH_TIP", sha),
            ],
            want_publish: "true",
            want_promote: "true",
            want_ref: sha,
            want_sha_tag: "sha-0123456789abcdef0123456789abcdef01234567",
            want_version: "",
        },
        Case {
            name: "pull_request CI run never publishes even if tip matches",
            env: vec![
                ("EVENT_NAME", "workflow_run"),
                ("REPOSITORY", "robert-crandall/model-arena-rust"),
                ("RUN_EVENT", "pull_request"),
                ("RUN_CONCLUSION", "success"),
                ("RUN_HEAD_SHA", sha),
                ("BRANCH_TIP", sha),
            ],
            want_publish: "false",
            want_promote: "false",
            want_ref: "",
            want_sha_tag: "",
            want_version: "",
        },
        Case {
            name: "stale main CI run declines when main moved",
            env: vec![
                ("EVENT_NAME", "workflow_run"),
                ("REPOSITORY", "robert-crandall/model-arena-rust"),
                ("RUN_EVENT", "push"),
                ("RUN_CONCLUSION", "success"),
                ("RUN_HEAD_SHA", sha),
                ("BRANCH_TIP", other_sha),
            ],
            want_publish: "false",
            want_promote: "false",
            want_ref: "",
            want_sha_tag: "",
            want_version: "",
        },
        Case {
            name: "failed CI run declines",
            env: vec![
                ("EVENT_NAME", "workflow_run"),
                ("REPOSITORY", "robert-crandall/model-arena-rust"),
                ("RUN_EVENT", "push"),
                ("RUN_CONCLUSION", "failure"),
                ("RUN_HEAD_SHA", sha),
                ("BRANCH_TIP", sha),
            ],
            want_publish: "false",
            want_promote: "false",
            want_ref: "",
            want_sha_tag: "",
            want_version: "",
        },
        Case {
            name: "version tag push publishes immutable tag without moving main",
            env: vec![
                ("EVENT_NAME", "push"),
                ("REPOSITORY", "robert-crandall/model-arena-rust"),
                ("GITHUB_REF", "refs/tags/v0.2.0"),
                ("SHA", sha),
            ],
            want_publish: "true",
            want_promote: "false",
            want_ref: sha,
            want_sha_tag: "",
            want_version: "v0.2.0",
        },
        Case {
            name: "workflow_dispatch on main publishes and promotes",
            env: vec![
                ("EVENT_NAME", "workflow_dispatch"),
                ("REPOSITORY", "robert-crandall/model-arena-rust"),
                ("GITHUB_REF", "refs/heads/main"),
                ("SHA", sha),
            ],
            want_publish: "true",
            want_promote: "true",
            want_ref: sha,
            want_sha_tag: "sha-0123456789abcdef0123456789abcdef01234567",
            want_version: "",
        },
        Case {
            name: "workflow_dispatch on feature branch is rejected",
            env: vec![
                ("EVENT_NAME", "workflow_dispatch"),
                ("REPOSITORY", "robert-crandall/model-arena-rust"),
                ("GITHUB_REF", "refs/heads/feature-branch"),
                ("SHA", sha),
            ],
            want_publish: "false",
            want_promote: "false",
            want_ref: "",
            want_sha_tag: "",
            want_version: "",
        },
    ];

    let script = repo_root().join("scripts/ci/publish-plan.sh");
    for tc in cases {
        let output = std::process::Command::new(&script)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .envs(tc.env)
            .output()
            .expect("run publish-plan.sh");
        assert!(
            output.status.success(),
            "case '{}' exited non-zero: {}",
            tc.name,
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
        let parsed: BTreeMap<&str, &str> = stdout
            .lines()
            .map(|line| line.split_once('=').expect("key=value line"))
            .collect();

        assert_eq!(
            parsed.get("image").copied(),
            Some("ghcr.io/robert-crandall/model-arena-rust"),
            "case '{}': image must be lowercased",
            tc.name
        );
        assert_eq!(
            parsed.get("publish").copied(),
            Some(tc.want_publish),
            "{}",
            tc.name
        );
        assert_eq!(
            parsed.get("promote_main").copied(),
            Some(tc.want_promote),
            "{}",
            tc.name
        );
        assert_eq!(parsed.get("ref").copied(), Some(tc.want_ref), "{}", tc.name);
        assert_eq!(
            parsed.get("sha_tag").copied(),
            Some(tc.want_sha_tag),
            "{}",
            tc.name
        );
        assert_eq!(
            parsed.get("version").copied(),
            Some(tc.want_version),
            "{}",
            tc.name
        );
    }
}

#[test]
fn notify_decision_script_matrix() {
    let cases = vec![
        (
            "CI/push/success",
            "CI",
            "push",
            "success",
            "false",
            "not-a-failure",
        ),
        (
            "CI/push/failure",
            "CI",
            "push",
            "failure",
            "true",
            "main-broken",
        ),
        (
            "CI/push/timed_out",
            "CI",
            "push",
            "timed_out",
            "true",
            "main-broken",
        ),
        (
            "CI/push/startup_failure",
            "CI",
            "push",
            "startup_failure",
            "true",
            "main-broken",
        ),
        (
            "CI/push/cancelled",
            "CI",
            "push",
            "cancelled",
            "false",
            "not-a-failure",
        ),
        (
            "CI/pull_request/success",
            "CI",
            "pull_request",
            "success",
            "false",
            "not-a-failure",
        ),
        (
            "CI/pull_request/failure",
            "CI",
            "pull_request",
            "failure",
            "false",
            "not-a-push",
        ),
        (
            "CI/pull_request/timed_out",
            "CI",
            "pull_request",
            "timed_out",
            "false",
            "not-a-push",
        ),
        (
            "CI/pull_request/startup_failure",
            "CI",
            "pull_request",
            "startup_failure",
            "false",
            "not-a-push",
        ),
        (
            "CI/pull_request/cancelled",
            "CI",
            "pull_request",
            "cancelled",
            "false",
            "not-a-failure",
        ),
        (
            "Publish/workflow_run/success",
            "Publish",
            "workflow_run",
            "success",
            "false",
            "not-a-failure",
        ),
        (
            "Publish/workflow_run/failure",
            "Publish",
            "workflow_run",
            "failure",
            "true",
            "publish-failed",
        ),
        (
            "Publish/workflow_run/skipped",
            "Publish",
            "workflow_run",
            "skipped",
            "false",
            "not-a-failure",
        ),
        (
            "Publish/workflow_run/cancelled",
            "Publish",
            "workflow_run",
            "cancelled",
            "false",
            "not-a-failure",
        ),
        (
            "Publish/workflow_dispatch/success",
            "Publish",
            "workflow_dispatch",
            "success",
            "false",
            "not-a-failure",
        ),
        (
            "Publish/workflow_dispatch/failure",
            "Publish",
            "workflow_dispatch",
            "failure",
            "true",
            "publish-failed",
        ),
        (
            "Publish/workflow_dispatch/skipped",
            "Publish",
            "workflow_dispatch",
            "skipped",
            "false",
            "not-a-failure",
        ),
        (
            "Publish/workflow_dispatch/cancelled",
            "Publish",
            "workflow_dispatch",
            "cancelled",
            "false",
            "not-a-failure",
        ),
        (
            "Publish/push/success",
            "Publish",
            "push",
            "success",
            "false",
            "not-a-failure",
        ),
        (
            "Publish/push/failure",
            "Publish",
            "push",
            "failure",
            "true",
            "publish-failed",
        ),
        (
            "Publish/push/skipped",
            "Publish",
            "push",
            "skipped",
            "false",
            "not-a-failure",
        ),
        (
            "Publish/push/cancelled",
            "Publish",
            "push",
            "cancelled",
            "false",
            "not-a-failure",
        ),
    ];

    let script = repo_root().join("scripts/ci/notify-decision.sh");
    for (name, workflow, event, concl, want_notify, want_reason) in cases {
        let output = std::process::Command::new(&script)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("WORKFLOW_NAME", workflow)
            .env("RUN_EVENT", event)
            .env("RUN_CONCLUSION", concl)
            .output()
            .expect("run notify-decision.sh");
        assert!(
            output.status.success(),
            "case '{}' exited non-zero: {}",
            name,
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
        let parsed: BTreeMap<&str, &str> = stdout
            .lines()
            .filter_map(|line| line.split_once('='))
            .collect();

        assert_eq!(parsed.get("notify").copied(), Some(want_notify), "{}", name);
        assert_eq!(parsed.get("reason").copied(), Some(want_reason), "{}", name);
    }
}

#[test]
fn init_script_renames_template_cleanly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = repo_root();

    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg("tar -cf - --exclude=./target --exclude=./bin --exclude=./.bin --exclude=./web/node_modules --exclude=./web/build --exclude=./.git . | (cd \"$1\" && tar -xf -)")
        .arg("copy-template")
        .arg(dir.path())
        .current_dir(&root)
        .status()
        .expect("copy template tree");
    assert!(status.success(), "copy template tree failed");

    let git_init = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .expect("git init");
    assert!(git_init.success());

    let git_add = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir.path())
        .status()
        .expect("git add");
    assert!(git_add.success());

    let output = std::process::Command::new("bash")
        .args([
            "scripts/init.sh",
            "github.com/acme/chore-board",
            "Chore Board",
        ])
        .current_dir(dir.path())
        .output()
        .expect("run scripts/init.sh");
    assert!(
        output.status.success(),
        "scripts/init.sh failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let cargo_toml =
        std::fs::read_to_string(dir.path().join("Cargo.toml")).expect("read renamed Cargo.toml");
    assert!(cargo_toml.contains("name = \"chore-board\""));
    assert!(cargo_toml.contains("name = \"chore-board-mcp\""));
    assert!(cargo_toml.contains("name = \"app\""));

    let apiclient_rs = std::fs::read_to_string(dir.path().join("src/apiclient.rs"))
        .expect("read renamed src/apiclient.rs");
    assert!(apiclient_rs.contains("pub const APP_SLUG: &str = \"chore-board\";"));

    let auth_rs =
        std::fs::read_to_string(dir.path().join("src/auth.rs")).expect("read renamed src/auth.rs");
    assert!(auth_rs.contains("pub const SESSION_COOKIE_NAME: &str = \"chore_board_session\";"));
}

struct TestDb {
    admin_url: String,
    db_name: String,
    db_url: String,
    pool: PgPool,
}

impl TestDb {
    async fn setup() -> Option<Self> {
        let base_url = match std::env::var("TEST_DATABASE_URL") {
            Ok(val) if !val.trim().is_empty() => val,
            _ => {
                if std::env::var("REQUIRE_TEST_DB").as_deref() == Ok("1") {
                    panic!("REQUIRE_TEST_DB=1 is set, but TEST_DATABASE_URL is missing");
                }
                eprintln!("skipping Postgres integration test (TEST_DATABASE_URL not set)");
                return None;
            }
        };

        let mut parsed = url::Url::parse(&base_url).expect("parse TEST_DATABASE_URL");
        let db_name = format!("model_arena_rust_test_{}", Uuid::new_v4().simple());

        let mut admin_conn = PgConnection::connect(parsed.as_str())
            .await
            .expect("connect to admin database");
        admin_conn
            .execute(format!("CREATE DATABASE \"{db_name}\"").as_str())
            .await
            .expect("create isolated test database");
        drop(admin_conn);

        parsed.set_path(&format!("/{db_name}"));
        let db_url = parsed.to_string();

        let pool = db::connect_and_migrate(&db_url)
            .await
            .expect("connect and run sqlx migrations");

        Some(Self {
            admin_url: base_url,
            db_name,
            db_url,
            pool,
        })
    }

    async fn teardown(self) {
        self.pool.close().await;
        if let Ok(mut admin_conn) = PgConnection::connect(&self.admin_url).await {
            let _ = admin_conn
                .execute(
                    format!("DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)", self.db_name).as_str(),
                )
                .await;
        }
    }
}

async fn spawn_in_process_server(pool: PgPool, allow_open_registration: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");

    let state = AppState {
        pool,
        secure_cookies: false,
        allow_open_registration,
    };
    let app = build_router(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    format!("http://{addr}")
}

#[tokio::test]
async fn multi_user_auth_csrf_and_tokens_flow() {
    let Some(tdb) = TestDb::setup().await else {
        return;
    };
    let base_url = spawn_in_process_server(tdb.pool.clone(), true).await;

    let client_a = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("client a");
    let client_b = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("client b");
    let bare_client = reqwest::Client::new();

    // 1. Healthz and open-registration app info
    let health: Value = bare_client
        .get(format!("{base_url}/healthz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health, json!({ "status": "ok" }));

    let app_info: Value = bare_client
        .get(format!("{base_url}/api/app"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(app_info, json!({ "registrationOpen": true }));

    // 2. Register two household users; duplicate email (case-insensitive) fails with 409
    let reg_a = client_a
        .post(format!("{base_url}/api/auth/register"))
        .json(&json!({
            "email": "Leo@Example.com",
            "password": "very-strong-passphrase",
            "name": "Leo"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reg_a.status(), StatusCode::CREATED);
    let user_a: Value = reg_a.json().await.unwrap();
    assert_eq!(user_a["email"], "leo@example.com");
    assert_eq!(user_a["name"], "Leo");

    let dup = bare_client
        .post(format!("{base_url}/api/auth/register"))
        .json(&json!({
            "email": "leo@example.com",
            "password": "another-strong-passphrase"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(dup.status(), StatusCode::CONFLICT);

    let reg_b = client_b
        .post(format!("{base_url}/api/auth/register"))
        .json(&json!({
            "email": "dad@example.com",
            "password": "dad-strong-passphrase",
            "name": "Dad"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reg_b.status(), StatusCode::CREATED);

    // 3. Invalid login attempts return 401
    let bad_pw = bare_client
        .post(format!("{base_url}/api/auth/login"))
        .json(&json!({
            "email": "leo@example.com",
            "password": "wrong-password"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_pw.status(), StatusCode::UNAUTHORIZED);

    let bad_user = bare_client
        .post(format!("{base_url}/api/auth/login"))
        .json(&json!({
            "email": "nobody@example.com",
            "password": "wrong-password"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_user.status(), StatusCode::UNAUTHORIZED);

    // 4. CSRF defense blocks cross-site Sec-Fetch-Site and mismatched Origin
    let csrf_fetch_site = client_a
        .post(format!("{base_url}/api/tokens"))
        .header("Sec-Fetch-Site", "cross-site")
        .json(&json!({ "name": "evil" }))
        .send()
        .await
        .unwrap();
    assert_eq!(csrf_fetch_site.status(), StatusCode::FORBIDDEN);

    let csrf_origin = client_a
        .post(format!("{base_url}/api/tokens"))
        .header(ORIGIN, "https://attacker.example")
        .json(&json!({ "name": "evil" }))
        .send()
        .await
        .unwrap();
    assert_eq!(csrf_origin.status(), StatusCode::FORBIDDEN);

    // 5. Same-origin token minting, listing, bearer auth, and cookie-fallback isolation
    let create_tok = client_a
        .post(format!("{base_url}/api/tokens"))
        .header(ORIGIN, &base_url)
        .json(&json!({ "name": "laptop-mcp" }))
        .send()
        .await
        .unwrap();
    assert_eq!(create_tok.status(), StatusCode::CREATED);
    let minted: Value = create_tok.json().await.unwrap();
    let token_id = minted["id"].as_str().unwrap().to_string();
    let raw_token = minted["token"].as_str().unwrap().to_string();
    assert!(raw_token.starts_with("pat_"));

    // Bearer token authenticates /api/auth/me without cookies
    let bearer_me = bare_client
        .get(format!("{base_url}/api/auth/me"))
        .header(AUTHORIZATION, format!("Bearer {raw_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(bearer_me.status(), StatusCode::OK);
    let bearer_user: Value = bearer_me.json().await.unwrap();
    assert_eq!(bearer_user["email"], "leo@example.com");

    // Household directory (/api/users) requires auth and works for both session cookies and PATs
    let unauth_users = bare_client
        .get(format!("{base_url}/api/users"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth_users.status(), StatusCode::UNAUTHORIZED);

    let cookie_users: Vec<Value> = client_a
        .get(format!("{base_url}/api/users"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cookie_users.len(), 2);
    assert_eq!(cookie_users[0]["email"], "leo@example.com");
    assert_eq!(cookie_users[0]["name"], "Leo");
    assert_eq!(cookie_users[1]["email"], "dad@example.com");
    assert_eq!(cookie_users[1]["name"], "Dad");

    let bearer_users: Vec<Value> = bare_client
        .get(format!("{base_url}/api/users"))
        .header(AUTHORIZATION, format!("BEARER {raw_token}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(bearer_users, cookie_users);

    // Logout rejects Authorization headers (403) and preserves any active browser session
    let bearer_logout = client_a
        .post(format!("{base_url}/api/auth/logout"))
        .header(ORIGIN, &base_url)
        .header(AUTHORIZATION, format!("Bearer {raw_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(bearer_logout.status(), StatusCode::FORBIDDEN);

    let still_logged_in = client_a
        .get(format!("{base_url}/api/auth/me"))
        .send()
        .await
        .unwrap();
    assert_eq!(still_logged_in.status(), StatusCode::OK);

    // Token listing returns metadata + updated lastUsedAt, never the raw token
    let list_tok: Vec<Value> = client_a
        .get(format!("{base_url}/api/tokens"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list_tok.len(), 1);
    assert!(list_tok[0].get("token").is_none());
    assert!(list_tok[0]["lastUsedAt"].is_string());

    // Bearer token cannot mint another token (SessionUser required -> 403 Forbidden)
    let bearer_mint = bare_client
        .post(format!("{base_url}/api/tokens"))
        .header(AUTHORIZATION, format!("Bearer {raw_token}"))
        .json(&json!({ "name": "escalate" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bearer_mint.status(), StatusCode::FORBIDDEN);

    // Invalid Authorization header must NOT fall back to a valid session cookie
    let bad_bearer_with_cookie = client_a
        .get(format!("{base_url}/api/auth/me"))
        .header(AUTHORIZATION, "Bearer pat_invalid_token")
        .send()
        .await
        .unwrap();
    assert_eq!(bad_bearer_with_cookie.status(), StatusCode::UNAUTHORIZED);

    // User B cannot delete User A's personal access token
    let wrong_owner_del = client_b
        .delete(format!("{base_url}/api/tokens/{token_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_owner_del.status(), StatusCode::NOT_FOUND);

    // User A revokes token; subsequent bearer call fails
    let owner_del = client_a
        .delete(format!("{base_url}/api/tokens/{token_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(owner_del.status(), StatusCode::NO_CONTENT);

    let revoked_me = bare_client
        .get(format!("{base_url}/api/auth/me"))
        .header(AUTHORIZATION, format!("Bearer {raw_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(revoked_me.status(), StatusCode::UNAUTHORIZED);

    // 6. Sliding session renewal extends DB expiry and re-emits Set-Cookie
    sqlx::query("UPDATE sessions SET expires_at = now() + interval '5 days'")
        .execute(&tdb.pool)
        .await
        .unwrap();

    let login_fresh = bare_client
        .post(format!("{base_url}/api/auth/login"))
        .json(&json!({
            "email": "leo@example.com",
            "password": "very-strong-passphrase"
        }))
        .send()
        .await
        .unwrap();
    let set_cookie = login_fresh
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert!(set_cookie.starts_with("model_arena_rust_session="));

    sqlx::query("UPDATE sessions SET expires_at = now() + interval '5 days'")
        .execute(&tdb.pool)
        .await
        .unwrap();

    let renewed_resp = bare_client
        .get(format!("{base_url}/api/auth/me"))
        .header(COOKIE, &set_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(renewed_resp.status(), StatusCode::OK);
    assert!(
        renewed_resp.headers().get("set-cookie").is_some(),
        "sliding renewal must refresh Set-Cookie when <15 days remain"
    );

    tdb.teardown().await;
}

#[tokio::test]
async fn closed_registration_allows_only_bootstrap_user() {
    let Some(tdb) = TestDb::setup().await else {
        return;
    };
    let base_url = spawn_in_process_server(tdb.pool.clone(), false).await;
    let client = reqwest::Client::new();

    // Fresh DB reports registrationOpen=true so the initial bootstrap user can register
    let before: Value = client
        .get(format!("{base_url}/api/app"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(before, json!({ "registrationOpen": true }));

    // Race two simultaneous registrations against ALLOW_OPEN_REGISTRATION=false
    let fut1 = client
        .post(format!("{base_url}/api/auth/register"))
        .json(&json!({
            "email": "first@example.com",
            "password": "first-strong-passphrase"
        }))
        .send();
    let fut2 = client
        .post(format!("{base_url}/api/auth/register"))
        .json(&json!({
            "email": "second@example.com",
            "password": "second-strong-passphrase"
        }))
        .send();

    let (r1, r2) = tokio::join!(fut1, fut2);
    let mut statuses = [r1.unwrap().status(), r2.unwrap().status()];
    statuses.sort_by_key(|s| s.as_u16());
    assert_eq!(statuses, [StatusCode::CREATED, StatusCode::FORBIDDEN]);

    let after: Value = client
        .get(format!("{base_url}/api/app"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after, json!({ "registrationOpen": false }));

    tdb.teardown().await;
}

#[tokio::test]
async fn binaries_spa_healthcheck_and_mcp_stdio_end_to_end() {
    let Some(tdb) = TestDb::setup().await else {
        return;
    };

    // Reserve a free port for the compiled `model-arena-rust` server binary
    let probe = StdTcpListener::bind("127.0.0.1:0").expect("reserve port");
    let addr = probe.local_addr().expect("local addr");
    drop(probe);

    let server_bin = env!("CARGO_BIN_EXE_model-arena-rust");
    let mut server_child = Command::new(server_bin)
        .env("DATABASE_URL", &tdb.db_url)
        .env("ADDR", addr.to_string())
        .env("APP_ENV", "development")
        .env("ALLOW_OPEN_REGISTRATION", "true")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn model-arena-rust binary");

    let base_url = format!("http://{addr}");
    let http = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();

    let mut healthy = false;
    for _ in 0..50 {
        if let Ok(resp) = http.get(format!("{base_url}/healthz")).send().await
            && resp.status() == StatusCode::OK
        {
            healthy = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        healthy,
        "model-arena-rust binary did not become healthy on {base_url}"
    );

    // 1. Embedded Solid 2.0 SPA assets + client-side route fallback + API 404 boundary
    let root_html = http.get(&base_url).send().await.unwrap();
    assert_eq!(root_html.status(), StatusCode::OK);
    let body = root_html.text().await.unwrap();
    assert!(body.contains("<div id=\"app\"></div>"));

    let spa_route = http.get(format!("{base_url}/login")).send().await.unwrap();
    assert_eq!(spa_route.status(), StatusCode::OK);

    let theme_js = http
        .get(format!("{base_url}/theme.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(theme_js.status(), StatusCode::OK);

    let api_404 = http
        .get(format!("{base_url}/api/does-not-exist"))
        .send()
        .await
        .unwrap();
    assert_eq!(api_404.status(), StatusCode::NOT_FOUND);

    // 2. Built-in container healthcheck subcommand (`model-arena-rust healthcheck`)
    let hc_status = Command::new(server_bin)
        .arg("healthcheck")
        .env("ADDR", addr.to_string())
        .status()
        .await
        .expect("run model-arena-rust healthcheck");
    assert!(hc_status.success(), "model-arena-rust healthcheck failed");

    // 3. Register two household users, mint MCP config (0600), and exercise model-arena-rust-mcp over stdio
    let reg = http
        .post(format!("{base_url}/api/auth/register"))
        .json(&json!({
            "email": "agent@example.com",
            "password": "mcp-strong-passphrase",
            "name": "Homelab Agent"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reg.status(), StatusCode::CREATED);

    let reg_sister = reqwest::Client::new()
        .post(format!("{base_url}/api/auth/register"))
        .json(&json!({
            "email": "maya@example.com",
            "password": "maya-strong-passphrase",
            "name": "Maya"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reg_sister.status(), StatusCode::CREATED);

    let minted = mint_token_via_login(
        &base_url,
        "agent@example.com",
        "mcp-strong-passphrase",
        "stdio-test",
    )
    .await
    .expect("mint token via login");

    let temp_dir =
        std::env::temp_dir().join(format!("model-arena-rust-mcp-test-{}", Uuid::new_v4()));
    let cfg_path = temp_dir.join("model-arena-rust.json");
    let cfg = ClientConfig {
        app_url: base_url.clone(),
        token: minted.token.clone(),
    };
    cfg.write_atomic(&cfg_path)
        .expect("write atomic mcp config");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&cfg_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "MCP config must be written with 0600 permissions"
        );
    }

    // Invoke `model-arena-rust-mcp --config <path>` without `serve` to verify default subcommand dispatch
    let mcp_bin = env!("CARGO_BIN_EXE_model-arena-rust-mcp");
    let mut mcp_child = Command::new(mcp_bin)
        .arg("--config")
        .arg(&cfg_path)
        .env_remove("MCP_APP_URL")
        .env_remove("MCP_APP_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn model-arena-rust-mcp");

    let mut mcp_stdin = mcp_child.stdin.take().unwrap();
    let mut mcp_lines = BufReader::new(mcp_child.stdout.take().unwrap()).lines();

    // JSON-RPC 1: initialize
    let init_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "integration-test", "version": "0.1.0" }
        }
    });
    mcp_stdin
        .write_all(format!("{init_req}\n").as_bytes())
        .await
        .unwrap();
    mcp_stdin.flush().await.unwrap();

    let init_line = tokio::time::timeout(Duration::from_secs(5), mcp_lines.next_line())
        .await
        .expect("initialize timeout")
        .expect("read initialize line")
        .expect("initialize eof");
    let init_resp: Value = serde_json::from_str(&init_line).unwrap();
    assert_eq!(init_resp["id"], 1);
    assert_eq!(
        init_resp["result"]["serverInfo"]["name"],
        "model-arena-rust-mcp"
    );

    // JSON-RPC notification: notifications/initialized
    let initialized_notif = json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    mcp_stdin
        .write_all(format!("{initialized_notif}\n").as_bytes())
        .await
        .unwrap();

    // JSON-RPC 2: tools/call whoami
    let call_req = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "whoami",
            "arguments": {}
        }
    });
    mcp_stdin
        .write_all(format!("{call_req}\n").as_bytes())
        .await
        .unwrap();
    mcp_stdin.flush().await.unwrap();

    let call_line = tokio::time::timeout(Duration::from_secs(5), mcp_lines.next_line())
        .await
        .expect("whoami timeout")
        .expect("read whoami line")
        .expect("whoami eof");
    let call_resp: Value = serde_json::from_str(&call_line).unwrap();
    assert_eq!(call_resp["id"], 2);
    let text = call_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("whoami text content");
    let whoami_user: Value = serde_json::from_str(text).expect("whoami json user");
    assert_eq!(whoami_user["email"], "agent@example.com");
    assert_eq!(whoami_user["name"], "Homelab Agent");

    // JSON-RPC 3: tools/call list_users
    let users_req = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "list_users",
            "arguments": {}
        }
    });
    mcp_stdin
        .write_all(format!("{users_req}\n").as_bytes())
        .await
        .unwrap();
    mcp_stdin.flush().await.unwrap();

    let users_line = tokio::time::timeout(Duration::from_secs(5), mcp_lines.next_line())
        .await
        .expect("list_users timeout")
        .expect("read list_users line")
        .expect("list_users eof");
    let users_resp: Value = serde_json::from_str(&users_line).unwrap();
    assert_eq!(users_resp["id"], 3);
    let users_text = users_resp["result"]["content"][0]["text"]
        .as_str()
        .expect("list_users text content");
    let listed_users: Vec<Value> = serde_json::from_str(users_text).expect("list_users json array");
    assert_eq!(listed_users.len(), 2);
    assert_eq!(listed_users[0]["email"], "agent@example.com");
    assert_eq!(listed_users[1]["email"], "maya@example.com");
    assert_eq!(listed_users[1]["name"], "Maya");

    drop(mcp_stdin);
    let _ = mcp_child.kill().await;
    let _ = std::fs::remove_dir_all(&temp_dir);
    let _ = server_child.kill().await;
    tdb.teardown().await;
}

#[tokio::test]
async fn arena_schema_migration_enforces_constraints_and_cascades() {
    let Some(tdb) = TestDb::setup().await else {
        return;
    };

    let user_id: Uuid = sqlx::query_scalar(
        "INSERT INTO users (email, password_hash, name)
         VALUES ('arena@example.com', 'hash', 'Arena')
         RETURNING id",
    )
    .fetch_one(&tdb.pool)
    .await
    .expect("insert user");

    sqlx::query(
        "INSERT INTO arena_roster (user_id, skill, lineage, champion_model, backup_model, sort_order)
         VALUES ($1, 'adversarial-review', 'openai', 'gpt-6.1-sol', 'gpt-5.5', 0)",
    )
    .bind(user_id)
    .execute(&tdb.pool)
    .await
    .expect("insert roster");

    let trial_id: i64 = sqlx::query_scalar(
        "INSERT INTO arena_trials (user_id, skill, lineage, champion_model, challenger_model, target_runs)
         VALUES ($1, 'adversarial-review', 'openai', 'gpt-6.1-sol', 'gpt-6.2-sol', 15)
         RETURNING id",
    )
    .bind(user_id)
    .fetch_one(&tdb.pool)
    .await
    .expect("insert first active trial");

    // Partial unique index must block a second open trial on the same (user_id, skill, lineage)
    let dup_open = sqlx::query(
        "INSERT INTO arena_trials (user_id, skill, lineage, champion_model, challenger_model, target_runs)
         VALUES ($1, 'adversarial-review', 'openai', 'gpt-6.1-sol', 'gpt-6.3-sol', 15)",
    )
    .bind(user_id)
    .execute(&tdb.pool)
    .await;
    assert!(
        dup_open.is_err(),
        "second open trial on the same slot must fail unique index"
    );

    // Retiring the trial frees the slot for a new bout
    sqlx::query("UPDATE arena_trials SET status = 'retired', decided_at = NOW() WHERE id = $1")
        .bind(trial_id)
        .execute(&tdb.pool)
        .await
        .expect("retire trial");

    sqlx::query(
        "INSERT INTO arena_trials (user_id, skill, lineage, champion_model, challenger_model, target_runs)
         VALUES ($1, 'adversarial-review', 'openai', 'gpt-6.1-sol', 'gpt-6.3-sol', 15)",
    )
    .bind(user_id)
    .execute(&tdb.pool)
    .await
    .expect("insert trial after prior retired");

    // Reject invalid skill CHECK constraint on arena_roster
    let invalid_skill = sqlx::query(
        "INSERT INTO arena_roster (user_id, skill, lineage, champion_model, backup_model, sort_order, enabled)
         VALUES ($1, 'invalid-skill', 'anthropic', 'claude-opus-5.5', '', 0, TRUE)",
    )
    .bind(user_id)
    .execute(&tdb.pool)
    .await;
    assert!(
        invalid_skill.is_err(),
        "expected CHECK constraint on arena_roster.skill to reject invalid skill"
    );

    // Insert a coding run and verify cascade on user deletion
    sqlx::query(
        "INSERT INTO arena_coding_runs (user_id, run_key, model, lineage, task_kind, loc_added, loc_deleted, size_bucket)
         VALUES ($1, 'cr_1', 'claude-opus-5.5', 'anthropic', 'feature', 80, 20, 'M')",
    )
    .bind(user_id)
    .execute(&tdb.pool)
    .await
    .expect("insert coding run");

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(user_id)
        .execute(&tdb.pool)
        .await
        .expect("delete user");

    let remaining_roster: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM arena_roster WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&tdb.pool)
            .await
            .unwrap();
    assert_eq!(remaining_roster, 0);

    let remaining_trials: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM arena_trials WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&tdb.pool)
            .await
            .unwrap();
    assert_eq!(remaining_trials, 0);

    let remaining_coding_runs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM arena_coding_runs WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&tdb.pool)
            .await
            .unwrap();
    assert_eq!(remaining_coding_runs, 0);

    tdb.teardown().await;
}

#[tokio::test]
async fn arena_roster_trials_and_panel_selection_flow() {
    let Some(tdb) = TestDb::setup().await else {
        return;
    };
    let base_url = spawn_in_process_server(tdb.pool.clone(), true).await;

    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("cookie client");

    let reg = client
        .post(format!("{base_url}/api/auth/register"))
        .json(&json!({
            "email": "arena@example.com",
            "password": "very-strong-passphrase",
            "name": "Arena Tester"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reg.status(), StatusCode::CREATED);

    // Configure rosters for adversarial-review and plan-tournament
    let slots = json!({
        "slots": [
            { "lineage": "anthropic", "championModel": "claude-opus-5.5", "backupModel": "claude-sonnet-5", "sortOrder": 0, "enabled": true },
            { "lineage": "openai", "championModel": "gpt-6.1-sol", "backupModel": "gpt-5.6-sol", "sortOrder": 1, "enabled": true },
            { "lineage": "google", "championModel": "gemini-4-argon", "backupModel": "gemini-4-carbon", "sortOrder": 2, "enabled": true }
        ]
    });

    for skill in ["adversarial-review", "plan-tournament"] {
        let put_res = client
            .put(format!("{base_url}/api/roster/{skill}"))
            .json(&slots)
            .send()
            .await
            .unwrap();
        assert_eq!(put_res.status(), StatusCode::OK);
        let body: Value = put_res.json().await.unwrap();
        assert_eq!(body["roster"].as_array().unwrap().len(), 3);
    }

    // Select panel for adversarial-review: pr_diff uses all 3 lineages; plan excludes host lineage
    let ar_pr_diff: Value = client
        .post(format!("{base_url}/api/panel/select"))
        .json(&json!({
            "skill": "adversarial-review",
            "artifactKind": "pr_diff",
            "hostModel": "claude-opus-5.5"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ar_pr_diff["configured"], true);
    assert_eq!(ar_pr_diff["hostLineage"], "anthropic");
    assert_eq!(ar_pr_diff["legs"].as_array().unwrap().len(), 3);

    let ar_plan: Value = client
        .post(format!("{base_url}/api/panel/select"))
        .json(&json!({
            "skill": "adversarial-review",
            "artifactKind": "plan",
            "hostModel": "claude-opus-5.5"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let plan_legs = ar_plan["legs"].as_array().unwrap();
    assert_eq!(plan_legs.len(), 2);
    assert!(plan_legs.iter().all(|l| l["lineage"] != "anthropic"));

    // Explicit swapPercent: 0 must be rejected with 400 Bad Request
    let zero_swap = client
        .post(format!("{base_url}/api/trials"))
        .json(&json!({
            "skill": "plan-tournament",
            "lineage": "openai",
            "challengerModel": "gpt-6.2-sol",
            "targetRuns": 6,
            "swapPercent": 0
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(zero_swap.status(), StatusCode::BAD_REQUEST);

    // Start a 50% swap Challenger trial in plan-tournament on openai
    let start_res = client
        .post(format!("{base_url}/api/trials"))
        .json(&json!({
            "skill": "plan-tournament",
            "lineage": "openai",
            "challengerModel": "gpt-6.2-sol",
            "targetRuns": 6,
            "swapPercent": 50,
            "allowPrDiff": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(start_res.status(), StatusCode::OK);
    let trial: Value = start_res.json().await.unwrap();
    let trial_id = trial["id"].as_i64().unwrap();
    assert_eq!(trial["championModel"], "gpt-6.1-sol");
    assert_eq!(trial["challengerModel"], "gpt-6.2-sol");

    // Replacing the roster with a different openai champion while the trial is open must fail with 409
    let conflict_roster = client
        .put(format!("{base_url}/api/roster/plan-tournament"))
        .json(&json!({
            "slots": [
                { "lineage": "anthropic", "championModel": "claude-opus-5.5", "sortOrder": 0, "enabled": true },
                { "lineage": "openai", "championModel": "gpt-6.2-sol", "sortOrder": 1, "enabled": true },
                { "lineage": "google", "championModel": "gemini-4-argon", "sortOrder": 2, "enabled": true }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(conflict_roster.status(), StatusCode::CONFLICT);

    // Run 6 plan-tournament selections (2 full lens cycles of 3 turns) and verify the
    // Challenger is swapped once per lens rather than phase-locking onto a single lens.
    let mut challenger_lenses = Vec::new();
    for expected_turn in 0..6 {
        let sel: Value = client
            .post(format!("{base_url}/api/panel/select"))
            .json(&json!({
                "skill": "plan-tournament",
                "artifactKind": "plan"
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(sel["rotationTurn"], expected_turn);
        for leg in sel["legs"].as_array().unwrap() {
            if leg["lineage"] == "openai" && leg["isChallenger"] == true {
                assert_eq!(leg["model"], "gpt-6.2-sol");
                assert_eq!(leg["trialId"], trial_id);
                challenger_lenses.push(leg["role"].as_str().unwrap().to_string());
            }
        }
    }
    challenger_lenses.sort();
    assert_eq!(
        challenger_lenses,
        vec![
            "clean_architecture".to_string(),
            "edge_cases".to_string(),
            "minimal_diff".to_string()
        ]
    );

    // Promote the Challenger and verify old champion moves to backupModel
    let decide_res = client
        .post(format!("{base_url}/api/trials/{trial_id}/decide"))
        .json(&json!({ "decision": "promote" }))
        .send()
        .await
        .unwrap();
    assert_eq!(decide_res.status(), StatusCode::OK);
    let decided: Value = decide_res.json().await.unwrap();
    assert_eq!(decided["status"], "promoted");

    let updated_roster: Value = client
        .get(format!("{base_url}/api/roster?skill=plan-tournament"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let openai_slot = updated_roster["roster"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["lineage"] == "openai")
        .expect("openai slot");
    assert_eq!(openai_slot["championModel"], "gpt-6.2-sol");
    assert_eq!(openai_slot["backupModel"], "gpt-6.1-sol");

    tdb.teardown().await;
}

#[tokio::test]
async fn arena_runs_overrides_timing_and_leaderboard_flow() {
    let Some(tdb) = TestDb::setup().await else {
        return;
    };
    let base_url = spawn_in_process_server(tdb.pool.clone(), true).await;

    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("cookie client");

    let reg = client
        .post(format!("{base_url}/api/auth/register"))
        .json(&json!({
            "email": "leaderboard@example.com",
            "password": "very-strong-passphrase",
            "name": "Leaderboard Tester"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reg.status(), StatusCode::CREATED);

    let slots = json!({
        "slots": [
            { "lineage": "anthropic", "championModel": "claude-opus-5.5", "backupModel": "claude-sonnet-5", "sortOrder": 0, "enabled": true },
            { "lineage": "openai", "championModel": "gpt-6.1-sol", "backupModel": "gpt-5.6-sol", "sortOrder": 1, "enabled": true },
            { "lineage": "google", "championModel": "gemini-4-argon", "backupModel": "gemini-4-carbon", "sortOrder": 2, "enabled": true }
        ]
    });
    for skill in ["adversarial-review", "plan-tournament"] {
        let res = client
            .put(format!("{base_url}/api/roster/{skill}"))
            .json(&slots)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    // Start an adversarial-review Challenger trial on openai (targetRuns: 3)
    let trial_resp: Value = client
        .post(format!("{base_url}/api/trials"))
        .json(&json!({
            "skill": "adversarial-review",
            "lineage": "openai",
            "challengerModel": "gpt-6.2-sol",
            "targetRuns": 3,
            "swapPercent": 50,
            "allowPrDiff": true
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let trial_id = trial_resp["id"].as_i64().unwrap();

    // Record 3 champion runs (gpt-6.1-sol takes 40s, claude-opus-5.5 takes 20s)
    // and 3 challenger runs (gpt-6.2-sol takes 20s, claude-opus-5.5 takes 30s)
    // Both gpt-6.1-sol and gpt-6.2-sol have identical quality (1 solo adopted catch each across 3 runs).
    for i in 1..=3 {
        let champ_run = client
            .post(format!("{base_url}/api/runs"))
            .json(&json!({
                "runKey": format!("ar_champ_{i}"),
                "skill": "adversarial-review",
                "artifactKind": "pr_diff",
                "repo": "robert-crandall/model-arena-rust",
                "hostModel": "gemini-4-argon",
                "approachVerdict": "keep",
                "clientSentAt": "2026-04-19T12:00:00Z",
                "legs": [
                    { "slotIndex": 0, "role": "reviewer", "lineage": "openai", "model": "gpt-6.1-sol", "trialId": trial_id, "status": "ok", "durationMs": 40000 },
                    { "slotIndex": 1, "role": "reviewer", "lineage": "anthropic", "model": "claude-opus-5.5", "status": "ok", "durationMs": 20000 }
                ],
                "candidates": if i == 1 {
                    json!([
                        { "summary": "Missing transaction lock", "severity": "blocking", "disposition": "adopted", "causedApproachPivot": true, "proposingModels": ["gpt-6.1-sol"] },
                        { "summary": "Speculative cache wrapper", "severity": "minor", "disposition": "adopted", "causedApproachPivot": false, "proposingModels": ["claude-opus-5.5"] }
                    ])
                } else if i == 2 {
                    json!([
                        { "summary": "Shared boundary check", "severity": "important", "disposition": "adopted", "causedApproachPivot": false, "proposingModels": ["gpt-6.1-sol", "claude-opus-5.5"] }
                    ])
                } else {
                    json!([])
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(champ_run.status(), StatusCode::OK);
    }

    // Verify out-of-order replay (older clientSentAt) is ignored without overwriting
    let stale_replay = client
        .post(format!("{base_url}/api/runs"))
        .json(&json!({
            "runKey": "ar_champ_1",
            "skill": "adversarial-review",
            "artifactKind": "pr_diff",
            "repo": "robert-crandall/model-arena-rust",
            "hostModel": "gemini-4-argon",
            "approachVerdict": "rethink",
            "clientSentAt": "2026-04-19T11:00:00Z",
            "legs": [
                { "slotIndex": 0, "role": "reviewer", "lineage": "openai", "model": "gpt-6.1-sol", "trialId": trial_id, "status": "ok", "durationMs": 40000 }
            ],
            "candidates": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale_replay.status(), StatusCode::OK);

    // Fetch leaderboard to get candidate IDs on ar_champ_1 and overturn the second candidate
    let lb_initial: Value = client
        .get(format!("{base_url}/api/leaderboard"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let champ_1_view = lb_initial["recentRuns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["runKey"] == "ar_champ_1")
        .expect("ar_champ_1 in recentRuns");
    assert_eq!(
        champ_1_view["approachVerdict"], "keep",
        "stale clientSentAt replay must not overwrite newer run"
    );
    let second_cand_id = champ_1_view["candidates"][1]["id"].as_i64().unwrap();

    let ov_res = client
        .post(format!(
            "{base_url}/api/candidates/{second_cand_id}/override"
        ))
        .json(&json!({
            "humanOverride": "overturned",
            "note": "False positive"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ov_res.status(), StatusCode::OK);

    // Re-submit ar_champ_1 with newer clientSentAt and reversed candidate order; override must stick!
    let reordered_update = client
        .post(format!("{base_url}/api/runs"))
        .json(&json!({
            "runKey": "ar_champ_1",
            "skill": "adversarial-review",
            "artifactKind": "pr_diff",
            "repo": "robert-crandall/model-arena-rust",
            "hostModel": "gemini-4-argon",
            "approachVerdict": "keep",
            "clientSentAt": "2026-04-19T12:05:00Z",
            "legs": [
                { "slotIndex": 0, "role": "reviewer", "lineage": "openai", "model": "gpt-6.1-sol", "trialId": trial_id, "status": "ok", "durationMs": 40000 },
                { "slotIndex": 1, "role": "reviewer", "lineage": "anthropic", "model": "claude-opus-5.5", "status": "ok", "durationMs": 20000 }
            ],
            "candidates": [
                { "summary": "Speculative cache wrapper", "severity": "minor", "disposition": "adopted", "causedApproachPivot": false, "proposingModels": ["claude-opus-5.5"] },
                { "summary": "Missing transaction lock", "severity": "blocking", "disposition": "adopted", "causedApproachPivot": true, "proposingModels": ["gpt-6.1-sol"] }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reordered_update.status(), StatusCode::OK);

    // Record 3 challenger runs; the 3rd run must auto-complete the trial
    for i in 1..=3 {
        let chall_res = client
            .post(format!("{base_url}/api/runs"))
            .json(&json!({
                "runKey": format!("ar_chall_{i}"),
                "skill": "adversarial-review",
                "artifactKind": "pr_diff",
                "repo": "robert-crandall/model-arena-rust",
                "hostModel": "gemini-4-argon",
                "approachVerdict": "keep",
                "legs": [
                    { "slotIndex": 0, "role": "reviewer", "lineage": "openai", "model": "gpt-6.2-sol", "trialId": trial_id, "status": "ok", "durationMs": 20000 },
                    { "slotIndex": 1, "role": "reviewer", "lineage": "anthropic", "model": "claude-opus-5.5", "status": "ok", "durationMs": 30000 }
                ],
                "candidates": if i == 1 {
                    json!([
                        { "summary": "Race condition in trial counter", "severity": "blocking", "disposition": "adopted", "causedApproachPivot": false, "proposingModels": ["gpt-6.2-sol"] }
                    ])
                } else {
                    json!([])
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(chall_res.status(), StatusCode::OK);
        let body: Value = chall_res.json().await.unwrap();
        if i < 3 {
            assert_eq!(body["completedTrials"].as_array().unwrap().len(), 0);
        } else {
            assert_eq!(body["completedTrials"].as_array().unwrap().len(), 1);
            assert_eq!(body["completedTrials"][0]["id"], trial_id);
            assert_eq!(body["completedTrials"][0]["status"], "completed");
        }
    }

    // Record a plan-tournament run to exercise lensMatrix and debiasedTournamentScore
    let pt_res = client
        .post(format!("{base_url}/api/runs"))
        .json(&json!({
            "runKey": "pt_1",
            "skill": "plan-tournament",
            "artifactKind": "plan",
            "repo": "robert-crandall/model-arena-rust",
            "hostModel": "gemini-4-argon",
            "winningRole": "minimal_diff",
            "legs": [
                { "slotIndex": 0, "role": "minimal_diff", "lineage": "anthropic", "model": "claude-opus-5.5", "status": "ok", "durationMs": 15000, "wonCorePlan": true, "cherryPicksCount": 1 },
                { "slotIndex": 1, "role": "clean_architecture", "lineage": "openai", "model": "gpt-6.1-sol", "status": "ok", "durationMs": 25000, "wonCorePlan": false, "cherryPicksCount": 2 },
                { "slotIndex": 2, "role": "edge_cases", "lineage": "google", "model": "gemini-4-argon", "status": "ok", "durationMs": 18000, "wonCorePlan": false, "cherryPicksCount": 0 }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(pt_res.status(), StatusCode::OK);

    // Inspect the leaderboard
    let lb: Value = client
        .get(format!("{base_url}/api/leaderboard?days=30"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(lb["totalRuns"], 7);

    // Verify candidate override survived reordering on ar_champ_1
    let champ_1_after = lb["recentRuns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["runKey"] == "ar_champ_1")
        .expect("ar_champ_1");
    assert_eq!(
        champ_1_after["candidates"][0]["summary"],
        "Speculative cache wrapper"
    );
    assert_eq!(
        champ_1_after["candidates"][0]["humanOverride"],
        "overturned"
    );
    assert_eq!(
        champ_1_after["candidates"][0]["effectiveDisposition"],
        "set_aside"
    );

    // Verify timing metrics (#4) on gpt-6.1-sol (3 AR runs @ 40s + 1 PT run @ 25s -> 4 timed runs, straggler in all 4)
    let sol_61 = lb["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == "gpt-6.1-sol")
        .expect("gpt-6.1-sol scorecard");
    assert_eq!(sol_61["timedRuns"], 4);
    assert_eq!(sol_61["p50DurationMs"], 40000);
    assert_eq!(sol_61["p90DurationMs"], 40000);
    assert_eq!(sol_61["stragglerCount"], 4);
    assert_eq!(sol_61["stragglerRate"], 1.0);
    assert_eq!(sol_61["soloCatches"], 1);
    assert_eq!(sol_61["sharedCatches"], 1);
    assert_eq!(sol_61["catchesPerMinute"], 1.0); // 2 catches across 120s (2.0 minutes) of review runs

    // Verify speed-weighed Challenger trial verdict (#4): quality parity + 2.0x faster p50 -> promote
    // Also verify cross-lineage shared catch stays shared (not inflated to solo) inside trial comparison
    let tc = &lb["trials"][0];
    assert_eq!(tc["trial"]["id"], trial_id);
    assert_eq!(tc["champion"]["soloCatches"], 1);
    assert_eq!(tc["champion"]["sharedCatches"], 1);
    assert_eq!(tc["verdictHint"], "promote");
    assert_eq!(tc["p50DeltaMs"], -20000);
    assert_eq!(tc["speedupRatio"], 2.0);
    assert!(
        tc["verdictNotes"]
            .as_str()
            .unwrap()
            .contains("faster p50 latency")
    );

    tdb.teardown().await;
}

#[tokio::test]
async fn arena_coding_runs_overrides_and_coding_leaderboard_flow() {
    let Some(tdb) = TestDb::setup().await else {
        eprintln!("skipping Postgres integration test (TEST_DATABASE_URL not set)");
        return;
    };
    let base_url = spawn_in_process_server(tdb.pool.clone(), true).await;

    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("cookie client");

    let reg = client
        .post(format!("{base_url}/api/auth/register"))
        .json(&json!({
            "email": "coder@example.com",
            "password": "correct-horse-battery-staple",
            "name": "Coder"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reg.status(), StatusCode::CREATED);

    // 1. Record a small bugfix PR (S: 20 + 10 = 30 LOC) by claude-opus-5.5
    let rec1_resp = client
        .post(format!("{base_url}/api/coding-runs"))
        .json(&json!({
            "runKey": "pr_model_arena_101",
            "model": "claude-opus-5.5",
            "harness": "copilot-cli",
            "repo": "robert-crandall/model-arena-rust",
            "prNumber": 101,
            "taskKind": "bugfix",
            "locAdded": 20,
            "locDeleted": 10,
            "filesChanged": 2,
            "testsDelta": 15,
            "promptTurnCount": 1,
            "redCiPushes": 0,
            "ccrRounds": 1,
            "ccrActionableCount": 0,
            "ccrNoiseCount": 1,
            "humanThreadsCount": 0,
            "escapedRegression": false,
            "mergedWithoutFollowup24h": true,
            "firstPassClean": true,
            "abandoned": false,
            "timeToGreenSec": 180,
            "timeToMergeSec": 420,
            "digestSummary": "Zero-touch bugfix, 1 noisy CCR nit pushed back",
            "clientSentAt": "2026-04-01T12:00:00Z"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(rec1_resp.status(), StatusCode::OK);
    let rec1: Value = rec1_resp.json().await.unwrap();
    let run1_id = rec1["runId"].as_i64().expect("run1Id");
    assert_eq!(rec1["sizeBucket"], "S");

    // 2. Record a medium feature PR (M: 120 + 30 = 150 LOC) by claude-opus-5.5 that needed 2 turns and 1 actionable CCR fix
    let rec2_resp = client
        .post(format!("{base_url}/api/coding-runs"))
        .json(&json!({
            "runKey": "pr_model_arena_102",
            "model": "claude-opus-5.5",
            "harness": "t3-code",
            "repo": "robert-crandall/model-arena-rust",
            "prNumber": 102,
            "taskKind": "feature",
            "locAdded": 120,
            "locDeleted": 30,
            "filesChanged": 4,
            "testsDelta": 45,
            "promptTurnCount": 2,
            "redCiPushes": 1,
            "ccrRounds": 2,
            "ccrActionableCount": 1,
            "ccrNoiseCount": 1,
            "humanThreadsCount": 1,
            "escapedRegression": false,
            "mergedWithoutFollowup24h": true,
            "firstPassClean": false,
            "abandoned": false,
            "timeToGreenSec": 300,
            "timeToMergeSec": 900,
            "digestSummary": "1 steering turn + 1 actionable CCR fix"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(rec2_resp.status(), StatusCode::OK);
    let rec2: Value = rec2_resp.json().await.unwrap();
    let run2_id = rec2["runId"].as_i64().expect("run2Id");
    assert_eq!(rec2["sizeBucket"], "M");

    // 3. Stale replay on run 1 is ignored without overwriting
    let stale_resp = client
        .post(format!("{base_url}/api/coding-runs"))
        .json(&json!({
            "runKey": "pr_model_arena_101",
            "model": "claude-opus-5.5",
            "taskKind": "bugfix",
            "locAdded": 400,
            "locDeleted": 100,
            "abandoned": true,
            "clientSentAt": "2026-04-01T11:00:00Z"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale_resp.status(), StatusCode::OK);
    let stale: Value = stale_resp.json().await.unwrap();
    assert_eq!(stale["runId"], run1_id);
    assert_eq!(stale["sizeBucket"], "S");

    // 4. Override run 2 as "overturned" (human discovered an escaped regression after merge)
    let ovr_resp = client
        .post(format!("{base_url}/api/coding-runs/{run2_id}/override"))
        .json(&json!({
            "humanOverride": "overturned",
            "note": "Broke pagination cursor under empty slice"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ovr_resp.status(), StatusCode::OK);
    let ovr: Value = ovr_resp.json().await.unwrap();
    assert_eq!(ovr["humanOverride"], "overturned");

    // 5. Query coding leaderboard and verify multi-axis profile + slice breakdown
    let lb_resp = client
        .get(format!("{base_url}/api/coding-leaderboard?days=90"))
        .send()
        .await
        .unwrap();
    assert_eq!(lb_resp.status(), StatusCode::OK);
    let lb: Value = lb_resp.json().await.unwrap();
    assert_eq!(lb["totalRuns"], 2);
    let opus = &lb["models"][0];
    assert_eq!(opus["model"], "claude-opus-5.5");
    assert_eq!(opus["lineage"], "anthropic");
    assert_eq!(opus["totalRuns"], 2);
    assert_eq!(opus["mergedRuns"], 2);
    assert_eq!(opus["zeroTouchRuns"], 1);
    assert_eq!(opus["zeroTouchRate"], 0.5);
    assert_eq!(opus["firstPassCleanRuns"], 1);
    assert_eq!(opus["firstPassCleanRate"], 0.5);
    // Run 2 was overturned, so only Run 1 counts as mergedClean24h, and Run 2 counts as escapedRegression
    assert_eq!(opus["mergedClean24hRuns"], 1);
    assert_eq!(opus["mergedClean24hRate"], 0.5);
    assert_eq!(opus["escapedRegressions"], 1);
    assert_eq!(opus["escapedRegressionRate"], 0.5);
    assert_eq!(opus["totalCcrActionable"], 1);
    assert_eq!(opus["totalCcrNoise"], 2);
    assert_eq!(opus["ccrNoiseRatio"], 0.667);
    assert_eq!(opus["p50TimeToGreenSec"], 240);
    assert_eq!(opus["p50TimeToMergeSec"], 660);

    // Slice filter check: sizeBucket=S returns only the S bugfix run
    let lb_s_resp = client
        .get(format!(
            "{base_url}/api/coding-leaderboard?sizeBucket=S&taskKind=bugfix"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(lb_s_resp.status(), StatusCode::OK);
    let lb_s: Value = lb_s_resp.json().await.unwrap();
    assert_eq!(lb_s["totalRuns"], 1);
    assert_eq!(lb_s["slices"][0]["sizeBucket"], "S");
    assert_eq!(lb_s["slices"][0]["taskKind"], "bugfix");
    assert_eq!(lb_s["slices"][0]["zeroTouchRate"], 1.0);

    // 6. Override run 2 as "confirmed" (clearing the overturned regression) and verify clean rates restore
    let ovr_confirm = client
        .post(format!("{base_url}/api/coding-runs/{run2_id}/override"))
        .json(&json!({
            "humanOverride": "confirmed",
            "note": "Follow-up commit was unrelated doc tweak"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ovr_confirm.status(), StatusCode::OK);
    let lb_after: Value = client
        .get(format!("{base_url}/api/coding-leaderboard?days=90"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(lb_after["models"][0]["escapedRegressions"], 0);
    assert_eq!(lb_after["models"][0]["mergedClean24hRuns"], 2);
    assert_eq!(lb_after["models"][0]["mergedClean24hRate"], 1.0);

    tdb.teardown().await;
}

#[tokio::test]
async fn arena_mcp_cli_spool_and_backfill_flow() {
    use app::arena::{
        CodingRunInput, RecordRunInput, RunLegInput, SpoolScope, flush_spool,
        submit_or_spool_coding_run, submit_or_spool_run,
    };

    let Some(tdb) = TestDb::setup().await else {
        eprintln!("skipping Postgres integration test (TEST_DATABASE_URL not set)");
        return;
    };
    let base_url = spawn_in_process_server(tdb.pool.clone(), true).await;

    let http = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("cookie client");

    let reg = http
        .post(format!("{base_url}/api/auth/register"))
        .json(&json!({
            "email": "mcp-arena@example.com",
            "password": "mcp-strong-passphrase",
            "name": "MCP Arena"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reg.status(), StatusCode::CREATED);

    let minted = mint_token_via_login(
        &base_url,
        "mcp-arena@example.com",
        "mcp-strong-passphrase",
        "mcp-arena-cli",
    )
    .await
    .expect("mint token");

    let temp_dir = tempfile::tempdir().expect("tempdir");
    let cfg_path = temp_dir.path().join("model-arena-rust.json");
    let live_cfg = ClientConfig {
        app_url: base_url.clone(),
        token: minted.token.clone(),
    };
    live_cfg.write_atomic(&cfg_path).expect("write config");

    // 1. Offline spooling when server is unreachable, followed by flush_spool against live server
    let spool_root = temp_dir.path().join("spool");
    let dead_cfg = ClientConfig {
        app_url: "http://127.0.0.1:1".to_string(),
        token: minted.token.clone(),
    };
    let dead_client = app::apiclient::ApiClient::new(&dead_cfg).unwrap();
    let spool_scope = SpoolScope::with_base_dir(&dead_cfg, Some(spool_root.clone()));

    let spooled_run = submit_or_spool_run(
        &dead_client,
        &spool_scope,
        RecordRunInput {
            run_key: "spooled_ar_1".to_string(),
            skill: "adversarial-review".to_string(),
            artifact_kind: "pr_diff".to_string(),
            artifact_ref: "robert-crandall/model-arena-rust#5".to_string(),
            repo: "robert-crandall/model-arena-rust".to_string(),
            phase: "independent".to_string(),
            host_model: "gemini-4-argon".to_string(),
            host_reasoning_effort: String::new(),
            host_lineage: String::new(),
            harness: "copilot-cli".to_string(),
            machine: "test-host".to_string(),
            approach_verdict: "keep".to_string(),
            winning_role: String::new(),
            occurred_at: None,
            client_sent_at: None,
            queue_only: false,
            legs: vec![RunLegInput {
                slot_index: 0,
                role: "reviewer".to_string(),
                lineage: "openai".to_string(),
                model: "gpt-6.1-sol".to_string(),
                reasoning_effort: String::new(),
                is_challenger: false,
                trial_id: None,
                status: "ok".to_string(),
                duration_ms: 32_000,
                won_core_plan: false,
                cherry_picks_count: 0,
                critic_adopted_count: 0,
                critic_bloat_dropped_count: 0,
            }],
            candidates: Vec::new(),
        },
    )
    .await
    .expect("spool run offline");
    assert!(spooled_run.spooled);

    let spooled_coding = submit_or_spool_coding_run(
        &dead_client,
        &spool_scope,
        CodingRunInput {
            run_key: "spooled_cr_1".to_string(),
            model: "claude-opus-5.5".to_string(),
            reasoning_effort: "high".to_string(),
            lineage: String::new(),
            harness: "t3-code".to_string(),
            repo: "robert-crandall/model-arena-rust".to_string(),
            pr_number: 55,
            issue_ref: String::new(),
            task_kind: "feature".to_string(),
            loc_added: 60,
            loc_deleted: 15,
            files_changed: 3,
            tests_delta: 25,
            prompt_turn_count: 1,
            red_ci_pushes: 0,
            ccr_rounds: 1,
            ccr_actionable_count: 0,
            ccr_noise_count: 0,
            human_threads_count: 0,
            escaped_regression: false,
            merged_without_followup_24h: true,
            first_pass_clean: true,
            abandoned: false,
            time_to_green_sec: 120,
            time_to_merge_sec: 300,
            digest_summary: "Clean spooled coding run".to_string(),
            occurred_at: None,
            client_sent_at: None,
            queue_only: false,
        },
    )
    .await
    .expect("spool coding run offline");
    assert!(spooled_coding.spooled);
    assert_eq!(spooled_coding.size_bucket, "M");

    // Flush the spooled files to the live server
    let live_client = app::apiclient::ApiClient::new(&live_cfg).unwrap();
    let flush_res = flush_spool(&live_client, &spool_scope).await;
    assert_eq!(flush_res.flushed, 2);
    assert_eq!(flush_res.pending, 0);

    // 2. Verify `model-arena-rust-mcp list` exposes all 15 tools
    let mcp_bin = env!("CARGO_BIN_EXE_model-arena-rust-mcp");
    let list_out = Command::new(mcp_bin)
        .arg("list")
        .arg("--config")
        .arg(&cfg_path)
        .output()
        .await
        .expect("run mcp list");
    assert!(list_out.status.success());
    let list_json: Value = serde_json::from_slice(&list_out.stdout).expect("parse mcp list json");
    let tool_names: Vec<&str> = list_json["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "whoami",
        "list_users",
        "select_panel",
        "record_run",
        "flush_spool",
        "get_leaderboard",
        "list_roster",
        "set_roster",
        "start_trial",
        "decide_trial",
        "override_candidate",
        "record_coding_run",
        "override_coding_run",
        "get_coding_leaderboard",
        "backfill_sessions",
    ] {
        assert!(
            tool_names.contains(&expected),
            "missing tool '{expected}' in list: {tool_names:?}"
        );
    }

    // 3. Create synthetic ~/.copilot/session-state/<id>/events.jsonl and run `call backfill_sessions`
    let session_root = temp_dir.path().join("session-state");
    let sess_dir = session_root.join("sess-backfill-1");
    std::fs::create_dir_all(&sess_dir).unwrap();
    let base_ts = chrono::Utc::now() - chrono::Duration::hours(2);
    let ts = |offset_secs: i64| (base_ts + chrono::Duration::seconds(offset_secs)).to_rfc3339();
    let events_jsonl = [
        json!({"type":"session.start","timestamp":ts(0),"data":{"startTime":ts(0),"selectedModel":"gemini-4-argon"}}).to_string(),
        json!({"type":"user.message","timestamp":ts(1),"data":{"content":"Run plan tournament and review"}}).to_string(),
        json!({"type":"tool.execution_start","timestamp":ts(2),"data":{"toolCallId":"pt_a","toolName":"task","arguments":{"name":"planner-a","description":"Minimal diff planner","agent_type":"general-purpose","model":"claude-opus-5.5"}}}).to_string(),
        json!({"type":"tool.execution_start","timestamp":ts(3),"data":{"toolCallId":"pt_b","toolName":"task","arguments":{"name":"planner-b","description":"Clean architecture planner","agent_type":"general-purpose","model":"gpt-6.1-sol"}}}).to_string(),
        json!({"type":"tool.execution_complete","timestamp":ts(22),"data":{"toolCallId":"pt_a","success":true,"result":"plan A"}}).to_string(),
        json!({"type":"tool.execution_complete","timestamp":ts(38),"data":{"toolCallId":"pt_b","success":true,"result":"plan B"}}).to_string(),
        json!({"type":"assistant.message","timestamp":ts(45),"data":{"content":"### Hybrid Plan\nWinning core plan: Planner A (claude-opus-5.5) on robert-crandall/model-arena-rust#8.\nCherry-picks from gpt-6.1-sol for transaction locking."}}).to_string(),
        json!({"type":"user.message","timestamp":ts(300),"data":{"content":"Skip this delegated review"}}).to_string(),
        json!({"type":"tool.execution_start","timestamp":ts(301),"data":{"toolCallId":"skip_1","toolName":"bash","arguments":{"command":"true \"model-arena:skip\""}}}).to_string(),
        json!({"type":"assistant.message","timestamp":ts(310),"data":{"content":"### Adversarial review (gpt-6.1-sol, claude-opus-5.5)\n- **Approach:** `keep`\n- **Findings:** none\n- **Set aside:** none"}}).to_string(),
    ]
    .join("\n");
    std::fs::write(sess_dir.join("events.jsonl"), events_jsonl).unwrap();

    let bf_input = json!({
        "sessionDir": session_root.to_string_lossy(),
        "days": 30,
        "dryRun": false
    })
    .to_string();
    let bf_out = Command::new(mcp_bin)
        .env("MODEL_ARENA_SPOOL_DIR", &spool_root)
        .env_remove("MCP_APP_URL")
        .env_remove("MCP_APP_TOKEN")
        .args([
            "call",
            "backfill_sessions",
            "--input",
            &bf_input,
            "--config",
        ])
        .arg(&cfg_path)
        .output()
        .await
        .expect("run mcp call backfill_sessions");
    assert!(
        bf_out.status.success(),
        "backfill_sessions failed: {}",
        String::from_utf8_lossy(&bf_out.stderr)
    );
    let bf_summary: Value = serde_json::from_slice(&bf_out.stdout).expect("parse backfill summary");
    assert_eq!(bf_summary["sessionsScanned"], 1);
    assert_eq!(bf_summary["runsFound"], 1);
    assert_eq!(bf_summary["runsSubmitted"], 1);

    // 4. Verify both the spooled run and the backfilled plan-tournament run appear in `call get_leaderboard`
    let lb_out = Command::new(mcp_bin)
        .env("MODEL_ARENA_SPOOL_DIR", &spool_root)
        .env_remove("MCP_APP_URL")
        .env_remove("MCP_APP_TOKEN")
        .args([
            "call",
            "get_leaderboard",
            "--input",
            r#"{"days":90}"#,
            "--config",
        ])
        .arg(&cfg_path)
        .output()
        .await
        .expect("run mcp call get_leaderboard");
    assert!(lb_out.status.success());
    let lb: Value = serde_json::from_slice(&lb_out.stdout).expect("parse leaderboard json");
    assert_eq!(lb["totalRuns"], 2);
    let sol = lb["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == "gpt-6.1-sol")
        .expect("gpt-6.1-sol in leaderboard");
    assert_eq!(sol["timedRuns"], 2); // 32s from spooled AR run + 35s from backfilled PT run
    assert_eq!(sol["cherryPicks"], 1);

    tdb.teardown().await;
}

#[tokio::test]
async fn arena_reasoning_effort_trials_and_leaderboards_flow() {
    let Some(tdb) = TestDb::setup().await else {
        eprintln!("skipping Postgres integration test (TEST_DATABASE_URL not set)");
        return;
    };
    let base_url = spawn_in_process_server(tdb.pool.clone(), true).await;

    let client = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .expect("cookie client");

    let reg = client
        .post(format!("{base_url}/api/auth/register"))
        .json(&json!({
            "email": "effort@example.com",
            "password": "very-strong-passphrase",
            "name": "Effort Tester"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(reg.status(), StatusCode::CREATED);

    // 1. Invalid reasoning effort is rejected with 400
    let bad_roster = client
        .put(format!("{base_url}/api/roster/adversarial-review"))
        .json(&json!({
            "slots": [
                { "lineage": "openai", "championModel": "gpt-6.2-sol", "championReasoningEffort": "turbo", "sortOrder": 0, "enabled": true }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_roster.status(), StatusCode::BAD_REQUEST);

    // 2. Valid roster with champion + backup reasoning efforts
    let put_roster = client
        .put(format!("{base_url}/api/roster/adversarial-review"))
        .json(&json!({
            "slots": [
                {
                    "lineage": "openai",
                    "championModel": "gpt-6.2-sol",
                    "championReasoningEffort": "high",
                    "backupModel": "gpt-6.1-sol",
                    "backupReasoningEffort": "medium",
                    "sortOrder": 0,
                    "enabled": true
                },
                {
                    "lineage": "anthropic",
                    "championModel": "claude-opus-5.5",
                    "championReasoningEffort": "high",
                    "backupModel": "claude-sonnet-5",
                    "backupReasoningEffort": "",
                    "sortOrder": 1,
                    "enabled": true
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(put_roster.status(), StatusCode::OK);

    // 3. Same-model + same-effort trial must fail with 400
    let same_tuple = client
        .post(format!("{base_url}/api/trials"))
        .json(&json!({
            "skill": "adversarial-review",
            "lineage": "openai",
            "challengerModel": "gpt-6.2-sol",
            "challengerReasoningEffort": "high",
            "targetRuns": 2,
            "swapPercent": 100
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(same_tuple.status(), StatusCode::BAD_REQUEST);

    // 4. Same-model + different-effort trial (high vs xhigh) succeeds
    let start_trial = client
        .post(format!("{base_url}/api/trials"))
        .json(&json!({
            "skill": "adversarial-review",
            "lineage": "openai",
            "challengerModel": "gpt-6.2-sol",
            "challengerReasoningEffort": "xhigh",
            "targetRuns": 1,
            "swapPercent": 100,
            "allowPrDiff": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(start_trial.status(), StatusCode::OK);
    let trial: Value = start_trial.json().await.unwrap();
    let trial_id = trial["id"].as_i64().unwrap();
    assert_eq!(trial["championReasoningEffort"], "high");
    assert_eq!(trial["challengerReasoningEffort"], "xhigh");

    // 5. select_panel returns challenger effort and backup effort
    let panel: Value = client
        .post(format!("{base_url}/api/panel/select"))
        .json(&json!({
            "skill": "adversarial-review",
            "artifactKind": "pr_diff",
            "hostModel": "gemini-4-neon"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let openai_leg = panel["legs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["lineage"] == "openai")
        .expect("openai leg");
    assert_eq!(openai_leg["model"], "gpt-6.2-sol");
    assert_eq!(openai_leg["reasoningEffort"], "xhigh");
    assert_eq!(openai_leg["backupReasoningEffort"], "medium");
    assert_eq!(openai_leg["isChallenger"], true);

    // 6. Ambiguous same-model conflicting-effort reviewers in one run are rejected with 400
    let ambiguous = client
        .post(format!("{base_url}/api/runs"))
        .json(&json!({
            "runKey": "ar_ambiguous_effort",
            "skill": "adversarial-review",
            "artifactKind": "pr_diff",
            "hostModel": "gemini-4-neon",
            "legs": [
                { "slotIndex": 0, "role": "reviewer", "lineage": "openai", "model": "gpt-6.2-sol", "reasoningEffort": "high", "status": "ok" },
                { "slotIndex": 1, "role": "reviewer", "lineage": "openai", "model": "gpt-6.2-sol", "reasoningEffort": "xhigh", "status": "ok" }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(ambiguous.status(), StatusCode::BAD_REQUEST);

    // 7. Record one champion (high) run and one challenger (xhigh) run for the same model
    for (run_key, effort, duration_ms) in [
        ("ar_eff_champ", "high", 20_000),
        ("ar_eff_chall", "xhigh", 35_000),
    ] {
        let rec = client
            .post(format!("{base_url}/api/runs"))
            .json(&json!({
                "runKey": run_key,
                "skill": "adversarial-review",
                "artifactKind": "pr_diff",
                "hostModel": "gemini-4-neon",
                "hostReasoningEffort": "high",
                "approachVerdict": "keep",
                "legs": [
                    {
                        "slotIndex": 0,
                        "role": "reviewer",
                        "lineage": "openai",
                        "model": "gpt-6.2-sol",
                        "reasoningEffort": effort,
                        "trialId": trial_id,
                        "status": "ok",
                        "durationMs": duration_ms
                    }
                ],
                "candidates": [
                    {
                        "summary": format!("Finding from {effort}"),
                        "severity": "important",
                        "disposition": "adopted",
                        "proposingModels": ["gpt-6.2-sol"]
                    }
                ]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(rec.status(), StatusCode::OK);
    }

    // Leaderboard splits (gpt-6.2-sol, high) and (gpt-6.2-sol, xhigh) into separate scorecards
    let lb: Value = client
        .get(format!("{base_url}/api/leaderboard?days=30"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let high_sc = lb["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == "gpt-6.2-sol" && m["reasoningEffort"] == "high")
        .expect("high scorecard");
    let xhigh_sc = lb["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == "gpt-6.2-sol" && m["reasoningEffort"] == "xhigh")
        .expect("xhigh scorecard");
    assert_eq!(high_sc["soloCatches"], 1);
    assert_eq!(xhigh_sc["soloCatches"], 1);
    assert_eq!(high_sc["p50DurationMs"], 20_000);
    assert_eq!(xhigh_sc["p50DurationMs"], 35_000);

    // Trial comparison splits champion (high) vs challenger (xhigh) separately
    let comparison = &lb["trials"][0];
    assert_eq!(comparison["trial"]["id"], trial_id);
    assert_eq!(comparison["champion"]["reasoningEffort"], "high");
    assert_eq!(comparison["challenger"]["reasoningEffort"], "xhigh");
    assert_eq!(comparison["champion"]["soloCatches"], 1);
    assert_eq!(comparison["challenger"]["soloCatches"], 1);
    assert_eq!(comparison["champion"]["p50DurationMs"], 20_000);
    assert_eq!(comparison["challenger"]["p50DurationMs"], 35_000);

    // 7b. Failed primary at xhigh + retried fallback at high for same model is accepted,
    // and adopted findings credit the successful fallback effort only.
    let fallback_rec = client
        .post(format!("{base_url}/api/runs"))
        .json(&json!({
            "runKey": "ar_eff_fallback",
            "skill": "adversarial-review",
            "artifactKind": "pr_diff",
            "hostModel": "gemini-4-neon",
            "approachVerdict": "keep",
            "legs": [
                { "slotIndex": 0, "role": "reviewer", "lineage": "openai", "model": "gpt-6.2-sol", "reasoningEffort": "xhigh", "status": "timeout" },
                { "slotIndex": 1, "role": "reviewer", "lineage": "openai", "model": "gpt-6.2-sol", "reasoningEffort": "high", "status": "retried", "durationMs": 22_000 }
            ],
            "candidates": [
                {
                    "summary": "Fallback catch",
                    "severity": "important",
                    "disposition": "adopted",
                    "proposingModels": ["gpt-6.2-sol"]
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(fallback_rec.status(), StatusCode::OK);

    let lb_after_fb: Value = client
        .get(format!("{base_url}/api/leaderboard?days=30"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let high_after_fb = lb_after_fb["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == "gpt-6.2-sol" && m["reasoningEffort"] == "high")
        .expect("high scorecard after fallback");
    let xhigh_after_fb = lb_after_fb["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model"] == "gpt-6.2-sol" && m["reasoningEffort"] == "xhigh")
        .expect("xhigh scorecard after fallback");
    assert_eq!(high_after_fb["soloCatches"], 2);
    assert_eq!(xhigh_after_fb["soloCatches"], 1);
    assert_eq!(xhigh_after_fb["failuresCount"], 1);

    // Host-judge affinity matrix splits cells by both host and reviewer reasoningEffort
    let bias_cells = lb_after_fb["hostBiasMatrix"].as_array().unwrap();
    assert_eq!(bias_cells.len(), 3);
    assert!(bias_cells.iter().any(|c| {
        c["hostModel"] == "gemini-4-neon"
            && c["hostReasoningEffort"] == "high"
            && c["reviewerModel"] == "gpt-6.2-sol"
            && c["reviewerReasoningEffort"] == "high"
            && c["candidatesEvaluated"] == 1
            && c["adoptedCount"] == 1
    }));
    assert!(bias_cells.iter().any(|c| {
        c["hostModel"] == "gemini-4-neon"
            && c["hostReasoningEffort"] == "high"
            && c["reviewerModel"] == "gpt-6.2-sol"
            && c["reviewerReasoningEffort"] == "xhigh"
            && c["candidatesEvaluated"] == 1
            && c["adoptedCount"] == 1
    }));
    assert!(bias_cells.iter().any(|c| {
        c["hostModel"] == "gemini-4-neon"
            && c["hostReasoningEffort"] == ""
            && c["reviewerModel"] == "gpt-6.2-sol"
            && c["reviewerReasoningEffort"] == "high"
            && c["candidatesEvaluated"] == 1
            && c["adoptedCount"] == 1
    }));

    // 7d. Changing only championReasoningEffort during an open trial returns 409 Conflict
    let effort_conflict = client
        .put(format!("{base_url}/api/roster/adversarial-review"))
        .json(&json!({
            "slots": [
                {
                    "lineage": "openai",
                    "championModel": "gpt-6.2-sol",
                    "championReasoningEffort": "medium",
                    "backupModel": "gpt-6.1-sol",
                    "backupReasoningEffort": "medium",
                    "sortOrder": 0,
                    "enabled": true
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(effort_conflict.status(), StatusCode::CONFLICT);

    // 7c. Plan-tournament lens matrix splits same model across reasoning efforts
    for (run_key, effort, won, picks) in [
        ("pt_eff_high", "high", false, 1),
        ("pt_eff_xhigh", "xhigh", true, 0),
    ] {
        let rec = client
            .post(format!("{base_url}/api/runs"))
            .json(&json!({
                "runKey": run_key,
                "skill": "plan-tournament",
                "artifactKind": "plan",
                "hostModel": "gemini-4-neon",
                "winningRole": if won { "edge_cases" } else { "minimal_diff" },
                "legs": [
                    {
                        "slotIndex": 0,
                        "role": "edge_cases",
                        "lineage": "openai",
                        "model": "gpt-6.2-sol",
                        "reasoningEffort": effort,
                        "status": "ok",
                        "wonCorePlan": won,
                        "cherryPicksCount": picks
                    }
                ]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(rec.status(), StatusCode::OK);
    }
    let pt_lb: Value = client
        .get(format!(
            "{base_url}/api/leaderboard?skill=plan-tournament&days=30"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let lens_cells = pt_lb["lensMatrix"].as_array().unwrap();
    let high_cell = lens_cells
        .iter()
        .find(|c| {
            c["model"] == "gpt-6.2-sol"
                && c["reasoningEffort"] == "high"
                && c["lens"] == "edge_cases"
        })
        .expect("high lens cell");
    let xhigh_cell = lens_cells
        .iter()
        .find(|c| {
            c["model"] == "gpt-6.2-sol"
                && c["reasoningEffort"] == "xhigh"
                && c["lens"] == "edge_cases"
        })
        .expect("xhigh lens cell");
    assert_eq!(high_cell["wins"], 0);
    assert_eq!(high_cell["cherryPicks"], 1);
    assert_eq!(xhigh_cell["wins"], 1);
    assert_eq!(xhigh_cell["cherryPicks"], 0);

    // 8. Promoting the effort trial rotates (gpt-6.2-sol, high) to backup and promotes (gpt-6.2-sol, xhigh)
    let decide = client
        .post(format!("{base_url}/api/trials/{trial_id}/decide"))
        .json(&json!({ "decision": "promote" }))
        .send()
        .await
        .unwrap();
    assert_eq!(decide.status(), StatusCode::OK);

    let roster_after: Value = client
        .get(format!("{base_url}/api/roster?skill=adversarial-review"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let openai_slot = roster_after["roster"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["lineage"] == "openai")
        .expect("openai slot");
    assert_eq!(openai_slot["championModel"], "gpt-6.2-sol");
    assert_eq!(openai_slot["championReasoningEffort"], "xhigh");
    assert_eq!(openai_slot["backupModel"], "gpt-6.2-sol");
    assert_eq!(openai_slot["backupReasoningEffort"], "high");

    // 9. Coding leaderboard splits same model by reasoningEffort
    for (run_key, effort) in [("cr_eff_1", "medium"), ("cr_eff_2", "high")] {
        let rec = client
            .post(format!("{base_url}/api/coding-runs"))
            .json(&json!({
                "runKey": run_key,
                "model": "claude-opus-5.5",
                "reasoningEffort": effort,
                "taskKind": "feature",
                "locAdded": 40,
                "locDeleted": 10,
                "firstPassClean": true,
                "mergedWithoutFollowup24h": true
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(rec.status(), StatusCode::OK);
    }

    let coding_lb: Value = client
        .get(format!("{base_url}/api/coding-leaderboard?days=30"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(coding_lb["models"].as_array().unwrap().len(), 2);
    let slices = coding_lb["slices"].as_array().unwrap();
    assert_eq!(slices.len(), 2);
    assert!(
        slices
            .iter()
            .any(|s| s["model"] == "claude-opus-5.5" && s["reasoningEffort"] == "medium")
    );
    assert!(
        slices
            .iter()
            .any(|s| s["model"] == "claude-opus-5.5" && s["reasoningEffort"] == "high")
    );

    tdb.teardown().await;
}
