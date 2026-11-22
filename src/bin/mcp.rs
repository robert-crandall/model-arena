use std::env;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use app::apiclient::{APP_SLUG, ApiClient, ClientConfig, mint_token_via_login};
use app::arena::{
    BackfillArgs, CodingLeaderboardQuery, CodingRunInput, LeaderboardQuery, OverrideCandidateInput,
    OverrideCodingRunInput, RecordRunInput, RosterSlotInput, SelectPanelInput, SpoolScope,
    StartTrialInput, flush_spool, run_backfill, submit_or_spool_coding_run, submit_or_spool_run,
};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ListRosterArgs {
    /// Optional filter: adversarial-review or plan-tournament.
    #[serde(default)]
    skill: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct SetRosterArgs {
    /// Skill whose roster is replaced: adversarial-review or plan-tournament.
    pub skill: String,
    /// Ordered lineage slots for this skill.
    #[serde(default)]
    pub slots: Vec<RosterSlotInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct DecideTrialArgs {
    /// Trial ID to resolve.
    pub trial_id: i64,
    /// Decision: promote or retire.
    pub decision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct OverrideCandidateArgs {
    /// Candidate ID to confirm or overturn.
    pub candidate_id: i64,
    /// Override value: confirmed, overturned, or clear.
    pub human_override: String,
    /// Optional note explaining why the verdict was overridden.
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct OverrideCodingRunArgs {
    /// Coding run ID to confirm or overturn.
    pub coding_run_id: i64,
    /// Override value: confirmed, overturned, or clear.
    pub human_override: String,
    /// Optional note explaining why the coding run outcome was overridden.
    #[serde(default)]
    pub note: String,
}

#[derive(Clone)]
struct AppMcp {
    client: ApiClient,
    spool_scope: Arc<SpoolScope>,
    tool_router: ToolRouter<Self>,
}

fn json_tool_result<T: Serialize>(res: Result<T, String>) -> Result<CallToolResult, McpError> {
    match res {
        Ok(value) => {
            let json_val = serde_json::to_value(&value)
                .map_err(|err| McpError::internal_error(err.to_string(), None))?;
            let text = serde_json::to_string_pretty(&json_val)
                .map_err(|err| McpError::internal_error(err.to_string(), None))?;
            let mut out = CallToolResult::success(vec![ContentBlock::text(text)]);
            if json_val.is_object() {
                out.structured_content = Some(json_val);
            }
            Ok(out)
        }
        Err(err) => Ok(CallToolResult::error(vec![ContentBlock::text(err)])),
    }
}

#[tool_router]
impl AppMcp {
    fn new(client: ApiClient, spool_scope: Arc<SpoolScope>) -> Self {
        Self {
            client,
            spool_scope,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Verify connectivity to the Model Arena homelab portal and return the signed-in household user."
    )]
    async fn whoami(&self) -> Result<CallToolResult, McpError> {
        json_tool_result(self.client.me().await)
    }

    #[tool(
        description = "List household members on this Model Arena server so AI harnesses can reference shared household users."
    )]
    async fn list_users(&self) -> Result<CallToolResult, McpError> {
        json_tool_result(self.client.list_users().await)
    }

    #[tool(
        description = "Select subagent models and prompt lenses for the next adversarial-review or plan-tournament invocation."
    )]
    async fn select_panel(
        &self,
        Parameters(input): Parameters<SelectPanelInput>,
    ) -> Result<CallToolResult, McpError> {
        let _ = tokio::time::timeout(
            Duration::from_millis(250),
            flush_spool(&self.client, &self.spool_scope),
        )
        .await;
        json_tool_result(
            self.client
                .select_panel(&input, Some(Duration::from_millis(800)))
                .await
                .map_err(|e| e.to_string()),
        )
    }

    #[tool(
        description = "Record a completed adversarial-review or plan-tournament run idempotently, spooling locally if the server is temporarily unreachable."
    )]
    async fn record_run(
        &self,
        Parameters(input): Parameters<RecordRunInput>,
    ) -> Result<CallToolResult, McpError> {
        json_tool_result(submit_or_spool_run(&self.client, &self.spool_scope, input).await)
    }

    #[tool(description = "Flush any offline-spooled skill runs to the Model Arena server.")]
    async fn flush_spool(&self) -> Result<CallToolResult, McpError> {
        json_tool_result(Ok(flush_spool(&self.client, &self.spool_scope).await))
    }

    #[tool(
        description = "Get empirical model scorecards, de-biased plan-tournament lens cells, host-judge affinity cells, and Challenger bout comparisons."
    )]
    async fn get_leaderboard(
        &self,
        Parameters(filter): Parameters<LeaderboardQuery>,
    ) -> Result<CallToolResult, McpError> {
        let _ = tokio::time::timeout(
            Duration::from_millis(1500),
            flush_spool(&self.client, &self.spool_scope),
        )
        .await;
        json_tool_result(
            self.client
                .get_leaderboard(&filter)
                .await
                .map_err(|e| e.to_string()),
        )
    }

    #[tool(description = "List configured champion and backup models across lineages.")]
    async fn list_roster(
        &self,
        Parameters(args): Parameters<ListRosterArgs>,
    ) -> Result<CallToolResult, McpError> {
        let skill = args.skill.as_deref();
        let res = async {
            let roster = self
                .client
                .list_roster(skill)
                .await
                .map_err(|e| e.to_string())?;
            let trials = self
                .client
                .list_trials(skill, None)
                .await
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({
                "slots": roster.roster,
                "trials": trials.trials,
            }))
        }
        .await;
        json_tool_result(res)
    }

    #[tool(
        description = "Atomically replace the model roster for adversarial-review or plan-tournament."
    )]
    async fn set_roster(
        &self,
        Parameters(args): Parameters<SetRosterArgs>,
    ) -> Result<CallToolResult, McpError> {
        json_tool_result(
            self.client
                .set_roster(&args.skill, args.slots)
                .await
                .map_err(|e| e.to_string()),
        )
    }

    #[tool(description = "Start a bounded Champion vs Challenger bout in one lineage slot.")]
    async fn start_trial(
        &self,
        Parameters(input): Parameters<StartTrialInput>,
    ) -> Result<CallToolResult, McpError> {
        json_tool_result(
            self.client
                .start_trial(&input)
                .await
                .map_err(|e| e.to_string()),
        )
    }

    #[tool(description = "Promote or retire an open Challenger trial.")]
    async fn decide_trial(
        &self,
        Parameters(args): Parameters<DecideTrialArgs>,
    ) -> Result<CallToolResult, McpError> {
        json_tool_result(
            self.client
                .decide_trial(args.trial_id, &args.decision)
                .await
                .map_err(|e| e.to_string()),
        )
    }

    #[tool(
        description = "Confirm or overturn a recorded candidate finding when human ground truth differs from host triage."
    )]
    async fn override_candidate(
        &self,
        Parameters(args): Parameters<OverrideCandidateArgs>,
    ) -> Result<CallToolResult, McpError> {
        json_tool_result(
            self.client
                .override_candidate(
                    args.candidate_id,
                    &OverrideCandidateInput {
                        human_override: args.human_override,
                        note: args.note,
                    },
                )
                .await
                .map_err(|e| e.to_string()),
        )
    }

    #[tool(
        description = "Record a single-model coding or PR implementation run idempotently, spooling locally if the server is temporarily unreachable."
    )]
    async fn record_coding_run(
        &self,
        Parameters(input): Parameters<CodingRunInput>,
    ) -> Result<CallToolResult, McpError> {
        json_tool_result(submit_or_spool_coding_run(&self.client, &self.spool_scope, input).await)
    }

    #[tool(
        description = "Confirm or overturn a recorded single-model coding run outcome when human ground truth differs."
    )]
    async fn override_coding_run(
        &self,
        Parameters(args): Parameters<OverrideCodingRunArgs>,
    ) -> Result<CallToolResult, McpError> {
        json_tool_result(
            self.client
                .override_coding_run(
                    args.coding_run_id,
                    &OverrideCodingRunInput {
                        human_override: args.human_override,
                        note: args.note,
                    },
                )
                .await
                .map_err(|e| e.to_string()),
        )
    }

    #[tool(
        description = "Get empirical single-model coding scorecards (first-pass clean rate, review findings per PR, CI iterations, and escaped regression rates)."
    )]
    async fn get_coding_leaderboard(
        &self,
        Parameters(filter): Parameters<CodingLeaderboardQuery>,
    ) -> Result<CallToolResult, McpError> {
        let _ = tokio::time::timeout(
            Duration::from_millis(1500),
            flush_spool(&self.client, &self.spool_scope),
        )
        .await;
        json_tool_result(
            self.client
                .get_coding_leaderboard(&filter)
                .await
                .map_err(|e| e.to_string()),
        )
    }

    #[tool(
        description = "Scan local ~/.copilot/session-state events.jsonl logs and backfill historical skill runs idempotently."
    )]
    async fn backfill_sessions(
        &self,
        Parameters(args): Parameters<BackfillArgs>,
    ) -> Result<CallToolResult, McpError> {
        json_tool_result(run_backfill(&self.client, &self.spool_scope, args).await)
    }

    async fn call_by_name(&self, name: &str, raw_input: &str) -> Result<CallToolResult, String> {
        let trimmed = raw_input.trim();
        let json_str = if trimmed.is_empty() { "{}" } else { trimmed };
        let parse_err = |e: serde_json::Error| format!("invalid JSON input for '{name}': {e}");

        let res = match name {
            "whoami" => self.whoami().await,
            "list_users" => self.list_users().await,
            "select_panel" => {
                let input: SelectPanelInput = serde_json::from_str(json_str).map_err(parse_err)?;
                self.select_panel(Parameters(input)).await
            }
            "record_run" => {
                let input: RecordRunInput = serde_json::from_str(json_str).map_err(parse_err)?;
                self.record_run(Parameters(input)).await
            }
            "flush_spool" => self.flush_spool().await,
            "get_leaderboard" => {
                let filter: LeaderboardQuery = serde_json::from_str(json_str).map_err(parse_err)?;
                self.get_leaderboard(Parameters(filter)).await
            }
            "list_roster" => {
                let args: ListRosterArgs = serde_json::from_str(json_str).map_err(parse_err)?;
                self.list_roster(Parameters(args)).await
            }
            "set_roster" => {
                let args: SetRosterArgs = serde_json::from_str(json_str).map_err(parse_err)?;
                self.set_roster(Parameters(args)).await
            }
            "start_trial" => {
                let input: StartTrialInput = serde_json::from_str(json_str).map_err(parse_err)?;
                self.start_trial(Parameters(input)).await
            }
            "decide_trial" => {
                let args: DecideTrialArgs = serde_json::from_str(json_str).map_err(parse_err)?;
                self.decide_trial(Parameters(args)).await
            }
            "override_candidate" => {
                let args: OverrideCandidateArgs =
                    serde_json::from_str(json_str).map_err(parse_err)?;
                self.override_candidate(Parameters(args)).await
            }
            "record_coding_run" => {
                let input: CodingRunInput = serde_json::from_str(json_str).map_err(parse_err)?;
                self.record_coding_run(Parameters(input)).await
            }
            "override_coding_run" => {
                let args: OverrideCodingRunArgs =
                    serde_json::from_str(json_str).map_err(parse_err)?;
                self.override_coding_run(Parameters(args)).await
            }
            "get_coding_leaderboard" => {
                let filter: CodingLeaderboardQuery =
                    serde_json::from_str(json_str).map_err(parse_err)?;
                self.get_coding_leaderboard(Parameters(filter)).await
            }
            "backfill_sessions" => {
                let args: BackfillArgs = serde_json::from_str(json_str).map_err(parse_err)?;
                self.backfill_sessions(Parameters(args)).await
            }
            other => return Err(format!("unknown tool '{other}'")),
        };

        res.map_err(|e| e.to_string())
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AppMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                format!("{APP_SLUG}-mcp"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Model Arena homelab MCP server. Connects to the household Model Arena API using personal access tokens.",
            )
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("serve") | Some("--config") => {
            let rest = if args.first().map(String::as_str) == Some("serve") {
                &args[1..]
            } else {
                &args[..]
            };
            if let Err(err) = run_serve(rest).await {
                eprintln!("model-arena-rust-mcp error: {err}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Some("list") => {
            if let Err(err) = run_list(&args[1..]) {
                eprintln!("model-arena-rust-mcp list error: {err}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Some("call") => {
            if let Err(err) = run_call(&args[1..]).await {
                eprintln!("model-arena-rust-mcp call error: {err}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Some("token") => {
            if let Err(err) = run_token(&args[1..]).await {
                eprintln!("model-arena-rust-mcp token error: {err}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Some("-h" | "--help" | "help") => {
            print_usage();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown subcommand '{other}'\n");
            print_usage();
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    eprintln!(
        "Usage:
  model-arena-rust-mcp [serve] [--config <path>]
  model-arena-rust-mcp list [--config <path>]
  model-arena-rust-mcp call <tool> [--input <json> | --input-file <path>] [--config <path>]
  model-arena-rust-mcp token [--url <app-url>] [--email <email>] [--name <token-name>] [--config <path>]"
    );
}

fn build_app_mcp(config_path: Option<&std::path::Path>) -> Result<AppMcp, String> {
    let cfg = ClientConfig::load(config_path)?;
    let client = ApiClient::new(&cfg)?;
    let spool_scope = Arc::new(SpoolScope::from_config(&cfg));
    Ok(AppMcp::new(client, spool_scope))
}

async fn run_serve(args: &[String]) -> Result<(), String> {
    let mut config_path: Option<PathBuf> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--config" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--config requires a path".to_string())?;
                config_path = Some(PathBuf::from(val));
            }
            other => return Err(format!("unexpected argument '{other}'")),
        }
    }

    let app_mcp = build_app_mcp(config_path.as_deref())?;
    let service = app_mcp
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|err| format!("failed to start MCP stdio server: {err}"))?;

    service
        .waiting()
        .await
        .map_err(|err| format!("MCP stdio session terminated with error: {err}"))?;
    Ok(())
}

fn run_list(args: &[String]) -> Result<(), String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--config" => {
                let _ = iter
                    .next()
                    .ok_or_else(|| "--config requires a path".to_string())?;
            }
            other => return Err(format!("unexpected argument '{other}'")),
        }
    }

    let tools = AppMcp::tool_router().list_all();
    let payload = serde_json::json!({ "tools": tools });
    let text = serde_json::to_string_pretty(&payload)
        .map_err(|err| format!("failed to serialize tool list: {err}"))?;
    println!("{text}");
    Ok(())
}

async fn run_call(args: &[String]) -> Result<(), String> {
    let mut tool_name: Option<String> = None;
    let mut input_json: Option<String> = None;
    let mut input_file: Option<PathBuf> = None;
    let mut config_path: Option<PathBuf> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--input" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--input requires a JSON string".to_string())?;
                input_json = Some(val.clone());
            }
            "--input-file" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--input-file requires a path".to_string())?;
                input_file = Some(PathBuf::from(val));
            }
            "--config" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--config requires a path".to_string())?;
                config_path = Some(PathBuf::from(val));
            }
            other if !other.starts_with('-') && tool_name.is_none() => {
                tool_name = Some(other.to_string());
            }
            other => return Err(format!("unexpected argument '{other}'")),
        }
    }

    let name = tool_name.ok_or_else(|| "call requires a <tool> name".to_string())?;
    if input_json.is_some() && input_file.is_some() {
        return Err("cannot specify both --input and --input-file".to_string());
    }

    let raw_input = if let Some(path) = input_file {
        if path.as_os_str() == "-" {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .map_err(|err| format!("failed to read stdin: {err}"))?;
            buf
        } else {
            fs::read_to_string(&path)
                .map_err(|err| format!("failed to read {}: {err}", path.display()))?
        }
    } else {
        input_json.unwrap_or_else(|| "{}".to_string())
    };

    let app_mcp = build_app_mcp(config_path.as_deref())?;
    let result = app_mcp.call_by_name(&name, &raw_input).await?;

    let mut printed = false;
    for block in &result.content {
        if let Some(text) = block.as_text() {
            println!("{}", text.text);
            printed = true;
        }
    }
    if !printed {
        let serialized = serde_json::to_string_pretty(&result)
            .map_err(|err| format!("failed to serialize tool result: {err}"))?;
        println!("{serialized}");
    }
    if result.is_error == Some(true) {
        return Err(format!("tool '{name}' returned an error"));
    }
    Ok(())
}

async fn run_token(args: &[String]) -> Result<(), String> {
    let mut url: Option<String> = env::var("MCP_APP_URL")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let mut email: Option<String> = env::var("MCP_EMAIL").ok().filter(|v| !v.trim().is_empty());
    let mut token_name = format!("{APP_SLUG}-mcp");
    let mut config_path: Option<PathBuf> = None;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--url" => {
                url = Some(
                    iter.next()
                        .ok_or_else(|| "--url requires a value".to_string())?
                        .clone(),
                );
            }
            "--email" => {
                email = Some(
                    iter.next()
                        .ok_or_else(|| "--email requires a value".to_string())?
                        .clone(),
                );
            }
            "--name" => {
                token_name = iter
                    .next()
                    .ok_or_else(|| "--name requires a value".to_string())?
                    .clone();
            }
            "--config" => {
                let val = iter
                    .next()
                    .ok_or_else(|| "--config requires a path".to_string())?;
                config_path = Some(PathBuf::from(val));
            }
            other => return Err(format!("unexpected argument '{other}'")),
        }
    }

    let app_url = match url {
        Some(v) => v,
        None => prompt_line("App URL [http://localhost:8080]: ").map(|v| {
            if v.trim().is_empty() {
                "http://localhost:8080".to_string()
            } else {
                v.trim().to_string()
            }
        })?,
    };

    let user_email = match email {
        Some(v) => v,
        None => {
            let entered = prompt_line("Email: ")?;
            if entered.trim().is_empty() {
                return Err("email is required".to_string());
            }
            entered.trim().to_string()
        }
    };

    let password = match env::var("MCP_PASSWORD") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            if !io::stdin().is_terminal() {
                return Err("MCP_PASSWORD must be set when running non-interactively".to_string());
            }
            rpassword::prompt_password("Password: ")
                .map_err(|err| format!("failed to read password: {err}"))?
        }
    };

    let dest = match config_path {
        Some(p) => p,
        None => ClientConfig::default_path()?,
    };

    let created = mint_token_via_login(&app_url, &user_email, &password, &token_name).await?;
    let cfg = ClientConfig {
        app_url: app_url.trim().trim_end_matches('/').to_string(),
        token: created.token,
    };
    cfg.write_atomic(&dest)?;

    println!(
        "minted token '{}' (...{}) and wrote {}",
        created.name,
        created.last4,
        dest.display()
    );
    Ok(())
}

fn prompt_line(prompt: &str) -> Result<String, String> {
    if !io::stdin().is_terminal() {
        return Err(format!(
            "missing required value for '{prompt}' in non-interactive mode"
        ));
    }
    eprint!("{prompt}");
    io::stderr()
        .flush()
        .map_err(|err| format!("failed to flush prompt: {err}"))?;
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|err| format!("failed to read input: {err}"))?;
    Ok(line)
}
