#![allow(clippy::collapsible_if)] // Production remains on Rust 1.86, before stable let chains.

use csd_pool_accounting::{
    LedgerEntry, MinerBalance, PayoutBatchDraft, PayoutRecipient, ShareWeight, allocate_pplns,
    payout_batch_draft, reward_ledger_entries, select_payouts,
};
use csd_pool_consensus::{
    SubmitSolution, coinbase_bytes, coinbase_txid, compose_extranonce, hash_leq_target, header_84,
    header_hash, merkle_root_from_branch, parse_le_u32_hex_bytes, verify_share_with_difficulty,
};
use csd_pool_db::{
    AlertEvent, AsyncPoolRepository, BlockRepository, BlockStatusUpdate, ControlRepository,
    InMemoryRepository, MonitoringRepository, NodeSampleRecord, PayoutAuditEvent,
    PayoutBatchRecord, PayoutRepository, PgRepository, PoolRepository, RewardBlock,
    RewardRepository, ShareQualityAlertRecord,
};
use csd_pool_node::{
    BlockCandidateSubmitRequest, BlockStatusResponse, CsdNodeClient, NodeMiningTemplate,
    SubmitTxResponse,
};
use csd_pool_protocol::{
    NotifyParams, Response, SubmitParams, SubscribeResult, authorize_request, serialize_line,
    submit_request, subscribe_request,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::timeout;

#[derive(Debug, Error)]
enum WorkerError {
    #[error("unknown command: {0}")]
    UnknownCommand(String),
    #[error("accounting error: {0}")]
    Accounting(#[from] csd_pool_accounting::AccountingError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("repository error: {0}")]
    Repository(#[from] csd_pool_db::RepositoryError),
    #[error("sql error: {0}")]
    Sql(#[from] sqlx::Error),
    #[error("config error: {0}")]
    Config(#[from] csd_pool_config::ConfigError),
    #[error("database URL missing; set CSD_POOL_DATABASE_URL or configure [database].url_env")]
    MissingDatabaseUrl,
    #[error("backup path missing; pass a path argument or set CSD_POOL_BACKUP_PATH")]
    MissingBackupPath,
    #[error("restore requires CSD_POOL_RESTORE_CONFIRM=restore")]
    RestoreConfirmationMissing,
    #[error("external command failed: {command} exited with {status}; stderr: {stderr}")]
    ExternalCommandFailed {
        command: String,
        status: String,
        stderr: String,
    },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "CSD node URL missing; set CSD_POOL_WATCH_NODE_URL, CSD_POOL_NODE_URL, or configure a watch node"
    )]
    MissingNodeUrl,
    #[error(
        "template CSD node URL missing; set CSD_POOL_TEMPLATE_NODE_URL, CSD_POOL_NODE_URL, or configure a template node"
    )]
    MissingTemplateNodeUrl,
    #[error(
        "pool mining address missing; set CSD_POOL_MINING_ADDRESS or configure [pool].mining_address"
    )]
    MissingMiningAddress,
    #[error("signer URL missing; set CSD_POOL_SIGNER_URL or configure [signer].url_env")]
    MissingSignerUrl,
    #[error("node error: {0}")]
    Node(#[from] csd_pool_node::NodeError),
    #[error("protocol error: {0}")]
    Protocol(#[from] csd_pool_protocol::ProtocolError),
    #[error("consensus error: {0}")]
    Consensus(#[from] csd_pool_consensus::ConsensusError),
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid amount: {0}")]
    InvalidAmount(String),
    #[error("candidate canary requires CSD_POOL_NODE_CANDIDATE_CANARY_CONFIRM=mine-and-submit")]
    CandidateCanaryConfirmationMissing,
    #[error("candidate canary exhausted {attempts} hashes without finding a network solution")]
    CandidateCanaryExhausted { attempts: u64 },
    #[error("candidate canary node rejected solved block: {0}")]
    CandidateCanaryRejected(String),
    #[error("stratum smoke failed: {failed_clients} of {requested_clients} clients failed")]
    StratumSmokeFailed {
        requested_clients: usize,
        failed_clients: usize,
    },
    #[error(
        "stratum load test failed: {succeeded_clients}/{requested_clients} clients succeeded, {failed_clients} failed, minimum success is {min_success_clients}"
    )]
    StratumLoadTestFailed {
        requested_clients: usize,
        min_success_clients: usize,
        succeeded_clients: usize,
        failed_clients: usize,
    },
    #[error("node template check failed for {node_url}: {reason}")]
    NodeTemplateCheckFailed { node_url: String, reason: String },
    #[error("signer contract check failed for {signer_url}: {reason}")]
    SignerCheckFailed { signer_url: String, reason: String },
    #[error("config check failed for {config_path}: {errors} error(s)")]
    ConfigCheckFailed { config_path: String, errors: usize },
}

type Result<T> = std::result::Result<T, WorkerError>;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).cloned().unwrap_or_else(|| "help".to_owned());
    match command.as_str() {
        "migrate" => print_json(&migrate().await?)?,
        "backup-db" => print_json(&backup_db(args.get(2).map(String::as_str)).await?)?,
        "restore-db" => print_json(&restore_db(args.get(2).map(String::as_str)).await?)?,
        "accounting-export" => accounting_export(args.get(2).map(String::as_str)).await?,
        "stratum-smoke" => run_stratum_smoke_command(args.get(2).map(String::as_str)).await?,
        "stratum-submit-probe" => {
            run_stratum_submit_probe_command(args.get(2).map(String::as_str)).await?
        }
        "stratum-accepted-share-probe" => {
            run_stratum_accepted_share_probe_command(args.get(2).map(String::as_str)).await?
        }
        "stratum-load-test" => {
            run_stratum_load_test_command(args.get(2).map(String::as_str)).await?
        }
        "check-config" => run_check_config_command(args.get(2).map(String::as_str)).await?,
        "check-node-template" => run_check_node_template_command().await?,
        "check-node-runtime" => run_check_node_runtime_command().await?,
        "mine-node-candidate-canary" => print_json(&mine_node_candidate_canary().await?)?,
        "check-signer" => run_check_signer_command().await?,
        "check-database-runtime" => run_check_database_runtime_command().await?,
        "reconcile-blocks" => print_json(&reconcile_blocks().await?)?,
        "settle-rewards" => print_json(&settle_rewards().await?)?,
        "mature-rewards" => print_json(&mature_rewards().await?)?,
        "reverse-orphans" => print_json(&reverse_orphans().await?)?,
        "payout-preview" => print_json(&payout_preview().await?)?,
        "create-payouts" => print_json(&create_payouts().await?)?,
        "sign-payouts" => print_json(&sign_payouts().await?)?,
        "submit-payouts" => print_json(&submit_payouts().await?)?,
        "reconcile-payouts" => print_json(&reconcile_payouts().await?)?,
        "sample-health" => print_json(&sample_health().await?)?,
        "check-alerts" => print_json(&check_alerts().await?)?,
        "reward-dry-run" => print_json(&reward_dry_run().await?)?,
        "payout-dry-run" => print_json(&payout_dry_run().await?)?,
        "help" | "--help" | "-h" => print_help(),
        other => return Err(WorkerError::UnknownCommand(other.to_owned())),
    }
    Ok(())
}

fn print_help() {
    println!("csd-pool-workers commands:");
    println!("  migrate          apply PostgreSQL migrations");
    println!("  backup-db [path] write a PostgreSQL custom-format backup with pg_dump");
    println!(
        "  restore-db <path> restore a backup with pg_restore; requires CSD_POOL_RESTORE_CONFIRM=restore"
    );
    println!("  accounting-export [path] export immutable ledger entries as CSV");
    println!("  stratum-smoke [addr] connect simulated miners to a Stratum endpoint");
    println!("  stratum-submit-probe [addr] exercise mining.submit and expect a Stratum response");
    println!(
        "  stratum-accepted-share-probe [addr] submit a known valid share to a static/easy endpoint"
    );
    println!("  stratum-load-test [addr] connect 100+ simulated miners and report pass/fail");
    println!("  check-config [path] validate pool config, payout limits, roles, and env wiring");
    println!("  check-node-template validate live CSD mining template adapter contract");
    println!("  check-node-runtime validate CSD node role quorum, health, and RPC latency");
    println!(
        "  mine-node-candidate-canary solve and submit one real node candidate; requires explicit confirmation"
    );
    println!("  check-signer    validate isolated payout signer health and sign contract");
    println!("  check-database-runtime validate PostgreSQL runtime, schema, and query latency");
    println!("  reconcile-blocks reconcile submitted block statuses");
    println!("  settle-rewards   allocate confirmed block rewards");
    println!("  mature-rewards   move mature rewards to confirmed balance");
    println!("  reverse-orphans  reverse rewards for orphaned blocks");
    println!("  payout-preview   preview next payable payout batch without locking balances");
    println!("  create-payouts   create and lock payable payout batch");
    println!("  sign-payouts     request signer for created payout batches");
    println!("  submit-payouts   broadcast signed payout transactions");
    println!("  reconcile-payouts reconcile submitted payout transactions");
    println!("  sample-health    sample CSD node and signer health");
    println!("  check-alerts     create alerts for unhealthy services and stuck payouts");
    println!("  reward-dry-run   run sample PPLNS allocation");
    println!("  payout-dry-run   run sample payout selection");
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

async fn run_check_config_command(path_arg: Option<&str>) -> Result<()> {
    let run = check_config(path_arg)?;
    print_json(&run)?;
    if !run.passed {
        return Err(WorkerError::ConfigCheckFailed {
            config_path: run.config_path,
            errors: run.errors.len(),
        });
    }
    Ok(())
}

async fn run_stratum_smoke_command(endpoint_arg: Option<&str>) -> Result<()> {
    let run = stratum_smoke(endpoint_arg).await?;
    print_json(&run)?;
    if run.failed_clients > 0 {
        return Err(WorkerError::StratumSmokeFailed {
            requested_clients: run.requested_clients,
            failed_clients: run.failed_clients,
        });
    }
    Ok(())
}

async fn run_stratum_submit_probe_command(endpoint_arg: Option<&str>) -> Result<()> {
    let run = stratum_submit_probe(endpoint_arg).await?;
    let passed = run.passed;
    print_json(&run)?;
    if !passed {
        return Err(WorkerError::StratumSmokeFailed {
            requested_clients: 1,
            failed_clients: 1,
        });
    }
    Ok(())
}

async fn run_stratum_accepted_share_probe_command(endpoint_arg: Option<&str>) -> Result<()> {
    let run = stratum_accepted_share_probe(endpoint_arg).await?;
    let passed = run.passed;
    print_json(&run)?;
    if !passed {
        return Err(WorkerError::StratumSmokeFailed {
            requested_clients: 1,
            failed_clients: 1,
        });
    }
    Ok(())
}

async fn run_stratum_load_test_command(endpoint_arg: Option<&str>) -> Result<()> {
    let run = stratum_load_test(endpoint_arg).await?;
    print_json(&run)?;
    if !run.passed {
        return Err(WorkerError::StratumLoadTestFailed {
            requested_clients: run.requested_clients,
            min_success_clients: run.min_success_clients,
            succeeded_clients: run.succeeded_clients,
            failed_clients: run.failed_clients,
        });
    }
    Ok(())
}

async fn run_check_node_template_command() -> Result<()> {
    let run = check_node_template().await?;
    print_json(&run)?;
    if !run.passed {
        return Err(WorkerError::NodeTemplateCheckFailed {
            node_url: run.template_node_url,
            reason: run
                .template_error
                .clone()
                .unwrap_or_else(|| "template contract did not pass".to_owned()),
        });
    }
    Ok(())
}

async fn run_check_node_runtime_command() -> Result<()> {
    let run = check_node_runtime().await?;
    print_json(&run)?;
    if !run.passed {
        return Err(WorkerError::ConfigCheckFailed {
            config_path: "node-runtime".to_owned(),
            errors: run.failed_checks.len(),
        });
    }
    Ok(())
}

async fn run_check_signer_command() -> Result<()> {
    let run = check_signer().await?;
    print_json(&run)?;
    if !run.passed {
        return Err(WorkerError::SignerCheckFailed {
            signer_url: run.signer_url,
            reason: run
                .sign_error
                .clone()
                .or(run.health_error.clone())
                .unwrap_or_else(|| "signer contract did not pass".to_owned()),
        });
    }
    Ok(())
}

async fn run_check_database_runtime_command() -> Result<()> {
    let run = check_database_runtime().await?;
    print_json(&run)?;
    if !run.passed {
        return Err(WorkerError::ConfigCheckFailed {
            config_path: "database-runtime".to_owned(),
            errors: run.failed_checks,
        });
    }
    Ok(())
}

async fn migrate() -> Result<MigrationRun> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let repo = PgRepository::connect(&database_url).await?;
    let applied_versions = csd_pool_db::run_migrations(repo.pool()).await?;
    let known_versions: Vec<i64> = csd_pool_db::all_migrations()
        .iter()
        .map(|migration| migration.version)
        .collect();
    let database_versions = csd_pool_db::applied_migration_versions(repo.pool()).await?;
    let latest_known_version = known_versions.iter().copied().max().unwrap_or_default();
    let latest_database_version = database_versions.iter().copied().max().unwrap_or_default();
    let complete = known_versions
        .iter()
        .all(|version| database_versions.contains(version));
    Ok(MigrationRun {
        applied_versions,
        known_versions,
        database_versions,
        latest_known_version,
        latest_database_version,
        known_migration_count: csd_pool_db::all_migrations().len(),
        complete,
    })
}

async fn check_database_runtime() -> Result<DatabaseRuntimeCheckRun> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let max_query_ms = env_u64("CSD_POOL_DATABASE_RUNTIME_MAX_QUERY_MS").unwrap_or(1_000) as f64;
    let max_transaction_ms =
        env_u64("CSD_POOL_DATABASE_RUNTIME_MAX_TRANSACTION_MS").unwrap_or(1_000) as f64;

    let connect_started = Instant::now();
    let repo = PgRepository::connect(&database_url).await?;
    let connect_ms = elapsed_instant_ms(connect_started);
    let pool = repo.pool();

    let ping_started = Instant::now();
    let ping: i64 = sqlx::query_scalar("select 1::bigint")
        .fetch_one(pool)
        .await?;
    let ping_ms = elapsed_instant_ms(ping_started);

    let identity_started = Instant::now();
    let (database_name, database_user, server_version): (String, String, String) =
        sqlx::query_as("select current_database(), current_user, version()")
            .fetch_one(pool)
            .await?;
    let identity_ms = elapsed_instant_ms(identity_started);

    let migrations_started = Instant::now();
    let known_versions: Vec<i64> = csd_pool_db::all_migrations()
        .iter()
        .map(|migration| migration.version)
        .collect();
    let database_versions = csd_pool_db::applied_migration_versions(pool).await?;
    let migrations_ms = elapsed_instant_ms(migrations_started);
    let latest_known_version = known_versions.iter().copied().max().unwrap_or_default();
    let latest_database_version = database_versions.iter().copied().max().unwrap_or_default();
    let migrations_complete = known_versions
        .iter()
        .all(|version| database_versions.contains(version));

    let mut table_counts = Vec::new();
    let table_count_started = Instant::now();
    for table in [
        "schema_migrations",
        "pool_settings",
        "miners",
        "workers",
        "sessions",
        "jobs",
        "shares",
        "share_events",
        "blocks",
        "ledger_entries",
        "balance_cache",
        "payout_batches",
        "payout_recipients",
        "payout_audit_events",
        "node_samples",
        "alert_events",
    ] {
        let query = format!("select count(*) from {table}");
        let row_count: i64 = sqlx::query_scalar(&query).fetch_one(pool).await?;
        table_counts.push(DatabaseTableRuntimeCheck {
            table: table.to_owned(),
            row_count,
        });
    }
    let table_counts_ms = elapsed_instant_ms(table_count_started);

    let transaction_started = Instant::now();
    let mut tx = pool.begin().await?;
    sqlx::query(
        "create temporary table csd_pool_runtime_probe (
            id bigint primary key,
            note text not null
        ) on commit drop",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query("insert into csd_pool_runtime_probe(id, note) values ($1, $2)")
        .bind(1_i64)
        .bind("runtime-check")
        .execute(&mut *tx)
        .await?;
    let transaction_probe_count: i64 =
        sqlx::query_scalar("select count(*) from csd_pool_runtime_probe")
            .fetch_one(&mut *tx)
            .await?;
    tx.rollback().await?;
    let rollback_probe_present: Option<String> =
        sqlx::query_scalar("select to_regclass('pg_temp.csd_pool_runtime_probe')::text")
            .fetch_one(pool)
            .await?;
    let transaction_ms = elapsed_instant_ms(transaction_started);

    let max_observed_query_ms = connect_ms
        .max(ping_ms)
        .max(identity_ms)
        .max(migrations_ms)
        .max(table_counts_ms);
    let failed_checks = [
        ("database_url_present", true),
        ("ping_ok", ping == 1),
        ("migrations_complete", migrations_complete),
        (
            "latest_database_matches_known",
            latest_database_version == latest_known_version,
        ),
        ("expected_table_count", table_counts.len() == 16),
        ("transaction_write_ok", transaction_probe_count == 1),
        ("transaction_rollback_ok", rollback_probe_present.is_none()),
        ("query_latency_ok", max_observed_query_ms <= max_query_ms),
        (
            "transaction_latency_ok",
            transaction_ms <= max_transaction_ms,
        ),
    ]
    .into_iter()
    .filter(|(_, passed)| !passed)
    .count();

    Ok(DatabaseRuntimeCheckRun {
        passed: failed_checks == 0,
        failed_checks,
        database_url_present: true,
        database_name,
        database_user,
        server_version,
        connect_ms,
        ping_ms,
        identity_ms,
        migrations_ms,
        table_counts_ms,
        transaction_ms,
        max_query_ms,
        max_transaction_ms,
        max_observed_query_ms,
        ping_ok: ping == 1,
        known_versions,
        database_versions,
        latest_known_version,
        latest_database_version,
        migrations_complete,
        latest_database_matches_known: latest_database_version == latest_known_version,
        table_counts,
        transaction_write_ok: transaction_probe_count == 1,
        transaction_rollback_ok: rollback_probe_present.is_none(),
        query_latency_ok: max_observed_query_ms <= max_query_ms,
        transaction_latency_ok: transaction_ms <= max_transaction_ms,
    })
}

async fn backup_db(path_arg: Option<&str>) -> Result<BackupRun> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let path = backup_path(path_arg)?;
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let plan = backup_command_plan(&path);
    let output = Command::new("pg_dump")
        .arg("-Fc")
        .arg("--no-owner")
        .arg("--no-privileges")
        .arg("-f")
        .arg(&path)
        .arg(&database_url)
        .output()?;
    ensure_command_success("pg_dump", &output)?;
    Ok(BackupRun {
        path: path.display().to_string(),
        command: plan.display,
        size_bytes: fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0),
    })
}

async fn restore_db(path_arg: Option<&str>) -> Result<RestoreRun> {
    if std::env::var("CSD_POOL_RESTORE_CONFIRM").ok().as_deref() != Some("restore") {
        return Err(WorkerError::RestoreConfirmationMissing);
    }
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let path = restore_path(path_arg)?;
    let plan = restore_command_plan(&path);
    let output = Command::new("pg_restore")
        .arg("--clean")
        .arg("--if-exists")
        .arg("--no-owner")
        .arg("--no-privileges")
        .arg("--dbname")
        .arg(&database_url)
        .arg(&path)
        .output()?;
    ensure_command_success("pg_restore", &output)?;
    Ok(RestoreRun {
        path: path.display().to_string(),
        command: plan.display,
    })
}

fn check_config(path_arg: Option<&str>) -> Result<ConfigCheckRun> {
    let config_path = config_check_path(path_arg);
    let config = csd_pool_config::PoolConfig::from_file(&config_path)?;
    let require_env = env_bool("CSD_POOL_CHECK_CONFIG_REQUIRE_ENV");
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let stratum_listen = match config.stratum_listen_addr() {
        Ok(addr) => addr.to_string(),
        Err(error) => {
            errors.push(error.to_string());
            config.stratum.listen.clone()
        }
    };
    let api_listen = match config.api_listen_addr() {
        Ok(addr) => addr.to_string(),
        Err(error) => {
            errors.push(error.to_string());
            config.api.listen.clone()
        }
    };
    let signer_listen = match config.signer.listen.parse::<std::net::SocketAddr>() {
        Ok(addr) => addr.to_string(),
        Err(_) => {
            errors.push(format!(
                "invalid socket address in signer.listen: {}",
                config.signer.listen
            ));
            config.signer.listen.clone()
        }
    };

    if config.pool.id.trim().is_empty() {
        errors.push("pool.id must not be empty".to_owned());
    }
    if !is_addr20_hex(&config.pool.mining_address) {
        errors.push("pool.mining_address must be a 40-character lowercase hex address".to_owned());
    }
    if !(0.0..=100.0).contains(&config.pool.fee_percent) {
        errors.push("pool.fee_percent must be between 0 and 100".to_owned());
    }
    if config.pool.fee_percent > 5.0 {
        warnings.push("pool.fee_percent is above 5%; confirm this is intentional".to_owned());
    }
    if config.pool.confirm_depth == 0 {
        errors.push("pool.confirm_depth must be greater than 0".to_owned());
    }
    if config.pool.payout_interval_secs < 60 {
        errors.push("pool.payout_interval_secs must be at least 60".to_owned());
    }

    let minimum_payout_base_units = checked_amount(
        "pool.minimum_payout_csd",
        &config.pool.minimum_payout_csd,
        &mut errors,
    );
    let manual_payout_approval_base_units = checked_amount(
        "pool.manual_payout_approval_csd",
        &config.pool.manual_payout_approval_csd,
        &mut errors,
    );
    let max_payout_batch_base_units = checked_amount(
        "pool.max_payout_batch_csd",
        &config.pool.max_payout_batch_csd,
        &mut errors,
    );
    let max_daily_payout_base_units = checked_amount(
        "pool.max_daily_payout_csd",
        &config.pool.max_daily_payout_csd,
        &mut errors,
    );
    if let (Some(minimum), Some(manual), Some(batch), Some(daily)) = (
        minimum_payout_base_units,
        manual_payout_approval_base_units,
        max_payout_batch_base_units,
        max_daily_payout_base_units,
    ) {
        if minimum == 0 {
            errors.push("pool.minimum_payout_csd must be greater than 0".to_owned());
        }
        if minimum > manual {
            errors.push("pool.manual_payout_approval_csd must be >= minimum_payout_csd".to_owned());
        }
        if manual > batch {
            errors
                .push("pool.max_payout_batch_csd must be >= manual_payout_approval_csd".to_owned());
        }
        if batch > daily {
            errors.push("pool.max_daily_payout_csd must be >= max_payout_batch_csd".to_owned());
        }
    }

    if config.stratum.initial_difficulty <= 0.0
        || config.stratum.min_difficulty <= 0.0
        || config.stratum.max_difficulty <= 0.0
    {
        errors.push("stratum difficulties must be greater than 0".to_owned());
    }
    if config.stratum.min_difficulty > config.stratum.initial_difficulty {
        errors.push("stratum.min_difficulty must be <= initial_difficulty".to_owned());
    }
    if config.stratum.initial_difficulty > config.stratum.max_difficulty {
        errors.push("stratum.initial_difficulty must be <= max_difficulty".to_owned());
    }
    if config.stratum.target_share_secs == 0 {
        errors.push("stratum.target_share_secs must be greater than 0".to_owned());
    }
    if config.abuse.max_connections_per_ip == 0
        || config.abuse.max_sessions_per_address == 0
        || config.abuse.malformed_frame_limit == 0
        || config.abuse.auth_failure_limit == 0
        || config.abuse.invalid_share_limit == 0
        || config.abuse.ban_secs == 0
    {
        errors.push("abuse limits must be greater than 0".to_owned());
    }

    let mut role_coverage = HashSet::new();
    let mut node_summaries = Vec::new();
    if config.csd_nodes.is_empty() {
        errors.push("csd_nodes must include at least one node".to_owned());
    }
    for node in &config.csd_nodes {
        let roles = node_roles(&node.role);
        for role in &roles {
            role_coverage.insert(role.clone());
        }
        if node.name.trim().is_empty() {
            errors.push("csd_nodes.name must not be empty".to_owned());
        }
        if !(node.rpc_url.starts_with("http://") || node.rpc_url.starts_with("https://")) {
            errors.push(format!(
                "csd_nodes.{}.rpc_url must start with http:// or https://",
                node.name
            ));
        }
        if roles.is_empty() {
            errors.push(format!("csd_nodes.{}.role must not be empty", node.name));
        }
        node_summaries.push(ConfigNodeSummary {
            name: node.name.clone(),
            rpc_url: node.rpc_url.clone(),
            roles,
        });
    }
    for required_role in ["template", "submit", "watch"] {
        if !role_coverage.contains(required_role) {
            errors.push(format!("csd_nodes must include a {required_role} role"));
        }
    }

    let env_checks = config_env_checks(&config, require_env, &mut errors);
    if !require_env {
        warnings.push(
            "environment variable presence is advisory; set CSD_POOL_CHECK_CONFIG_REQUIRE_ENV=1 to fail on missing env".to_owned(),
        );
    }

    Ok(ConfigCheckRun {
        passed: errors.is_empty(),
        config_path: config_path.display().to_string(),
        require_env,
        pool_id: config.pool.id,
        mining_address: config.pool.mining_address,
        fee_percent: config.pool.fee_percent,
        confirm_depth: config.pool.confirm_depth,
        stratum_listen,
        api_listen,
        signer_listen,
        minimum_payout_base_units,
        manual_payout_approval_base_units,
        max_payout_batch_base_units,
        max_daily_payout_base_units,
        nodes: node_summaries,
        env: env_checks,
        warnings,
        errors,
    })
}

async fn accounting_export(path_arg: Option<&str>) -> Result<()> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let repo = PgRepository::connect(&database_url).await?;
    csd_pool_db::run_migrations(repo.pool()).await?;
    let entries = repo.list_ledger_entries().await?;
    let csv = ledger_entries_csv(&entries);
    if let Some(path) = path_arg.filter(|path| !path.is_empty()) {
        if let Some(parent) = Path::new(path)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, csv)?;
        print_json(&AccountingExportRun {
            path: Some(path.to_owned()),
            exported_entries: entries.len(),
        })?;
    } else {
        print!("{csv}");
    }
    Ok(())
}

async fn stratum_smoke(endpoint_arg: Option<&str>) -> Result<StratumSmokeRun> {
    let config = stratum_smoke_config(endpoint_arg);
    run_stratum_clients(config).await
}

async fn stratum_load_test(endpoint_arg: Option<&str>) -> Result<StratumLoadTestRun> {
    let config = stratum_load_test_config(endpoint_arg);
    let min_success = config.min_success;
    let smoke = run_stratum_clients(config.smoke).await?;
    Ok(stratum_load_test_run_from_smoke(smoke, min_success))
}

async fn stratum_submit_probe(endpoint_arg: Option<&str>) -> Result<StratumSubmitProbeRun> {
    let endpoint = endpoint_arg
        .map(str::to_owned)
        .or_else(|| std::env::var("CSD_POOL_STRATUM_ADDR").ok())
        .unwrap_or_else(|| "127.0.0.1:3333".to_owned());
    stratum_submit_probe_client(endpoint, SubmitProbeMode::LowDifficulty).await
}

async fn stratum_accepted_share_probe(endpoint_arg: Option<&str>) -> Result<StratumSubmitProbeRun> {
    let endpoint = endpoint_arg
        .map(str::to_owned)
        .or_else(|| std::env::var("CSD_POOL_STRATUM_ADDR").ok())
        .unwrap_or_else(|| "127.0.0.1:3333".to_owned());
    stratum_submit_probe_client(endpoint, SubmitProbeMode::KnownAcceptedStatic).await
}

fn stratum_load_test_run_from_smoke(
    smoke: StratumSmokeRun,
    min_success: usize,
) -> StratumLoadTestRun {
    let passed = smoke.failed_clients == 0 && smoke.succeeded_clients >= min_success;
    let connections_per_sec = if smoke.elapsed_ms > 0.0 {
        smoke.succeeded_clients as f64 / (smoke.elapsed_ms / 1000.0)
    } else {
        0.0
    };

    StratumLoadTestRun {
        endpoint: smoke.endpoint,
        requested_clients: smoke.requested_clients,
        min_success_clients: min_success,
        succeeded_clients: smoke.succeeded_clients,
        failed_clients: smoke.failed_clients,
        passed,
        elapsed_ms: smoke.elapsed_ms,
        connections_per_sec,
        min_client_ms: smoke.min_client_ms,
        avg_client_ms: smoke.avg_client_ms,
        max_client_ms: smoke.max_client_ms,
        failures: smoke.failures,
    }
}

async fn run_stratum_clients(config: StratumSmokeConfig) -> Result<StratumSmokeRun> {
    let started = Instant::now();
    let mut handles = Vec::with_capacity(config.clients);

    for client_index in 0..config.clients {
        let endpoint = config.endpoint.clone();
        let malformed = config.malformed;
        handles.push(tokio::spawn(async move {
            stratum_smoke_client(client_index, endpoint, malformed).await
        }));
    }

    let mut clients = Vec::with_capacity(config.clients);
    for handle in handles {
        clients.push(match handle.await {
            Ok(outcome) => outcome,
            Err(err) => StratumSmokeClientResult {
                client_index: clients.len(),
                worker: smoke_worker_name(clients.len()),
                ok: false,
                elapsed_ms: 0.0,
                extranonce1_hex: None,
                difficulty_seen: false,
                notify_seen: false,
                error: Some(format!("task join failed: {err}")),
            },
        });
    }

    let succeeded_clients = clients.iter().filter(|client| client.ok).count();
    let failed_clients = clients.len().saturating_sub(succeeded_clients);
    let mut latencies = clients
        .iter()
        .filter(|client| client.ok)
        .map(|client| client.elapsed_ms);
    let first_latency = latencies.next();
    let (min_client_ms, avg_client_ms, max_client_ms) = match first_latency {
        Some(first) => {
            let mut min = first;
            let mut max = first;
            let mut sum = first;
            let mut count = 1.0;
            for latency in latencies {
                min = min.min(latency);
                max = max.max(latency);
                sum += latency;
                count += 1.0;
            }
            (Some(min), Some(sum / count), Some(max))
        }
        None => (None, None, None),
    };

    let failures = clients
        .iter()
        .filter_map(|client| {
            client.error.as_ref().map(|error| StratumSmokeFailure {
                client_index: client.client_index,
                worker: client.worker.clone(),
                error: error.clone(),
            })
        })
        .take(20)
        .collect();
    let successes = clients
        .iter()
        .filter(|client| client.ok)
        .map(|client| StratumSmokeSuccess {
            client_index: client.client_index,
            worker: client.worker.clone(),
            elapsed_ms: client.elapsed_ms,
            extranonce1_hex: client.extranonce1_hex.clone(),
            difficulty_seen: client.difficulty_seen,
            notify_seen: client.notify_seen,
        })
        .take(20)
        .collect();

    Ok(StratumSmokeRun {
        endpoint: config.endpoint,
        requested_clients: config.clients,
        succeeded_clients,
        failed_clients,
        malformed_sent: config.malformed,
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        min_client_ms,
        avg_client_ms,
        max_client_ms,
        successes,
        failures,
    })
}

async fn stratum_smoke_client(
    client_index: usize,
    endpoint: String,
    malformed: bool,
) -> StratumSmokeClientResult {
    let started = Instant::now();
    let worker = smoke_worker_name(client_index);
    let mut result = StratumSmokeClientResult {
        client_index,
        worker: worker.clone(),
        ok: false,
        elapsed_ms: 0.0,
        extranonce1_hex: None,
        difficulty_seen: false,
        notify_seen: false,
        error: None,
    };

    let stream = match timeout(
        Duration::from_secs(smoke_timeout_secs()),
        TcpStream::connect(&endpoint),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => return result.failed(started, format!("connect failed: {err}")),
        Err(_) => return result.failed(started, "connect timed out".to_owned()),
    };
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    if malformed && let Err(err) = write_with_timeout(&mut write_half, b"{bad-json\n").await {
        return result.failed(started, format!("malformed write failed: {err}"));
    }

    let subscribe_line = match serialize_line(&subscribe_request(1, "csd-pool-smoke/0.1")) {
        Ok(line) => line,
        Err(err) => return result.failed(started, err.to_string()),
    };
    if let Err(err) = write_with_timeout(&mut write_half, subscribe_line.as_bytes()).await {
        return result.failed(started, format!("subscribe write failed: {err}"));
    }

    let subscribe_response = match read_response_with_timeout(&mut reader).await {
        Ok(response) => response,
        Err(err) => return result.failed(started, format!("subscribe read failed: {err}")),
    };
    if subscribe_response.error.is_some() {
        return result.failed(
            started,
            format!("subscribe rejected: {:?}", subscribe_response.error),
        );
    }
    result.extranonce1_hex = subscribe_response
        .result
        .as_array()
        .and_then(|items| items.get(1))
        .and_then(|value| value.as_str())
        .map(str::to_owned);

    let authorize_line = match serialize_line(&authorize_request(2, &worker)) {
        Ok(line) => line,
        Err(err) => return result.failed(started, err.to_string()),
    };
    if let Err(err) = write_with_timeout(&mut write_half, authorize_line.as_bytes()).await {
        return result.failed(started, format!("authorize write failed: {err}"));
    }

    let authorize_response = match read_response_with_timeout(&mut reader).await {
        Ok(response) => response,
        Err(err) => return result.failed(started, format!("authorize read failed: {err}")),
    };
    if authorize_response.error.is_some() || authorize_response.result.as_bool() != Some(true) {
        return result.failed(
            started,
            format!(
                "authorize rejected: result={:?} error={:?}",
                authorize_response.result, authorize_response.error
            ),
        );
    }

    for _ in 0..4 {
        let push = match read_json_with_timeout(&mut reader).await {
            Ok(push) => push,
            Err(err) => return result.failed(started, format!("push read failed: {err}")),
        };
        match push.get("method").and_then(|method| method.as_str()) {
            Some("mining.set_difficulty") => result.difficulty_seen = true,
            Some("mining.notify") => result.notify_seen = true,
            _ => {}
        }
        if result.difficulty_seen && result.notify_seen {
            result.ok = true;
            result.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            return result;
        }
    }

    let error = format!(
        "missing required pushes: difficulty_seen={} notify_seen={}",
        result.difficulty_seen, result.notify_seen
    );
    result.failed(started, error)
}

async fn stratum_submit_probe_client(
    endpoint: String,
    mode: SubmitProbeMode,
) -> Result<StratumSubmitProbeRun> {
    let started = Instant::now();
    let worker = smoke_worker_name(0);
    let mut run = StratumSubmitProbeRun {
        endpoint: endpoint.clone(),
        worker: worker.clone(),
        mode: mode.as_str().to_owned(),
        passed: false,
        elapsed_ms: 0.0,
        extranonce1_hex: None,
        extranonce2_size: None,
        difficulty_seen: false,
        notify_seen: false,
        difficulty: None,
        job_id: None,
        submit_result: None,
        submit_error_code: None,
        submit_error_message: None,
        submit_response_received: false,
        submit_response_standard: false,
        error: None,
    };

    let stream = match timeout(
        Duration::from_secs(smoke_timeout_secs()),
        TcpStream::connect(&endpoint),
    )
    .await
    {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => return Ok(run.failed(started, format!("connect failed: {err}"))),
        Err(_) => return Ok(run.failed(started, "connect timed out".to_owned())),
    };
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let subscribe_line = serialize_line(&subscribe_request(1, "csd-pool-submit-probe/0.1"))?;
    if let Err(err) = write_with_timeout(&mut write_half, subscribe_line.as_bytes()).await {
        return Ok(run.failed(started, format!("subscribe write failed: {err}")));
    }
    let subscribe_response = match read_response_with_timeout(&mut reader).await {
        Ok(response) => response,
        Err(err) => return Ok(run.failed(started, format!("subscribe read failed: {err}"))),
    };
    if subscribe_response.error.is_some() {
        return Ok(run.failed(
            started,
            format!("subscribe rejected: {:?}", subscribe_response.error),
        ));
    }
    match SubscribeResult::parse(&subscribe_response.result) {
        Ok(result) => {
            run.extranonce1_hex = Some(result.extranonce1_hex);
            run.extranonce2_size = Some(result.extranonce2_size);
        }
        Err(err) => return Ok(run.failed(started, format!("subscribe parse failed: {err}"))),
    }

    let authorize_line = serialize_line(&authorize_request(2, &worker))?;
    if let Err(err) = write_with_timeout(&mut write_half, authorize_line.as_bytes()).await {
        return Ok(run.failed(started, format!("authorize write failed: {err}")));
    }
    let authorize_response = match read_response_with_timeout(&mut reader).await {
        Ok(response) => response,
        Err(err) => return Ok(run.failed(started, format!("authorize read failed: {err}"))),
    };
    if authorize_response.error.is_some() || authorize_response.result.as_bool() != Some(true) {
        return Ok(run.failed(
            started,
            format!(
                "authorize rejected: result={:?} error={:?}",
                authorize_response.result, authorize_response.error
            ),
        ));
    }

    let mut notify_params = None;
    for _ in 0..6 {
        let push = match read_json_with_timeout(&mut reader).await {
            Ok(push) => push,
            Err(err) => return Ok(run.failed(started, format!("push read failed: {err}"))),
        };
        match push.get("method").and_then(|method| method.as_str()) {
            Some("mining.set_difficulty") => {
                run.difficulty_seen = true;
                run.difficulty = push
                    .get("params")
                    .and_then(|params| params.as_array())
                    .and_then(|items| items.first())
                    .and_then(|value| value.as_f64());
            }
            Some("mining.notify") => {
                match NotifyParams::parse(push.get("params").unwrap_or(&serde_json::Value::Null)) {
                    Ok(params) => {
                        run.notify_seen = true;
                        run.job_id = Some(params.job_id.clone());
                        notify_params = Some(params);
                    }
                    Err(err) => {
                        return Ok(run.failed(started, format!("notify parse failed: {err}")));
                    }
                }
            }
            _ => {}
        }
        if run.difficulty_seen && notify_params.is_some() {
            break;
        }
    }
    let Some(notify) = notify_params else {
        return Ok(run.failed(started, "missing mining.notify push".to_owned()));
    };

    let (extranonce2_hex, nonce_hex) = match mode {
        SubmitProbeMode::LowDifficulty => (
            "00".repeat(run.extranonce2_size.unwrap_or(4)),
            "00000000".to_owned(),
        ),
        SubmitProbeMode::KnownAcceptedStatic => {
            match find_static_accepted_submit(
                &notify,
                run.extranonce1_hex.as_deref().unwrap_or_default(),
                run.extranonce2_size.unwrap_or(4),
                run.difficulty.unwrap_or(1.0),
            ) {
                Ok(solution) => solution,
                Err(error) => return Ok(run.failed(started, error)),
            }
        }
    };
    let submit = SubmitParams {
        worker_name: worker,
        job_id: notify.job_id,
        extranonce2_hex,
        ntime_hex: notify.ntime_hex,
        nonce_hex,
    };
    let submit_line = serialize_line(&submit_request(3, &submit))?;
    if let Err(err) = write_with_timeout(&mut write_half, submit_line.as_bytes()).await {
        return Ok(run.failed(started, format!("submit write failed: {err}")));
    }
    let submit_response = match read_response_with_timeout(&mut reader).await {
        Ok(response) => response,
        Err(err) => return Ok(run.failed(started, format!("submit read failed: {err}"))),
    };
    run.submit_response_received = true;
    run.submit_result = submit_response.result.as_bool();
    if let Some(error) = submit_response
        .error
        .as_ref()
        .and_then(|value| value.as_array())
    {
        run.submit_error_code = error.first().and_then(|value| value.as_i64());
        run.submit_error_message = error
            .get(1)
            .and_then(|value| value.as_str())
            .map(str::to_owned);
    }
    run.submit_response_standard = submit_response.id == Some(3)
        && (run.submit_result.is_some() || submit_response.error.is_some());
    run.passed = match mode {
        SubmitProbeMode::LowDifficulty => {
            run.difficulty_seen
                && run.notify_seen
                && run.submit_response_received
                && run.submit_response_standard
                && (run.submit_result == Some(true) || run.submit_error_code.is_some())
        }
        SubmitProbeMode::KnownAcceptedStatic => {
            run.difficulty_seen
                && run.notify_seen
                && run.submit_response_received
                && run.submit_response_standard
                && run.submit_result == Some(true)
        }
    };
    run.elapsed_ms = elapsed_instant_ms(started);
    Ok(run)
}

fn find_static_accepted_submit(
    notify: &NotifyParams,
    extranonce1_hex: &str,
    extranonce2_size: usize,
    difficulty: f64,
) -> std::result::Result<(String, String), String> {
    if extranonce2_size != 4 {
        return Err(format!(
            "known accepted static probe requires extranonce2_size=4, got {extranonce2_size}"
        ));
    }
    if notify.job_id != "static-1" {
        return Err(format!(
            "known accepted static probe requires static-1 job, got {}",
            notify.job_id
        ));
    }
    let extranonce1_value = u32::from_str_radix(extranonce1_hex, 16)
        .map_err(|_| format!("invalid extranonce1_hex: {extranonce1_hex}"))?;
    let extranonce1_le = extranonce1_value.to_le_bytes();
    let job = csd_pool_node::easy_static_job(notify.job_id.clone());
    let extranonce2_hex = "01020304".to_owned();
    let extranonce2_le = [1, 2, 3, 4];
    let ntime = u32::from_str_radix(&notify.ntime_hex, 16)
        .map_err(|_| format!("invalid ntime_hex: {}", notify.ntime_hex))?;

    for nonce in 0..1_000_000_u32 {
        let solution = SubmitSolution {
            extranonce2_le,
            ntime,
            nonce,
        };
        if verify_share_with_difficulty(&job.template, extranonce1_le, &solution, difficulty)
            .is_ok()
        {
            return Ok((extranonce2_hex, format!("{nonce:08x}")));
        }
    }
    Err(format!(
        "no accepted static share found within nonce search limit for difficulty {difficulty}"
    ))
}

async fn reconcile_blocks() -> Result<ReconcileBlocksRun> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let node_url = watch_node_url()?.ok_or(WorkerError::MissingNodeUrl)?;
    let repo = PgRepository::connect(&database_url).await?;
    csd_pool_db::run_migrations(repo.pool()).await?;
    let node = CsdNodeClient::from_env(node_url);
    let pending = repo.list_blocks_to_reconcile(100).await?;
    let required_confirmations = confirm_depth();
    let mut updates = Vec::with_capacity(pending.len());

    for block in pending {
        let status = node.block_status(&block.hash_hex).await?;
        let update = block_update_from_status(&block.hash_hex, &status, required_confirmations);
        let updated = repo.update_block_status(&update).await?;
        updates.push(ReconciledBlock {
            hash_hex: update.hash_hex,
            status: update.status,
            height: update.height,
            confirmations: update.confirmations,
            reward_base_units: update.reward_base_units,
            updated,
        });
    }

    Ok(ReconcileBlocksRun {
        reconciled_count: updates.len(),
        updates,
    })
}

async fn check_node_template() -> Result<NodeTemplateCheckRun> {
    let template_node_url = template_node_url()?.ok_or(WorkerError::MissingTemplateNodeUrl)?;
    let pool_address = mining_address()?.ok_or(WorkerError::MissingMiningAddress)?;
    let submit_node_url = submit_node_url()?;
    let node = CsdNodeClient::from_env(template_node_url.clone());
    let adapter_auth_required = std::env::var("CSD_POOL_NODE_TOKEN")
        .ok()
        .is_some_and(|token| !token.is_empty());

    let (unauthenticated_template_status, adapter_auth_boundary_ok, adapter_auth_error) =
        if adapter_auth_required {
            let result = reqwest::Client::new()
                .get(format!("{template_node_url}/api/rpc/mining/template"))
                .query(&[("address", pool_address.as_str())])
                .send()
                .await;
            match result {
                Ok(response) => {
                    let status = response.status().as_u16();
                    (Some(status), status == 401, None)
                }
                Err(error) => (None, false, Some(error.to_string())),
            }
        } else {
            (None, true, None)
        };

    let started = SystemTime::now();
    let health = node.health().await;
    let health_ms = elapsed_ms(started);
    let health_ok = health.is_ok();
    let health_error = health.err().map(|err| err.to_string());

    let started = SystemTime::now();
    let network = node.network().await;
    let network_ms = elapsed_ms(started);
    let (network_ok, network_hashrate_hs, target_block_secs, network_error) = match network {
        Ok(network) => (
            true,
            if network.hashrate > 0.0 {
                Some(network.hashrate)
            } else if network.hashrate_ghs > 0.0 {
                Some(network.hashrate_ghs * 1_000_000_000.0)
            } else {
                None
            },
            (network.target_block_secs > 0).then_some(network.target_block_secs),
            None,
        ),
        Err(err) => (false, None, None, Some(err.to_string())),
    };

    let started = SystemTime::now();
    let template_result = node.mining_template(&pool_address).await;
    let template_ms = elapsed_ms(started);
    let template_check = match template_result {
        Ok(template) => node_template_summary(template),
        Err(err) => NodeTemplateSummary {
            ok: false,
            error: Some(err.to_string()),
            ..NodeTemplateSummary::default()
        },
    };

    let submit_health = if let Some(url) = submit_node_url.as_ref() {
        let submit = CsdNodeClient::from_env(url.clone());
        let started = SystemTime::now();
        let result = submit.health().await;
        Some(NodeEndpointCheck {
            node_url: url.clone(),
            ok: result.is_ok(),
            latency_ms: elapsed_ms(started),
            error: result.err().map(|err| err.to_string()),
        })
    } else {
        None
    };

    let passed = template_check.ok && adapter_auth_boundary_ok;
    Ok(NodeTemplateCheckRun {
        passed,
        template_node_url,
        pool_address,
        adapter_auth_required,
        adapter_auth_boundary_ok,
        unauthenticated_template_status,
        adapter_auth_error,
        health_ok,
        health_ms,
        health_error,
        network_ok,
        network_ms,
        network_hashrate_hs,
        target_block_secs,
        network_error,
        template_ms,
        template_ok: template_check.ok,
        template_error: template_check.error,
        job_id: template_check.job_id,
        clean_jobs: template_check.clean_jobs,
        merkle_branches: template_check.merkle_branches,
        coinbase_prefix_bytes: template_check.coinbase_prefix_bytes,
        coinbase_suffix_bytes: template_check.coinbase_suffix_bytes,
        share_target_hex: template_check.share_target_hex,
        network_target_hex: template_check.network_target_hex,
        submit_node_url,
        submit_health,
    })
}

async fn mine_node_candidate_canary() -> Result<NodeCandidateCanaryRun> {
    if std::env::var("CSD_POOL_NODE_CANDIDATE_CANARY_CONFIRM").as_deref() != Ok("mine-and-submit") {
        return Err(WorkerError::CandidateCanaryConfirmationMissing);
    }

    let template_node_url = template_node_url()?.ok_or(WorkerError::MissingTemplateNodeUrl)?;
    let submit_node_url = submit_node_url()?.unwrap_or_else(|| template_node_url.clone());
    if submit_node_url != template_node_url {
        return Err(WorkerError::CandidateCanaryRejected(
            "template and submit URL must match because adapter jobs are node-local".to_owned(),
        ));
    }
    let pool_address = mining_address()?.ok_or(WorkerError::MissingMiningAddress)?;
    let extranonce1_le = parse_le_u32_hex_bytes(
        "CSD_POOL_NODE_CANDIDATE_CANARY_EXTRANONCE1",
        &std::env::var("CSD_POOL_NODE_CANDIDATE_CANARY_EXTRANONCE1")
            .unwrap_or_else(|_| "00000000".to_owned()),
    )?;
    let extranonce2_le = parse_le_u32_hex_bytes(
        "CSD_POOL_NODE_CANDIDATE_CANARY_EXTRANONCE2",
        &std::env::var("CSD_POOL_NODE_CANDIDATE_CANARY_EXTRANONCE2")
            .unwrap_or_else(|_| "00000000".to_owned()),
    )?;
    let start_nonce = env_u64("CSD_POOL_NODE_CANDIDATE_CANARY_START_NONCE")
        .unwrap_or(0)
        .min(u64::from(u32::MAX));
    let max_attempts = env_u64("CSD_POOL_NODE_CANDIDATE_CANARY_MAX_ATTEMPTS")
        .unwrap_or(u64::from(u32::MAX) + 1)
        .clamp(1, u64::from(u32::MAX) + 1 - start_nonce);
    let threads = env_usize(
        "CSD_POOL_NODE_CANDIDATE_CANARY_THREADS",
        std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1),
    )
    .clamp(1, 256);

    let node = CsdNodeClient::from_env(template_node_url.clone());
    let pool_job = node.mining_template(&pool_address).await?.into_pool_job()?;
    let extranonce = compose_extranonce(extranonce1_le, extranonce2_le);
    let coinbase_txid = coinbase_txid(
        &pool_job.template.coinbase_prefix,
        extranonce,
        &pool_job.template.coinbase_suffix,
    );
    let merkle_root = merkle_root_from_branch(coinbase_txid, &pool_job.template.merkle_branch);
    let header = header_84(
        pool_job.template.version,
        &pool_job.template.prev,
        &merkle_root,
        pool_job.template.time,
        pool_job.template.bits,
        0,
    );

    let started = Instant::now();
    let search = search_candidate_nonce(
        header,
        pool_job.template.network_target,
        start_nonce as u32,
        max_attempts,
        threads,
    );
    let Some((nonce, hash, attempted_hashes)) = search else {
        return Err(WorkerError::CandidateCanaryExhausted {
            attempts: max_attempts,
        });
    };
    let elapsed_ms = elapsed_instant_ms(started);
    let mut solved_header = header;
    solved_header[80..84].copy_from_slice(&nonce.to_le_bytes());
    let coinbase = coinbase_bytes(
        &pool_job.template.coinbase_prefix,
        extranonce,
        &pool_job.template.coinbase_suffix,
    );
    let request = BlockCandidateSubmitRequest {
        job_id: pool_job.template.job_id.clone(),
        worker_name: "node-candidate-canary".to_owned(),
        header_hex: hex::encode(solved_header),
        hash_hex: hex::encode(hash),
        coinbase_txid_hex: hex::encode(coinbase_txid),
        coinbase_hex: hex::encode(coinbase),
        merkle_root_hex: hex::encode(merkle_root),
        extranonce2_hex: hex::encode(extranonce2_le),
        ntime_hex: format!("{:08x}", pool_job.template.time),
        nonce_hex: format!("{nonce:08x}"),
    };
    let submit = node.submit_candidate(&request).await?;
    if !submit.ok {
        return Err(WorkerError::CandidateCanaryRejected(
            submit.extra.to_string(),
        ));
    }
    let accepted_hash = submit.hash.unwrap_or_else(|| request.hash_hex.clone());
    let status_deadline = Instant::now() + Duration::from_secs(10);
    let final_status = loop {
        let status = node.block_status(&accepted_hash).await?;
        if status.confirmations >= 1 && !matches!(status.status.as_str(), "orphan" | "orphaned") {
            break status;
        }
        if Instant::now() >= status_deadline {
            return Err(WorkerError::CandidateCanaryRejected(format!(
                "submitted block did not become canonical: status={} confirmations={}",
                status.status, status.confirmations
            )));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };

    Ok(NodeCandidateCanaryRun {
        passed: true,
        template_node_url,
        submit_node_url,
        pool_address,
        job_id: request.job_id,
        hash_hex: accepted_hash,
        nonce,
        attempted_hashes,
        search_threads: threads,
        search_elapsed_ms: elapsed_ms,
        search_hashrate_hs: if elapsed_ms > 0.0 {
            attempted_hashes as f64 / (elapsed_ms / 1000.0)
        } else {
            0.0
        },
        status: final_status.status,
        confirmations: final_status.confirmations,
        height: final_status.height,
        reward_base_units: final_status.reward_base_units,
    })
}

fn search_candidate_nonce(
    header: [u8; 84],
    target: [u8; 32],
    start_nonce: u32,
    max_attempts: u64,
    threads: usize,
) -> Option<(u32, [u8; 32], u64)> {
    let stop = Arc::new(AtomicBool::new(false));
    let attempts = Arc::new(AtomicU64::new(0));
    let result = Arc::new(Mutex::new(None));
    let start = u64::from(start_nonce);
    let end = start
        .saturating_add(max_attempts)
        .min(u64::from(u32::MAX) + 1);
    let span = end.saturating_sub(start);
    let chunk = span.div_ceil(threads as u64);
    let mut handles = Vec::with_capacity(threads);

    for thread_index in 0..threads {
        let from = start.saturating_add(chunk.saturating_mul(thread_index as u64));
        let to = from.saturating_add(chunk).min(end);
        if from >= to {
            continue;
        }
        let stop = stop.clone();
        let attempts = attempts.clone();
        let result = result.clone();
        handles.push(std::thread::spawn(move || {
            let mut candidate_header = header;
            for nonce in from..to {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                candidate_header[80..84].copy_from_slice(&(nonce as u32).to_le_bytes());
                let hash = header_hash(&candidate_header);
                attempts.fetch_add(1, Ordering::Relaxed);
                if hash_leq_target(&hash, &target) {
                    if !stop.swap(true, Ordering::SeqCst) {
                        *result.lock().expect("candidate result lock poisoned") =
                            Some((nonce as u32, hash));
                    }
                    break;
                }
            }
        }));
    }
    for handle in handles {
        handle.join().expect("candidate search thread panicked");
    }
    let found = *result.lock().expect("candidate result lock poisoned");
    found.map(|(nonce, hash)| (nonce, hash, attempts.load(Ordering::Relaxed)))
}

async fn check_node_runtime() -> Result<NodeRuntimeCheckRun> {
    let nodes = configured_nodes();
    let pool_address = mining_address()?;
    let max_health_ms = env_u64("CSD_POOL_NODE_RUNTIME_MAX_HEALTH_MS").unwrap_or(2_000);
    let max_network_ms = env_u64("CSD_POOL_NODE_RUNTIME_MAX_NETWORK_MS").unwrap_or(3_000);
    let max_template_ms = env_u64("CSD_POOL_NODE_RUNTIME_MAX_TEMPLATE_MS").unwrap_or(5_000);
    let min_template_nodes = env_usize("CSD_POOL_NODE_RUNTIME_MIN_TEMPLATE_NODES", 1).clamp(1, 100);
    let min_submit_nodes = env_usize("CSD_POOL_NODE_RUNTIME_MIN_SUBMIT_NODES", 2).clamp(1, 100);
    let min_watch_nodes = env_usize("CSD_POOL_NODE_RUNTIME_MIN_WATCH_NODES", 2).clamp(1, 100);

    let configured_template_nodes = nodes
        .iter()
        .filter(|node| role_includes(&node.role, "template"))
        .count();
    let configured_submit_nodes = nodes
        .iter()
        .filter(|node| role_includes(&node.role, "submit"))
        .count();
    let configured_watch_nodes = nodes
        .iter()
        .filter(|node| role_includes(&node.role, "watch"))
        .count();

    let mut checks = Vec::with_capacity(nodes.len());
    for node in nodes {
        let client = CsdNodeClient::from_env(node.rpc_url.clone());

        let started = SystemTime::now();
        let health = client.health().await;
        let health_ms = elapsed_ms(started);
        let health_ok = health.is_ok();
        let health_error = health.err().map(|err| err.to_string());

        let started = SystemTime::now();
        let network = client.network().await;
        let network_ms = elapsed_ms(started);
        let (network_ok, network_hashrate_hs, target_block_secs, network_error) = match network {
            Ok(network) => (
                true,
                if network.hashrate > 0.0 {
                    Some(network.hashrate)
                } else if network.hashrate_ghs > 0.0 {
                    Some(network.hashrate_ghs * 1_000_000_000.0)
                } else {
                    None
                },
                (network.target_block_secs > 0).then_some(network.target_block_secs),
                None,
            ),
            Err(err) => (false, None, None, Some(err.to_string())),
        };

        let template_role = role_includes(&node.role, "template");
        let (
            template_ok,
            template_ms,
            template_error,
            job_id,
            share_target_hex,
            network_target_hex,
        ) = if template_role {
            match pool_address.as_ref() {
                Some(address) => {
                    let started = SystemTime::now();
                    let template_result = client.mining_template(address).await;
                    let template_ms = elapsed_ms(started);
                    match template_result {
                        Ok(template) => {
                            let summary = node_template_summary(template);
                            (
                                summary.ok,
                                Some(template_ms),
                                summary.error,
                                summary.job_id,
                                summary.share_target_hex,
                                summary.network_target_hex,
                            )
                        }
                        Err(err) => (
                            false,
                            Some(template_ms),
                            Some(err.to_string()),
                            None,
                            None,
                            None,
                        ),
                    }
                }
                None => (
                    false,
                    None,
                    Some("missing mining address for template probe".to_owned()),
                    None,
                    None,
                    None,
                ),
            }
        } else {
            (true, None, None, None, None, None)
        };

        checks.push(NodeRuntimeNodeCheck {
            name: node.name,
            rpc_url: node.rpc_url,
            role: node.role,
            health_ok,
            health_ms,
            health_error,
            network_ok,
            network_ms,
            network_hashrate_hs,
            target_block_secs,
            network_error,
            template_ok,
            template_ms,
            template_error,
            job_id,
            share_target_hex,
            network_target_hex,
        });
    }

    let healthy_template_nodes = checks
        .iter()
        .filter(|node| {
            node.template_role() && node.health_ok && node.network_ok && node.template_ok
        })
        .count();
    let healthy_submit_nodes = checks
        .iter()
        .filter(|node| node.submit_role() && node.health_ok && node.network_ok)
        .count();
    let healthy_watch_nodes = checks
        .iter()
        .filter(|node| node.watch_role() && node.health_ok && node.network_ok)
        .count();
    let role_quorum_ok = configured_template_nodes >= min_template_nodes
        && configured_submit_nodes >= min_submit_nodes
        && configured_watch_nodes >= min_watch_nodes;
    let health_quorum_ok = healthy_template_nodes >= min_template_nodes
        && healthy_submit_nodes >= min_submit_nodes
        && healthy_watch_nodes >= min_watch_nodes;
    let network_ok = !checks.is_empty() && checks.iter().all(|node| node.network_ok);
    let template_contract_ok = configured_template_nodes > 0
        && checks
            .iter()
            .filter(|node| node.template_role())
            .all(|node| node.template_ok);
    let latency_ok = checks.iter().all(|node| {
        node.health_ms <= max_health_ms as f64
            && node.network_ms <= max_network_ms as f64
            && node
                .template_ms
                .map(|latency| latency <= max_template_ms as f64)
                .unwrap_or(true)
    });

    let mut failed_checks = Vec::new();
    if checks.is_empty() {
        failed_checks.push("config_nodes_present".to_owned());
    }
    if !role_quorum_ok {
        failed_checks.push("role_quorum_ok".to_owned());
    }
    if !health_quorum_ok {
        failed_checks.push("health_quorum_ok".to_owned());
    }
    if !network_ok {
        failed_checks.push("network_ok".to_owned());
    }
    if !template_contract_ok {
        failed_checks.push("template_contract_ok".to_owned());
    }
    if !latency_ok {
        failed_checks.push("latency_ok".to_owned());
    }
    for node in &checks {
        if !node.health_ok {
            failed_checks.push(format!("{}.health_ok", node.name));
        }
        if !node.network_ok {
            failed_checks.push(format!("{}.network_ok", node.name));
        }
        if node.template_role() && !node.template_ok {
            failed_checks.push(format!("{}.template_ok", node.name));
        }
    }

    Ok(NodeRuntimeCheckRun {
        passed: failed_checks.is_empty(),
        failed_checks,
        config_node_count: checks.len(),
        configured_template_nodes,
        configured_submit_nodes,
        configured_watch_nodes,
        healthy_template_nodes,
        healthy_submit_nodes,
        healthy_watch_nodes,
        min_template_nodes,
        min_submit_nodes,
        min_watch_nodes,
        max_health_ms,
        max_network_ms,
        max_template_ms,
        role_quorum_ok,
        health_quorum_ok,
        network_ok,
        template_contract_ok,
        latency_ok,
        nodes: checks,
    })
}

async fn check_signer() -> Result<SignerCheckRun> {
    let signer_url = signer_url()?.ok_or(WorkerError::MissingSignerUrl)?;
    let expected_wallet_address = signer_wallet_address();
    let signer = PayoutSignerClient::new(signer_url.clone(), signer_token()?);

    let started = SystemTime::now();
    let health = signer.health().await;
    let health_ms = elapsed_ms(started);
    let health_ok = health.is_ok();
    let (health_service, health_mode, health_wallet_address, health_error) = match health {
        Ok(health) => (
            health.service,
            health.mode,
            health
                .wallet_address
                .and_then(|value| normalize_addr20(&value)),
            None,
        ),
        Err(err) => (None, None, None, Some(err.to_string())),
    };

    let request = signer_contract_request(expected_wallet_address.as_deref());
    let started = SystemTime::now();
    let signed = signer.sign_request(&request).await;
    let sign_ms = elapsed_ms(started);
    let (
        sign_ok,
        raw_tx_hex,
        raw_tx_hex_len,
        raw_tx_mock_prefix_present,
        node_tx,
        node_tx_present,
        node_tx_valid,
        node_tx_outputs_match_request,
        txid,
        sign_error,
    ) = match signed {
        Ok(signed) => match validate_signed_payout_response(&signed, &request) {
            Ok(validation) => (
                true,
                signed.raw_tx_hex.clone(),
                signed.raw_tx_hex.as_deref().map(str::len).unwrap_or(0),
                signed_payout_has_mock_prefix(&signed),
                signed.node_tx,
                validation.node_tx_present,
                validation.node_tx_valid,
                validation.node_tx_outputs_match_request,
                Some(signed.txid),
                None::<String>,
            ),
            Err(err) => (
                false,
                signed.raw_tx_hex.clone(),
                signed.raw_tx_hex.as_deref().map(str::len).unwrap_or(0),
                signed_payout_has_mock_prefix(&signed),
                signed.node_tx.clone(),
                signed.node_tx.is_some(),
                false,
                false,
                Some(signed.txid),
                Some(err),
            ),
        },
        Err(err) => (
            false,
            None,
            0,
            false,
            None,
            false,
            false,
            false,
            None,
            Some(err.to_string()),
        ),
    };

    let passed = health_ok && sign_ok;
    Ok(SignerCheckRun {
        passed,
        signer_url,
        health_ok,
        health_ms,
        health_service,
        health_mode,
        health_wallet_address,
        expected_wallet_address,
        health_error,
        sign_ok,
        sign_ms,
        sign_error,
        test_batch_id: request.batch_id,
        test_outputs: request.outputs.len(),
        test_total_base_units: request.total_base_units,
        raw_tx_hex,
        raw_tx_hex_len,
        raw_tx_mock_prefix_present,
        node_tx,
        node_tx_present,
        node_tx_valid,
        node_tx_outputs_match_request,
        txid,
    })
}

async fn settle_rewards() -> Result<SettleRewardsRun> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let repo = PgRepository::connect(&database_url).await?;
    csd_pool_db::run_migrations(repo.pool()).await?;
    let fee_bps = pool_fee_bps();
    let blocks = repo.list_confirmed_unsettled_blocks(100).await?;
    let mut settlements = Vec::with_capacity(blocks.len());

    for block in blocks {
        let shares = repo.share_weights_for_job(&block.job_id).await?;
        match settle_reward_block(&block, fee_bps, &shares) {
            Ok(settlement) => {
                repo.append_ledger_entries(&settlement.ledger_entries)
                    .await?;
                settlements.push(SettleRewardOutcome::Settled { block: settlement });
            }
            Err(err) => settlements.push(SettleRewardOutcome::Skipped {
                block_hash: block.hash_hex,
                reason: err.to_string(),
            }),
        }
    }

    Ok(SettleRewardsRun {
        fee_bps,
        processed_count: settlements.len(),
        settlements,
    })
}

async fn mature_rewards() -> Result<MatureRewardsRun> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let repo = PgRepository::connect(&database_url).await?;
    csd_pool_db::run_migrations(repo.pool()).await?;
    let confirm_depth = confirm_depth();
    let ledger_entries = repo
        .list_mature_reward_entries(confirm_depth, 1_000)
        .await?;
    repo.append_ledger_entries(&ledger_entries).await?;
    let total_base_units = ledger_entries
        .iter()
        .filter(|entry| entry.amount_base_units > 0)
        .map(|entry| entry.amount_base_units as u128)
        .sum();
    Ok(MatureRewardsRun {
        confirm_depth,
        matured_count: ledger_entries.len(),
        total_base_units,
        ledger_entries,
    })
}

async fn reverse_orphans() -> Result<ReverseOrphansRun> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let repo = PgRepository::connect(&database_url).await?;
    csd_pool_db::run_migrations(repo.pool()).await?;
    let ledger_entries = repo.list_orphan_reversal_entries(1_000).await?;
    repo.append_ledger_entries(&ledger_entries).await?;
    let total_reversed_base_units = ledger_entries
        .iter()
        .filter(|entry| entry.amount_base_units < 0)
        .map(|entry| (-entry.amount_base_units) as u128)
        .sum();
    Ok(ReverseOrphansRun {
        reversed_count: ledger_entries.len(),
        total_reversed_base_units,
        ledger_entries,
    })
}

async fn payout_preview() -> Result<PayoutPreviewRun> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let repo = PgRepository::connect(&database_url).await?;
    csd_pool_db::run_migrations(repo.pool()).await?;
    let minimum_payout_base_units = minimum_payout_base_units()?;
    let balances = repo
        .list_payable_balances(minimum_payout_base_units, 1_000)
        .await?;
    let selection = select_payouts(&balances, minimum_payout_base_units, 100);
    let max_payout_batch_base_units = max_payout_batch_base_units()?;
    let max_daily_payout_base_units = max_daily_payout_base_units()?;
    let manual_payout_approval_base_units = manual_payout_approval_base_units()?;
    let daily_payout_used_base_units = repo.active_payout_total_today().await?;
    let daily_remaining_base_units =
        max_daily_payout_base_units.saturating_sub(daily_payout_used_base_units);
    let cap_exceeded = selection.total_base_units > max_payout_batch_base_units;
    let daily_cap_exceeded = daily_payout_used_base_units
        .saturating_add(selection.total_base_units)
        > max_daily_payout_base_units;
    let manual_approval_required = selection.total_base_units > manual_payout_approval_base_units;
    Ok(PayoutPreviewRun {
        payouts_enabled: repo.payouts_enabled().await?,
        minimum_payout_base_units,
        minimum_payout_csd: format_unsigned_csd(minimum_payout_base_units),
        max_payout_batch_base_units,
        max_payout_batch_csd: format_unsigned_csd(max_payout_batch_base_units),
        max_daily_payout_base_units,
        max_daily_payout_csd: format_unsigned_csd(max_daily_payout_base_units),
        manual_payout_approval_base_units,
        manual_payout_approval_csd: format_unsigned_csd(manual_payout_approval_base_units),
        daily_payout_used_base_units,
        daily_payout_used_csd: format_unsigned_csd(daily_payout_used_base_units),
        daily_remaining_base_units,
        daily_remaining_csd: format_unsigned_csd(daily_remaining_base_units),
        recipient_count: selection.recipients.len(),
        total_base_units: selection.total_base_units,
        total_csd: format_unsigned_csd(selection.total_base_units),
        would_create_batch: !selection.recipients.is_empty()
            && !cap_exceeded
            && !daily_cap_exceeded
            && !manual_approval_required,
        cap_exceeded,
        daily_cap_exceeded,
        manual_approval_required,
        recipients: selection.recipients,
    })
}

async fn create_payouts() -> Result<CreatePayoutsRun> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let repo = PgRepository::connect(&database_url).await?;
    csd_pool_db::run_migrations(repo.pool()).await?;
    if !repo.payouts_enabled().await? {
        return Ok(CreatePayoutsRun {
            minimum_payout_base_units: minimum_payout_base_units()?,
            max_payout_batch_base_units: max_payout_batch_base_units()?,
            max_daily_payout_base_units: max_daily_payout_base_units()?,
            manual_payout_approval_base_units: manual_payout_approval_base_units()?,
            daily_payout_used_base_units: repo.active_payout_total_today().await?,
            payouts_enabled: false,
            created: false,
            batch: None,
            selected_recipients: 0,
            total_base_units: 0,
            skipped_reason: Some("payouts paused".to_owned()),
        });
    }
    let minimum_payout_base_units = minimum_payout_base_units()?;
    let max_payout_batch_base_units = max_payout_batch_base_units()?;
    let max_daily_payout_base_units = max_daily_payout_base_units()?;
    let manual_payout_approval_base_units = manual_payout_approval_base_units()?;
    let daily_payout_used_base_units = repo.active_payout_total_today().await?;
    let balances = repo
        .list_payable_balances(minimum_payout_base_units, 1_000)
        .await?;
    let selection = select_payouts(&balances, minimum_payout_base_units, 100);
    if selection.recipients.is_empty() {
        return Ok(CreatePayoutsRun {
            minimum_payout_base_units,
            max_payout_batch_base_units,
            max_daily_payout_base_units,
            manual_payout_approval_base_units,
            daily_payout_used_base_units,
            payouts_enabled: true,
            created: false,
            batch: None,
            selected_recipients: 0,
            total_base_units: 0,
            skipped_reason: Some("no payable balances above threshold".to_owned()),
        });
    }
    if selection.total_base_units > max_payout_batch_base_units {
        return Ok(CreatePayoutsRun {
            minimum_payout_base_units,
            max_payout_batch_base_units,
            max_daily_payout_base_units,
            manual_payout_approval_base_units,
            daily_payout_used_base_units,
            payouts_enabled: true,
            created: false,
            batch: None,
            selected_recipients: selection.recipients.len(),
            total_base_units: selection.total_base_units,
            skipped_reason: Some("payout batch exceeds configured cap".to_owned()),
        });
    }
    if daily_payout_used_base_units.saturating_add(selection.total_base_units)
        > max_daily_payout_base_units
    {
        return Ok(CreatePayoutsRun {
            minimum_payout_base_units,
            max_payout_batch_base_units,
            max_daily_payout_base_units,
            manual_payout_approval_base_units,
            daily_payout_used_base_units,
            payouts_enabled: true,
            created: false,
            batch: None,
            selected_recipients: selection.recipients.len(),
            total_base_units: selection.total_base_units,
            skipped_reason: Some("payout batch exceeds daily cap".to_owned()),
        });
    }
    if selection.total_base_units > manual_payout_approval_base_units {
        let selected_recipients = selection.recipients.len();
        let total_base_units = selection.total_base_units;
        let batch_id = payout_batch_id();
        let draft = payout_batch_draft(batch_id, selection);
        let created = repo
            .create_locked_payout_batch_with_status(draft.clone(), "needs_approval")
            .await?;
        if created {
            append_payout_audit(
                &repo,
                &draft.batch_id,
                "worker:create-payouts",
                "needs_approval",
                serde_json::json!({
                    "total_base_units": total_base_units.to_string(),
                    "manual_payout_approval_base_units": manual_payout_approval_base_units.to_string()
                }),
            )
            .await?;
        }
        return Ok(CreatePayoutsRun {
            minimum_payout_base_units,
            max_payout_batch_base_units,
            max_daily_payout_base_units,
            manual_payout_approval_base_units,
            daily_payout_used_base_units,
            payouts_enabled: true,
            created,
            batch: created.then_some(draft),
            selected_recipients,
            total_base_units,
            skipped_reason: Some("manual payout approval required before signing".to_owned()),
        });
    }

    let batch_id = payout_batch_id();
    let draft = payout_batch_draft(batch_id, selection);
    let created = repo.create_locked_payout_batch(draft.clone()).await?;
    if created {
        append_payout_audit(
            &repo,
            &draft.batch_id,
            "worker:create-payouts",
            "create",
            serde_json::json!({
                "total_base_units": draft.total_base_units.to_string(),
                "recipient_count": draft.recipients.len()
            }),
        )
        .await?;
    }
    Ok(CreatePayoutsRun {
        minimum_payout_base_units,
        max_payout_batch_base_units,
        max_daily_payout_base_units,
        manual_payout_approval_base_units,
        daily_payout_used_base_units,
        payouts_enabled: true,
        created,
        selected_recipients: draft.recipients.len(),
        total_base_units: draft.total_base_units,
        batch: Some(draft),
        skipped_reason: None,
    })
}

async fn sign_payouts() -> Result<SignPayoutsRun> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let signer_url = signer_url()?.ok_or(WorkerError::MissingSignerUrl)?;
    let repo = PgRepository::connect(&database_url).await?;
    csd_pool_db::run_migrations(repo.pool()).await?;
    if !repo.payouts_enabled().await? {
        return Ok(SignPayoutsRun {
            payouts_enabled: false,
            blocked_by_inflight_batch: None,
            outcomes: vec![],
        });
    }

    // Keep the wallet's UTXO view serialized across timers, operators, and hosts.
    // The transaction-scoped lock is released automatically if this process exits.
    let mut signing_guard = repo.pool().begin().await?;
    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(0x4353_4450_4159_i64)
        .execute(&mut *signing_guard)
        .await?;
    if let Some(inflight) = repo
        .list_payout_batches_by_status(&["signed", "submitted"], 1)
        .await?
        .into_iter()
        .next()
    {
        signing_guard.commit().await?;
        return Ok(SignPayoutsRun {
            payouts_enabled: true,
            blocked_by_inflight_batch: Some(inflight.batch_id),
            outcomes: vec![],
        });
    }

    let signer = PayoutSignerClient::new(signer_url, signer_token()?);
    let batches = repo.list_payout_batches_by_status(&["created"], 1).await?;
    let mut outcomes = Vec::with_capacity(batches.len());

    for batch in batches {
        let request = SignPayoutRequest::from(&batch);
        match signer.sign_request(&request).await {
            Ok(signed) => {
                let storage_value = match validate_signed_payout_response(&signed, &request)
                    .and_then(|_| signed_payout_storage_value(&signed))
                {
                    Ok(value) => value,
                    Err(reason) => {
                        repo.mark_payout_failed(&batch.batch_id, &reason).await?;
                        repo.unlock_failed_payout(&batch).await?;
                        append_payout_audit(
                            &repo,
                            &batch.batch_id,
                            "worker:sign-payouts",
                            "fail",
                            serde_json::json!({ "reason": reason }),
                        )
                        .await?;
                        outcomes.push(SignPayoutOutcome {
                            batch_id: batch.batch_id,
                            status: "failed".to_owned(),
                            txid: None,
                            updated: true,
                            reason: Some(reason),
                        });
                        continue;
                    }
                };
                let updated = repo
                    .mark_payout_signed(&batch.batch_id, &signed.txid, &storage_value)
                    .await?;
                if updated {
                    append_payout_audit(
                        &repo,
                        &batch.batch_id,
                        "worker:sign-payouts",
                        "sign",
                        serde_json::json!({ "txid": signed.txid }),
                    )
                    .await?;
                }
                outcomes.push(SignPayoutOutcome {
                    batch_id: batch.batch_id,
                    status: "signed".to_owned(),
                    txid: Some(signed.txid),
                    updated,
                    reason: None,
                });
            }
            Err(err) => {
                let reason = err.to_string();
                repo.mark_payout_failed(&batch.batch_id, &reason).await?;
                repo.unlock_failed_payout(&batch).await?;
                append_payout_audit(
                    &repo,
                    &batch.batch_id,
                    "worker:sign-payouts",
                    "fail",
                    serde_json::json!({ "reason": reason }),
                )
                .await?;
                outcomes.push(SignPayoutOutcome {
                    batch_id: batch.batch_id,
                    status: "failed".to_owned(),
                    txid: None,
                    updated: true,
                    reason: Some(reason),
                });
            }
        }
    }

    signing_guard.commit().await?;
    Ok(SignPayoutsRun {
        payouts_enabled: true,
        blocked_by_inflight_batch: None,
        outcomes,
    })
}

async fn submit_payouts() -> Result<SubmitPayoutsRun> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let official_node_url = payout_node_url();
    let adapter_node_url = submit_node_url()?;
    if official_node_url.is_none() && adapter_node_url.is_none() {
        return Err(WorkerError::MissingNodeUrl);
    }
    let repo = PgRepository::connect(&database_url).await?;
    csd_pool_db::run_migrations(repo.pool()).await?;
    if !repo.payouts_enabled().await? {
        return Ok(SubmitPayoutsRun {
            payouts_enabled: false,
            outcomes: vec![],
        });
    }
    let official_node = official_node_url.map(CsdNodeClient::from_env);
    let adapter_node = adapter_node_url.map(CsdNodeClient::from_env);
    let batches = repo.list_payout_batches_by_status(&["signed"], 10).await?;
    let mut outcomes = Vec::with_capacity(batches.len());

    for batch in batches {
        let Some(stored_tx) = batch.raw_tx_hash.as_deref() else {
            repo.mark_payout_failed(&batch.batch_id, "missing signed transaction payload")
                .await?;
            repo.unlock_failed_payout(&batch).await?;
            append_payout_audit(
                &repo,
                &batch.batch_id,
                "worker:submit-payouts",
                "fail",
                serde_json::json!({ "reason": "missing signed transaction payload" }),
            )
            .await?;
            outcomes.push(SubmitPayoutOutcome {
                batch_id: batch.batch_id,
                status: "failed".to_owned(),
                txid: batch.txid,
                updated: true,
                reason: Some("missing signed transaction payload".to_owned()),
            });
            continue;
        };

        let submit_result: std::result::Result<_, String> =
            if let Some(node_tx_json) = stored_tx.strip_prefix(NODE_TX_STORAGE_PREFIX) {
                match serde_json::from_str::<serde_json::Value>(node_tx_json) {
                    Ok(node_tx) => {
                        if let Some(node) = official_node.as_ref() {
                            node.submit_official_transaction(&node_tx)
                                .await
                                .map_err(|err| err.to_string())
                        } else if let Some(node) = adapter_node.as_ref() {
                            node.submit_transaction(&node_tx)
                                .await
                                .map_err(|err| err.to_string())
                        } else {
                            Err("no payout transaction submit endpoint configured".to_owned())
                        }
                    }
                    Err(err) => {
                        let reason = format!("invalid stored node_tx JSON: {err}");
                        repo.mark_payout_failed(&batch.batch_id, &reason).await?;
                        repo.unlock_failed_payout(&batch).await?;
                        append_payout_audit(
                            &repo,
                            &batch.batch_id,
                            "worker:submit-payouts",
                            "fail",
                            serde_json::json!({ "reason": reason }),
                        )
                        .await?;
                        outcomes.push(SubmitPayoutOutcome {
                            batch_id: batch.batch_id,
                            status: "failed".to_owned(),
                            txid: batch.txid,
                            updated: true,
                            reason: Some(reason),
                        });
                        continue;
                    }
                }
            } else {
                if let Some(node) = adapter_node.as_ref() {
                    node.submit_raw_transaction(stored_tx)
                        .await
                        .map_err(|err| err.to_string())
                } else {
                    Err("legacy raw transaction requires the pool node adapter".to_owned())
                }
            };

        match submit_result {
            Ok(response) if response.ok => {
                let txid = response.txid.or(batch.txid).unwrap_or_default();
                let updated = repo.mark_payout_submitted(&batch.batch_id, &txid).await?;
                if updated {
                    append_payout_audit(
                        &repo,
                        &batch.batch_id,
                        "worker:submit-payouts",
                        "submit",
                        serde_json::json!({ "txid": txid }),
                    )
                    .await?;
                }
                outcomes.push(SubmitPayoutOutcome {
                    batch_id: batch.batch_id,
                    status: "submitted".to_owned(),
                    txid: Some(txid),
                    updated,
                    reason: None,
                });
            }
            Ok(response) if possible_prior_submit(&response, batch.txid.as_deref()) => {
                let txid = response.txid.or(batch.txid).unwrap_or_default();
                let reason = "node reports transaction already present or conflicting; reconciliation required";
                let updated = repo.mark_payout_submitted(&batch.batch_id, &txid).await?;
                if updated {
                    append_payout_audit(
                        &repo,
                        &batch.batch_id,
                        "worker:submit-payouts",
                        "submit_ambiguous",
                        serde_json::json!({ "txid": txid, "reason": reason }),
                    )
                    .await?;
                }
                outcomes.push(SubmitPayoutOutcome {
                    batch_id: batch.batch_id,
                    status: "submitted".to_owned(),
                    txid: Some(txid),
                    updated,
                    reason: Some(reason.to_owned()),
                });
            }
            Ok(response) => {
                let reason = format!("node rejected transaction: {:?}", response.extra);
                append_payout_audit(
                    &repo,
                    &batch.batch_id,
                    "worker:submit-payouts",
                    "submit_deferred",
                    serde_json::json!({ "reason": reason }),
                )
                .await?;
                outcomes.push(SubmitPayoutOutcome {
                    batch_id: batch.batch_id,
                    status: "signed".to_owned(),
                    txid: batch.txid,
                    updated: false,
                    reason: Some(reason),
                });
            }
            Err(err) => {
                let reason = format!("transaction submission uncertain: {err}");
                append_payout_audit(
                    &repo,
                    &batch.batch_id,
                    "worker:submit-payouts",
                    "submit_deferred",
                    serde_json::json!({ "reason": reason }),
                )
                .await?;
                outcomes.push(SubmitPayoutOutcome {
                    batch_id: batch.batch_id,
                    status: "signed".to_owned(),
                    txid: batch.txid,
                    updated: false,
                    reason: Some(reason),
                });
            }
        }
    }

    Ok(SubmitPayoutsRun {
        payouts_enabled: true,
        outcomes,
    })
}

async fn reconcile_payouts() -> Result<ReconcilePayoutsRun> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let node_url = watch_node_url()?.ok_or(WorkerError::MissingNodeUrl)?;
    let repo = PgRepository::connect(&database_url).await?;
    csd_pool_db::run_migrations(repo.pool()).await?;
    let node = CsdNodeClient::from_env(node_url);
    let batches = repo
        .list_payout_batches_by_status(&["submitted"], 100)
        .await?;
    let mut outcomes = Vec::with_capacity(batches.len());

    for batch in batches {
        let Some(txid) = batch.txid.as_deref() else {
            repo.mark_payout_failed(&batch.batch_id, "missing txid")
                .await?;
            repo.unlock_failed_payout(&batch).await?;
            append_payout_audit(
                &repo,
                &batch.batch_id,
                "worker:reconcile-payouts",
                "fail",
                serde_json::json!({ "reason": "missing txid" }),
            )
            .await?;
            outcomes.push(ReconcilePayoutOutcome {
                batch_id: batch.batch_id,
                status: "failed".to_owned(),
                confirmations: 0,
                updated: true,
                reason: Some("missing txid".to_owned()),
            });
            continue;
        };
        let status = node.transaction_status(txid).await?;
        match status.status.as_str() {
            "confirmed" if status.confirmations > 0 => {
                repo.mark_payout_confirmed(&batch.batch_id).await?;
                let paid_entries = repo.mark_payout_paid(&batch).await?;
                if paid_entries > 0 {
                    append_payout_audit(
                        &repo,
                        &batch.batch_id,
                        "worker:reconcile-payouts",
                        "confirm",
                        serde_json::json!({ "confirmations": status.confirmations }),
                    )
                    .await?;
                }
                outcomes.push(ReconcilePayoutOutcome {
                    batch_id: batch.batch_id,
                    status: "confirmed".to_owned(),
                    confirmations: status.confirmations,
                    updated: paid_entries > 0,
                    reason: None,
                });
            }
            "failed" | "rejected" => {
                let reason = format!("transaction status {}", status.status);
                repo.mark_payout_failed(&batch.batch_id, &reason).await?;
                repo.unlock_failed_payout(&batch).await?;
                append_payout_audit(
                    &repo,
                    &batch.batch_id,
                    "worker:reconcile-payouts",
                    "fail",
                    serde_json::json!({
                        "reason": reason,
                        "confirmations": status.confirmations
                    }),
                )
                .await?;
                outcomes.push(ReconcilePayoutOutcome {
                    batch_id: batch.batch_id,
                    status: "failed".to_owned(),
                    confirmations: status.confirmations,
                    updated: true,
                    reason: Some(reason),
                });
            }
            _ => outcomes.push(ReconcilePayoutOutcome {
                batch_id: batch.batch_id,
                status: status.status,
                confirmations: status.confirmations,
                updated: false,
                reason: None,
            }),
        }
    }

    Ok(ReconcilePayoutsRun { outcomes })
}

async fn sample_health() -> Result<SampleHealthRun> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let repo = PgRepository::connect(&database_url).await?;
    csd_pool_db::run_migrations(repo.pool()).await?;
    let mut samples = Vec::new();

    for node in configured_nodes() {
        let started = SystemTime::now();
        let client = CsdNodeClient::from_env(node.rpc_url);
        let health = client.health().await;
        let rpc_ms = elapsed_ms(started);
        let sample = match health {
            Ok(health) => NodeSampleRecord {
                node_name: format!("node:{}", node.name),
                height: health.height,
                chainwork: health.chainwork,
                peers: health.peers,
                mempool_size: None,
                rpc_ms: Some(rpc_ms),
                ok: true,
                sampled_at: None,
            },
            Err(_) => NodeSampleRecord {
                node_name: format!("node:{}", node.name),
                height: None,
                chainwork: None,
                peers: None,
                mempool_size: None,
                rpc_ms: Some(rpc_ms),
                ok: false,
                sampled_at: None,
            },
        };
        repo.insert_node_sample(&sample).await?;
        samples.push(sample);
    }

    if let Some(url) = signer_url()? {
        let started = SystemTime::now();
        let response = reqwest::Client::new()
            .get(format!("{}/health", url.trim_end_matches('/')))
            .send()
            .await;
        let rpc_ms = elapsed_ms(started);
        let sample = NodeSampleRecord {
            node_name: "signer".to_owned(),
            height: None,
            chainwork: None,
            peers: None,
            mempool_size: None,
            rpc_ms: Some(rpc_ms),
            ok: response
                .as_ref()
                .map(|response| response.status().is_success())
                .unwrap_or(false),
            sampled_at: None,
        };
        repo.insert_node_sample(&sample).await?;
        samples.push(sample);
    }

    Ok(SampleHealthRun { samples })
}

async fn check_alerts() -> Result<CheckAlertsRun> {
    let database_url = database_url()?.ok_or(WorkerError::MissingDatabaseUrl)?;
    let repo = PgRepository::connect(&database_url).await?;
    csd_pool_db::run_migrations(repo.pool()).await?;
    let mut active_fingerprints = Vec::new();

    for sample in repo.latest_node_samples(100).await? {
        let fingerprint = format!("health:{}", sample.node_name);
        if sample.ok {
            repo.resolve_alert(&fingerprint).await?;
        } else {
            let alert = AlertEvent {
                fingerprint: fingerprint.clone(),
                severity: "critical".to_owned(),
                status: "active".to_owned(),
                kind: "service_health".to_owned(),
                subject: sample.node_name.clone(),
                message: format!("{} health check is failing", sample.node_name),
                first_seen_at: None,
                last_seen_at: None,
                resolved_at: None,
                details: serde_json::json!({
                    "rpc_ms": sample.rpc_ms,
                    "sampled_at": sample.sampled_at,
                }),
            };
            repo.upsert_alert(&alert).await?;
            active_fingerprints.push(fingerprint);
        }
    }

    let no_share_fingerprint = "no_accepted_shares".to_owned();
    if let Some(gap) = repo.accepted_share_gap(no_accepted_share_minutes()).await? {
        let alert = AlertEvent {
            fingerprint: no_share_fingerprint.clone(),
            severity: "critical".to_owned(),
            status: "active".to_owned(),
            kind: "no_accepted_shares".to_owned(),
            subject: "pool".to_owned(),
            message: format!(
                "no accepted shares have been recorded for at least {} minutes",
                gap.quiet_minutes
            ),
            first_seen_at: None,
            last_seen_at: None,
            resolved_at: None,
            details: serde_json::json!({
                "quiet_minutes": gap.quiet_minutes,
                "latest_share_ts": gap.latest_share_ts,
                "latest_share_at": gap.latest_share_at,
            }),
        };
        repo.upsert_alert(&alert).await?;
        active_fingerprints.push(no_share_fingerprint);
    } else {
        repo.resolve_alert(&no_share_fingerprint).await?;
    }

    let template_age_fingerprint = "template_age".to_owned();
    let max_template_age_secs = max_template_age_secs();
    if let Some(job) = repo.latest_job().await? {
        if job.age_seconds > max_template_age_secs && !job_matches_any_node_tip(&job).await {
            let alert = template_age_alert(
                template_age_fingerprint.clone(),
                &job,
                max_template_age_secs,
            );
            repo.upsert_alert(&alert).await?;
            active_fingerprints.push(template_age_fingerprint);
        } else {
            repo.resolve_alert(&template_age_fingerprint).await?;
        }
    } else {
        repo.resolve_alert(&template_age_fingerprint).await?;
    }

    let block_submission_alerts = repo
        .list_block_submission_alerts(block_submission_stuck_minutes(), 100)
        .await?;
    let mut active_block_submission_fingerprints = HashSet::new();
    for block in block_submission_alerts {
        let fingerprint = block_submission_fingerprint(&block.hash_hex);
        active_block_submission_fingerprints.insert(fingerprint.clone());
        let alert = block_submission_alert(fingerprint.clone(), &block);
        repo.upsert_alert(&alert).await?;
        active_fingerprints.push(fingerprint);
    }

    for alert in repo.list_alerts(Some("active"), 1_000).await? {
        if alert.kind == "block_submission"
            && !active_block_submission_fingerprints.contains(&alert.fingerprint)
        {
            repo.resolve_alert(&alert.fingerprint).await?;
        }
    }

    let stuck_payouts = repo
        .list_stuck_payout_batches(stuck_payout_minutes(), 100)
        .await?;
    for batch in stuck_payouts {
        let fingerprint = format!("payout_stuck:{}", batch.batch_id);
        let alert = AlertEvent {
            fingerprint: fingerprint.clone(),
            severity: "warning".to_owned(),
            status: "active".to_owned(),
            kind: "payout_stuck".to_owned(),
            subject: batch.batch_id.clone(),
            message: format!(
                "payout batch {} is stuck in {}",
                batch.batch_id, batch.status
            ),
            first_seen_at: None,
            last_seen_at: None,
            resolved_at: None,
            details: serde_json::json!({
                "status": batch.status,
                "total_base_units": batch.total_base_units,
                "recipient_count": batch.recipients.len(),
            }),
        };
        repo.upsert_alert(&alert).await?;
        active_fingerprints.push(fingerprint);
    }

    let offline_minutes = worker_offline_minutes();
    let offline_workers = repo.list_offline_workers(offline_minutes, 1_000).await?;
    let excluded_worker_prefixes = worker_offline_excluded_prefixes();
    let mut active_worker_fingerprints = HashSet::new();
    for worker in offline_workers {
        if worker_offline_alert_excluded(&worker.worker_name, &excluded_worker_prefixes) {
            continue;
        }
        let fingerprint = worker_offline_fingerprint(&worker.miner, &worker.worker_name);
        active_worker_fingerprints.insert(fingerprint.clone());
        let alert = AlertEvent {
            fingerprint: fingerprint.clone(),
            severity: "warning".to_owned(),
            status: "active".to_owned(),
            kind: "worker_offline".to_owned(),
            subject: format!("{}.{}", worker.miner, worker.worker_name),
            message: format!(
                "worker {} for miner {} has been offline for at least {} minutes",
                worker.worker_name, worker.miner, offline_minutes
            ),
            first_seen_at: None,
            last_seen_at: None,
            resolved_at: None,
            details: serde_json::json!({
                "miner": worker.miner,
                "worker_name": worker.worker_name,
                "last_seen_ts": worker.last_seen_ts,
                "last_seen_at": worker.last_seen_at,
                "offline_minutes": offline_minutes,
            }),
        };
        repo.upsert_alert(&alert).await?;
        active_fingerprints.push(fingerprint);
    }

    for alert in repo.list_alerts(Some("active"), 1_000).await? {
        if alert.kind == "worker_offline"
            && !active_worker_fingerprints.contains(&alert.fingerprint)
        {
            repo.resolve_alert(&alert.fingerprint).await?;
        }
    }

    let quality_window_minutes = share_quality_window_minutes();
    let max_reject_rate = max_reject_rate();
    let max_stale_rate = max_stale_rate();
    let quality_alerts = repo
        .list_share_quality_alerts(
            quality_window_minutes,
            share_quality_min_total(),
            max_reject_rate,
            max_stale_rate,
            1_000,
        )
        .await?;
    let mut active_quality_fingerprints = HashSet::new();
    for quality in quality_alerts {
        if quality.reject_rate > max_reject_rate {
            let fingerprint =
                share_quality_fingerprint("high_reject_rate", &quality.miner, &quality.worker_name);
            active_quality_fingerprints.insert(fingerprint.clone());
            let alert = share_quality_alert(
                fingerprint.clone(),
                "high_reject_rate",
                "warning",
                &quality,
                quality.reject_rate,
                max_reject_rate,
            );
            repo.upsert_alert(&alert).await?;
            active_fingerprints.push(fingerprint);
        }
        if quality.stale_rate > max_stale_rate {
            let fingerprint =
                share_quality_fingerprint("high_stale_rate", &quality.miner, &quality.worker_name);
            active_quality_fingerprints.insert(fingerprint.clone());
            let alert = share_quality_alert(
                fingerprint.clone(),
                "high_stale_rate",
                "warning",
                &quality,
                quality.stale_rate,
                max_stale_rate,
            );
            repo.upsert_alert(&alert).await?;
            active_fingerprints.push(fingerprint);
        }
    }

    for alert in repo.list_alerts(Some("active"), 1_000).await? {
        if matches!(alert.kind.as_str(), "high_reject_rate" | "high_stale_rate")
            && !active_quality_fingerprints.contains(&alert.fingerprint)
        {
            repo.resolve_alert(&alert.fingerprint).await?;
        }
    }

    let alerts = repo.list_alerts(Some("active"), 100).await?;
    Ok(CheckAlertsRun {
        active_count: alerts.len(),
        active_fingerprints,
        alerts,
    })
}

fn settle_reward_block(
    block: &RewardBlock,
    fee_bps: u16,
    shares: &[ShareWeight],
) -> std::result::Result<SettledRewardBlock, csd_pool_accounting::AccountingError> {
    let result = allocate_pplns(block.reward_base_units, fee_bps, shares)?;
    let ledger_entries = reward_ledger_entries(&block.hash_hex, &result);
    Ok(SettledRewardBlock {
        block_hash: block.hash_hex.clone(),
        job_id: block.job_id.clone(),
        reward_base_units: block.reward_base_units,
        share_count: shares.len(),
        result,
        ledger_entries,
    })
}

fn pool_fee_bps() -> u16 {
    let fee_percent = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.pool.fee_percent)
            .unwrap_or(1.0)
    } else {
        1.0
    };
    (fee_percent.max(0.0) * 100.0).round().clamp(0.0, 10_000.0) as u16
}

fn confirm_depth() -> u64 {
    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.pool.confirm_depth)
            .unwrap_or(10)
    } else {
        10
    }
}

fn minimum_payout_base_units() -> Result<u128> {
    let value = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.pool.minimum_payout_csd)
            .unwrap_or_else(|| "1.0".to_owned())
    } else {
        std::env::var("CSD_POOL_MINIMUM_PAYOUT_CSD").unwrap_or_else(|_| "1.0".to_owned())
    };
    parse_csd_base_units(&value)
}

fn max_payout_batch_base_units() -> Result<u128> {
    let value = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.pool.max_payout_batch_csd)
            .unwrap_or_else(|| "1000.0".to_owned())
    } else {
        std::env::var("CSD_POOL_MAX_PAYOUT_BATCH_CSD").unwrap_or_else(|_| "1000.0".to_owned())
    };
    parse_csd_base_units(&value)
}

fn max_daily_payout_base_units() -> Result<u128> {
    let value = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.pool.max_daily_payout_csd)
            .unwrap_or_else(|| "5000.0".to_owned())
    } else {
        std::env::var("CSD_POOL_MAX_DAILY_PAYOUT_CSD").unwrap_or_else(|_| "5000.0".to_owned())
    };
    parse_csd_base_units(&value)
}

fn manual_payout_approval_base_units() -> Result<u128> {
    let value = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.pool.manual_payout_approval_csd)
            .unwrap_or_else(|| "250.0".to_owned())
    } else {
        std::env::var("CSD_POOL_MANUAL_PAYOUT_APPROVAL_CSD").unwrap_or_else(|_| "250.0".to_owned())
    };
    parse_csd_base_units(&value)
}

fn parse_csd_base_units(value: &str) -> Result<u128> {
    let trimmed = value.trim();
    let mut parts = trimmed.split('.');
    let whole = parts.next().unwrap_or_default();
    let frac = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
        || frac.len() > 8
    {
        return Err(WorkerError::InvalidAmount(trimmed.to_owned()));
    }
    let whole_units = whole
        .parse::<u128>()
        .map_err(|_| WorkerError::InvalidAmount(trimmed.to_owned()))?
        .checked_mul(100_000_000)
        .ok_or_else(|| WorkerError::InvalidAmount(trimmed.to_owned()))?;
    let mut frac_padded = frac.to_owned();
    while frac_padded.len() < 8 {
        frac_padded.push('0');
    }
    let frac_units = if frac_padded.is_empty() {
        0
    } else {
        frac_padded
            .parse::<u128>()
            .map_err(|_| WorkerError::InvalidAmount(trimmed.to_owned()))?
    };
    Ok(whole_units + frac_units)
}

fn config_check_path(path_arg: Option<&str>) -> PathBuf {
    path_arg
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var("CSD_POOL_CONFIG").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("config.example.toml"))
}

fn checked_amount(field: &str, value: &str, errors: &mut Vec<String>) -> Option<u128> {
    match parse_csd_base_units(value) {
        Ok(amount) => Some(amount),
        Err(error) => {
            errors.push(format!("{field}: {error}"));
            None
        }
    }
}

fn is_addr20_hex(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn node_roles(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|role| !role.is_empty())
        .map(str::to_owned)
        .collect()
}

fn config_env_checks(
    config: &csd_pool_config::PoolConfig,
    require_env: bool,
    errors: &mut Vec<String>,
) -> Vec<ConfigEnvCheck> {
    let mut names = Vec::new();
    push_unique_env(&mut names, &config.database.url_env);
    push_unique_env(&mut names, &config.redis.url_env);
    push_unique_env(&mut names, &config.signer.url_env);
    push_unique_env(&mut names, &config.signer.token_env);
    push_unique_env(&mut names, &config.api.operator_token_env);

    names
        .into_iter()
        .map(|name| {
            let value = std::env::var(&name).ok();
            let present = value.as_ref().is_some_and(|value| !value.trim().is_empty());
            let placeholder = value
                .as_deref()
                .map(has_placeholder_secret)
                .unwrap_or(false);
            if require_env && !present {
                errors.push(format!("required environment variable missing: {name}"));
            }
            if present && placeholder {
                errors.push(format!(
                    "environment variable contains placeholder value: {name}"
                ));
            }
            ConfigEnvCheck {
                name,
                present,
                length: value.as_ref().map(|value| value.len()).unwrap_or(0),
                placeholder,
            }
        })
        .collect()
}

fn push_unique_env(names: &mut Vec<String>, name: &str) {
    let trimmed = name.trim();
    if !trimmed.is_empty() && !names.iter().any(|existing| existing == trimmed) {
        names.push(trimmed.to_owned());
    }
}

fn has_placeholder_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "change-me",
        "dev-secret",
        "example-secret",
        "placeholder",
        "replace-me",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn payout_batch_id() -> String {
    let ts_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("payout-{ts_millis}")
}

async fn append_payout_audit(
    repo: &PgRepository,
    batch_id: &str,
    actor: &str,
    action: &str,
    details: serde_json::Value,
) -> Result<()> {
    repo.append_payout_audit_event(&PayoutAuditEvent {
        batch_id: batch_id.to_owned(),
        actor: actor.to_owned(),
        action: action.to_owned(),
        details,
        created_at: None,
    })
    .await?;
    Ok(())
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn configured_nodes() -> Vec<csd_pool_config::CsdNodeSection> {
    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        if let Ok(config) = csd_pool_config::PoolConfig::from_file(path) {
            return config.csd_nodes;
        }
    }
    vec![csd_pool_config::CsdNodeSection::default()]
}

fn elapsed_ms(started: SystemTime) -> f64 {
    SystemTime::now()
        .duration_since(started)
        .map(|duration| duration.as_secs_f64() * 1000.0)
        .unwrap_or_default()
}

fn stratum_smoke_config(endpoint_arg: Option<&str>) -> StratumSmokeConfig {
    StratumSmokeConfig {
        endpoint: endpoint_arg
            .filter(|endpoint| !endpoint.is_empty())
            .map(str::to_owned)
            .or_else(|| std::env::var("CSD_POOL_STRATUM_SMOKE_ADDR").ok())
            .filter(|endpoint| !endpoint.is_empty())
            .unwrap_or_else(|| "127.0.0.1:3333".to_owned()),
        clients: env_usize("CSD_POOL_SMOKE_CLIENTS", 10).clamp(1, 10_000),
        malformed: env_bool("CSD_POOL_SMOKE_MALFORMED"),
    }
}

fn stratum_load_test_config(endpoint_arg: Option<&str>) -> StratumLoadTestConfig {
    let endpoint = endpoint_arg
        .filter(|endpoint| !endpoint.is_empty())
        .map(str::to_owned)
        .or_else(|| std::env::var("CSD_POOL_STRATUM_LOAD_TEST_ADDR").ok())
        .or_else(|| std::env::var("CSD_POOL_STRATUM_SMOKE_ADDR").ok())
        .filter(|endpoint| !endpoint.is_empty())
        .unwrap_or_else(|| "127.0.0.1:3333".to_owned());
    let clients = env_usize("CSD_POOL_LOAD_TEST_CLIENTS", 100).clamp(1, 50_000);
    let min_success = env_usize("CSD_POOL_LOAD_TEST_MIN_SUCCESS", clients).clamp(1, clients);
    StratumLoadTestConfig {
        smoke: StratumSmokeConfig {
            endpoint,
            clients,
            malformed: env_bool("CSD_POOL_LOAD_TEST_MALFORMED"),
        },
        min_success,
    }
}

fn smoke_timeout_secs() -> u64 {
    env_usize("CSD_POOL_SMOKE_TIMEOUT_SECS", 5).clamp(1, 120) as u64
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_bool(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

fn elapsed_instant_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn smoke_worker_name(client_index: usize) -> String {
    let salt = client_index as u128 + 1;
    format!(
        "{:040x}",
        0x0123_4567_89ab_cdef_0123_4567_89ab_cdef_u128 + salt
    )
}

async fn write_with_timeout(writer: &mut OwnedWriteHalf, bytes: &[u8]) -> std::io::Result<()> {
    match timeout(
        Duration::from_secs(smoke_timeout_secs()),
        writer.write_all(bytes),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "write timed out",
        )),
    }
}

async fn read_response_with_timeout(reader: &mut BufReader<OwnedReadHalf>) -> Result<Response> {
    let value = read_json_with_timeout(reader).await?;
    Ok(serde_json::from_value(value)?)
}

async fn read_json_with_timeout(
    reader: &mut BufReader<OwnedReadHalf>,
) -> Result<serde_json::Value> {
    let mut line = String::new();
    let bytes_read = match timeout(
        Duration::from_secs(smoke_timeout_secs()),
        reader.read_line(&mut line),
    )
    .await
    {
        Ok(result) => result?,
        Err(_) => {
            return Err(WorkerError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "read timed out",
            )));
        }
    };
    if bytes_read == 0 {
        return Err(WorkerError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "connection closed",
        )));
    }
    Ok(serde_json::from_str(line.trim_end())?)
}

fn stuck_payout_minutes() -> i64 {
    std::env::var("CSD_POOL_STUCK_PAYOUT_MINUTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(60)
}

fn block_submission_stuck_minutes() -> i64 {
    std::env::var("CSD_POOL_BLOCK_SUBMISSION_STUCK_MINUTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10)
}

fn no_accepted_share_minutes() -> i64 {
    std::env::var("CSD_POOL_NO_ACCEPTED_SHARE_MINUTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10)
}

fn share_quality_window_minutes() -> i64 {
    std::env::var("CSD_POOL_SHARE_QUALITY_WINDOW_MINUTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(10)
}

fn share_quality_min_total() -> u64 {
    std::env::var("CSD_POOL_SHARE_QUALITY_MIN_TOTAL")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(50)
}

fn max_reject_rate() -> f64 {
    env_f64("CSD_POOL_MAX_REJECT_RATE", 0.05).clamp(0.0, 1.0)
}

fn max_stale_rate() -> f64 {
    env_f64("CSD_POOL_MAX_STALE_RATE", 0.02).clamp(0.0, 1.0)
}

fn max_template_age_secs() -> u64 {
    std::env::var("CSD_POOL_MAX_TEMPLATE_AGE_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(120)
}

async fn job_matches_any_node_tip(job: &csd_pool_db::LatestJobRecord) -> bool {
    for node in configured_nodes() {
        let client = CsdNodeClient::from_env(node.rpc_url);
        let Ok(health) = client.health().await else {
            continue;
        };
        let Some(tip) = health.tip.as_deref() else {
            continue;
        };
        if node_tip_matches_job_prev_hash(tip, &job.prev_hash) {
            return true;
        }
    }
    false
}

fn node_tip_matches_job_prev_hash(tip: &str, job_prev_hash: &str) -> bool {
    let tip = tip.strip_prefix("0x").unwrap_or(tip);
    let Ok(mut bytes) = hex::decode(tip) else {
        return false;
    };
    if bytes.len() != 32 {
        return false;
    }
    bytes.reverse();
    hex::encode(bytes).eq_ignore_ascii_case(job_prev_hash)
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value: &f64| value.is_finite())
        .unwrap_or(default)
}

fn worker_offline_minutes() -> i64 {
    std::env::var("CSD_POOL_WORKER_OFFLINE_MINUTES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(15)
}

fn worker_offline_excluded_prefixes() -> Vec<String> {
    std::env::var("CSD_POOL_WORKER_OFFLINE_EXCLUDED_PREFIXES")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
        .map(str::to_owned)
        .collect()
}

fn worker_offline_alert_excluded(worker_name: &str, prefixes: &[String]) -> bool {
    prefixes
        .iter()
        .any(|prefix| worker_name.starts_with(prefix))
}

fn worker_offline_fingerprint(miner: &str, worker_name: &str) -> String {
    format!("worker_offline:{miner}:{worker_name}")
}

fn share_quality_fingerprint(kind: &str, miner: &str, worker_name: &str) -> String {
    format!("{kind}:{miner}:{worker_name}")
}

fn block_submission_fingerprint(hash_hex: &str) -> String {
    format!("block_submission:{hash_hex}")
}

fn share_quality_alert(
    fingerprint: String,
    kind: &str,
    severity: &str,
    quality: &ShareQualityAlertRecord,
    rate: f64,
    threshold: f64,
) -> AlertEvent {
    AlertEvent {
        fingerprint,
        severity: severity.to_owned(),
        status: "active".to_owned(),
        kind: kind.to_owned(),
        subject: format!("{}.{}", quality.miner, quality.worker_name),
        message: format!(
            "{} for worker {} is {:.2}% over threshold {:.2}%",
            kind,
            quality.worker_name,
            rate * 100.0,
            threshold * 100.0
        ),
        first_seen_at: None,
        last_seen_at: None,
        resolved_at: None,
        details: serde_json::json!({
            "miner": &quality.miner,
            "worker_name": &quality.worker_name,
            "accepted_count": quality.accepted_count,
            "rejected_count": quality.rejected_count,
            "stale_count": quality.stale_count,
            "reject_rate": quality.reject_rate,
            "stale_rate": quality.stale_rate,
            "window_minutes": quality.window_minutes,
            "threshold": threshold,
        }),
    }
}

fn block_submission_alert(
    fingerprint: String,
    block: &csd_pool_db::BlockSubmissionAlertRecord,
) -> AlertEvent {
    AlertEvent {
        fingerprint,
        severity: "critical".to_owned(),
        status: "active".to_owned(),
        kind: "block_submission".to_owned(),
        subject: block.hash_hex.clone(),
        message: format!(
            "block candidate {} needs operator attention: {}",
            block.hash_hex, block.reason
        ),
        first_seen_at: None,
        last_seen_at: None,
        resolved_at: None,
        details: serde_json::json!({
            "hash_hex": block.hash_hex,
            "job_id": block.job_id,
            "status": block.status,
            "submitted_ts": block.submitted_ts,
            "submitted_at": block.submitted_at,
            "age_seconds": block.age_seconds,
            "submit_ok": block.submit_ok,
            "reason": block.reason,
        }),
    }
}

fn template_age_alert(
    fingerprint: String,
    job: &csd_pool_db::LatestJobRecord,
    threshold_seconds: u64,
) -> AlertEvent {
    AlertEvent {
        fingerprint,
        severity: "critical".to_owned(),
        status: "active".to_owned(),
        kind: "template_age".to_owned(),
        subject: job.job_id.clone(),
        message: format!(
            "latest mining job {} is {} seconds old",
            job.job_id, job.age_seconds
        ),
        first_seen_at: None,
        last_seen_at: None,
        resolved_at: None,
        details: serde_json::json!({
            "job_id": job.job_id,
            "prev_hash": job.prev_hash,
            "created_ts": job.created_ts,
            "created_at": job.created_at,
            "age_seconds": job.age_seconds,
            "threshold_seconds": threshold_seconds,
        }),
    }
}

fn block_update_from_status(
    hash_hex: &str,
    status: &BlockStatusResponse,
    required_confirmations: u64,
) -> BlockStatusUpdate {
    BlockStatusUpdate {
        hash_hex: hash_hex.to_owned(),
        status: normalize_block_status(
            &status.status,
            status.confirmations,
            required_confirmations,
        )
        .to_owned(),
        height: status.height,
        confirmations: status.confirmations,
        reward_base_units: status.reward_base_units.unwrap_or(0),
    }
}

fn normalize_block_status(status: &str, confirmations: u64, required_confirmations: u64) -> &str {
    match status {
        "orphan" | "orphaned" => "orphaned",
        _ if confirmations >= required_confirmations => "confirmed",
        _ if confirmations > 0 => "immature",
        "seen" | "seen_on_chain" => "seen_on_chain",
        _ => "submitted",
    }
}

async fn reward_dry_run() -> Result<RewardDryRun> {
    let shares = vec![
        ShareWeight {
            miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            difficulty: 10,
        },
        ShareWeight {
            miner: "89abcdef0123456789abcdef0123456789abcdef".to_owned(),
            difficulty: 30,
        },
    ];
    let result = allocate_pplns(5_000_000_000, 100, &shares)?;
    let ledger_entries = reward_ledger_entries("sample-block", &result);
    let (repository, persisted_ledger_entries) = match database_url()? {
        Some(url) => {
            let repo = PgRepository::connect(&url).await?;
            csd_pool_db::run_migrations(repo.pool()).await?;
            repo.append_ledger_entries(&ledger_entries).await?;
            ("postgres".to_owned(), repo.list_ledger_entries().await?)
        }
        None => {
            let repo = InMemoryRepository::new();
            repo.append_ledger_entries(&ledger_entries)?;
            ("memory".to_owned(), repo.list_ledger_entries()?)
        }
    };
    Ok(RewardDryRun {
        repository,
        shares,
        result,
        ledger_entries,
        persisted_ledger_entries,
    })
}

async fn payout_dry_run() -> Result<PayoutDryRun> {
    let balances = vec![
        MinerBalance {
            miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            address: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            confirmed_base_units: 250_000_000,
        },
        MinerBalance {
            miner: "89abcdef0123456789abcdef0123456789abcdef".to_owned(),
            address: "89abcdef0123456789abcdef0123456789abcdef".to_owned(),
            confirmed_base_units: 50_000_000,
        },
    ];
    let selection = select_payouts(&balances, 100_000_000, 100);
    let draft = payout_batch_draft("sample-batch", selection.clone());
    let (repository, persisted_balances, persisted_batches, persisted_ledger_entries) =
        match database_url()? {
            Some(url) => {
                let repo = PgRepository::connect(&url).await?;
                csd_pool_db::run_migrations(repo.pool()).await?;
                for balance in balances.iter().cloned() {
                    repo.set_balance(balance).await?;
                }
                repo.create_payout_batch(draft.clone()).await?;
                repo.append_ledger_entries(&draft.lock_entries).await?;
                (
                    "postgres".to_owned(),
                    repo.list_balances().await?,
                    repo.list_payout_batches().await?,
                    repo.list_ledger_entries().await?,
                )
            }
            None => {
                let repo = InMemoryRepository::new();
                for balance in balances.iter().cloned() {
                    repo.set_balance(balance)?;
                }
                repo.create_payout_batch(draft.clone())?;
                repo.append_ledger_entries(&draft.lock_entries)?;
                (
                    "memory".to_owned(),
                    repo.list_balances()?,
                    repo.list_payout_batches()?,
                    repo.list_ledger_entries()?,
                )
            }
        };
    Ok(PayoutDryRun {
        repository,
        minimum_payout_base_units: 100_000_000,
        balances,
        persisted_balances,
        selection,
        draft,
        persisted_batches,
        persisted_ledger_entries,
    })
}

fn database_url() -> Result<Option<String>> {
    let env_name = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.database.url_env)
            .unwrap_or_else(|| "CSD_POOL_DATABASE_URL".to_owned())
    } else {
        "CSD_POOL_DATABASE_URL".to_owned()
    };

    Ok(std::env::var(env_name)
        .ok()
        .filter(|value| !value.is_empty()))
}

fn backup_path(path_arg: Option<&str>) -> Result<PathBuf> {
    if let Some(path) = path_arg.filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    if let Ok(path) = std::env::var("CSD_POOL_BACKUP_PATH")
        && !path.is_empty()
    {
        return Ok(PathBuf::from(path));
    }
    let dir = std::env::var("CSD_POOL_BACKUP_DIR")
        .ok()
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| "backups".to_owned());
    Ok(PathBuf::from(dir).join(format!("csd_pool-{}.dump", now_ts())))
}

fn restore_path(path_arg: Option<&str>) -> Result<PathBuf> {
    if let Some(path) = path_arg.filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    if let Ok(path) = std::env::var("CSD_POOL_BACKUP_PATH")
        && !path.is_empty()
    {
        return Ok(PathBuf::from(path));
    }
    Err(WorkerError::MissingBackupPath)
}

fn backup_command_plan(path: &Path) -> CommandPlan {
    CommandPlan {
        display: format!(
            "pg_dump -Fc --no-owner --no-privileges -f {} <database-url>",
            path.display()
        ),
    }
}

fn restore_command_plan(path: &Path) -> CommandPlan {
    CommandPlan {
        display: format!(
            "pg_restore --clean --if-exists --no-owner --no-privileges --dbname <database-url> {}",
            path.display()
        ),
    }
}

fn ensure_command_success(command: &str, output: &std::process::Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    Err(WorkerError::ExternalCommandFailed {
        command: command.to_owned(),
        status: output
            .status
            .code()
            .map(|code| code.to_string())
            .unwrap_or_else(|| "signal".to_owned()),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

fn ledger_entries_csv(entries: &[LedgerEntry]) -> String {
    let mut csv =
        "entry_index,miner,amount_base_units,amount_csd,kind,ref_type,ref_id\n".to_owned();
    for (index, entry) in entries.iter().enumerate() {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{}\n",
            index + 1,
            csv_cell(entry.miner.as_deref().unwrap_or("pool")),
            entry.amount_base_units,
            format_signed_csd(entry.amount_base_units),
            csv_cell(entry.kind.as_str()),
            csv_cell(&entry.ref_type),
            csv_cell(&entry.ref_id)
        ));
    }
    csv
}

fn csv_cell(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn format_signed_csd(amount_base_units: i128) -> String {
    let sign = if amount_base_units < 0 { "-" } else { "" };
    let absolute = amount_base_units.unsigned_abs();
    format!(
        "{sign}{}.{:08}",
        absolute / 100_000_000,
        absolute % 100_000_000
    )
}

fn format_unsigned_csd(amount_base_units: u128) -> String {
    format!(
        "{}.{:08}",
        amount_base_units / 100_000_000,
        amount_base_units % 100_000_000
    )
}

fn watch_node_url() -> Result<Option<String>> {
    if let Ok(url) = std::env::var("CSD_POOL_WATCH_NODE_URL")
        && !url.is_empty()
    {
        return Ok(Some(url));
    }
    if let Ok(url) = std::env::var("CSD_POOL_NODE_URL")
        && !url.is_empty()
    {
        return Ok(Some(url));
    }
    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        let config = csd_pool_config::PoolConfig::from_file(path).ok();
        return Ok(config.and_then(|config| {
            config
                .csd_nodes
                .into_iter()
                .find(|node| node.role.split(',').any(|role| role.trim() == "watch"))
                .map(|node| node.rpc_url)
        }));
    }
    Ok(None)
}

fn template_node_url() -> Result<Option<String>> {
    if let Ok(url) = std::env::var("CSD_POOL_TEMPLATE_NODE_URL")
        && !url.is_empty()
    {
        return Ok(Some(url));
    }
    if let Ok(url) = std::env::var("CSD_POOL_NODE_URL")
        && !url.is_empty()
    {
        return Ok(Some(url));
    }
    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        let config = csd_pool_config::PoolConfig::from_file(path).ok();
        return Ok(config.and_then(|config| {
            config
                .csd_nodes
                .into_iter()
                .find(|node| role_includes(&node.role, "template"))
                .map(|node| node.rpc_url)
        }));
    }
    Ok(None)
}

fn submit_node_url() -> Result<Option<String>> {
    if let Ok(url) = std::env::var("CSD_POOL_SUBMIT_NODE_URL")
        && !url.is_empty()
    {
        return Ok(Some(url));
    }
    if let Ok(url) = std::env::var("CSD_POOL_NODE_URL")
        && !url.is_empty()
    {
        return Ok(Some(url));
    }
    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        let config = csd_pool_config::PoolConfig::from_file(path).ok();
        return Ok(config.and_then(|config| {
            config
                .csd_nodes
                .into_iter()
                .find(|node| node.role.split(',').any(|role| role.trim() == "submit"))
                .map(|node| node.rpc_url)
        }));
    }
    Ok(None)
}

fn payout_node_url() -> Option<String> {
    std::env::var("CSD_POOL_PAYOUT_NODE_URL")
        .ok()
        .filter(|url| !url.is_empty())
}

fn possible_prior_submit(response: &SubmitTxResponse, expected_txid: Option<&str>) -> bool {
    let Some(response_txid) = response.txid.as_deref() else {
        return false;
    };
    let Some(expected_txid) = expected_txid else {
        return false;
    };
    let txid_matches = response_txid
        .strip_prefix("0x")
        .unwrap_or(response_txid)
        .eq_ignore_ascii_case(expected_txid.strip_prefix("0x").unwrap_or(expected_txid));
    let already_present = response
        .extra
        .get("err")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|error| error.to_ascii_lowercase().contains("already present"));
    !response.ok && txid_matches && already_present
}

fn mining_address() -> Result<Option<String>> {
    if let Ok(address) = std::env::var("CSD_POOL_MINING_ADDRESS")
        && !address.is_empty()
    {
        return Ok(Some(address));
    }
    if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        return Ok(Some(
            csd_pool_config::PoolConfig::from_file(path)?
                .pool
                .mining_address,
        ));
    }
    Ok(None)
}

fn role_includes(value: &str, role: &str) -> bool {
    value
        .split(',')
        .any(|candidate| candidate.trim().eq_ignore_ascii_case(role))
}

fn node_template_summary(template: NodeMiningTemplate) -> NodeTemplateSummary {
    let job_id = template.job_id.clone();
    let clean_jobs = template.clean_jobs;
    let merkle_branches = template.merkle_branches_hex.len();
    let share_target_hex = template.share_target_hex.clone();
    let network_target_hex = template.network_target_hex.clone();

    match template.into_pool_job() {
        Ok(job) => NodeTemplateSummary {
            ok: true,
            error: None,
            job_id: Some(job_id),
            clean_jobs: Some(clean_jobs),
            merkle_branches,
            coinbase_prefix_bytes: job.template.coinbase_prefix.len(),
            coinbase_suffix_bytes: job.template.coinbase_suffix.len(),
            share_target_hex: Some(share_target_hex),
            network_target_hex: Some(network_target_hex),
        },
        Err(err) => NodeTemplateSummary {
            ok: false,
            error: Some(err.to_string()),
            job_id: Some(job_id),
            clean_jobs: Some(clean_jobs),
            merkle_branches,
            share_target_hex: Some(share_target_hex),
            network_target_hex: Some(network_target_hex),
            ..NodeTemplateSummary::default()
        },
    }
}

fn signer_url() -> Result<Option<String>> {
    let env_name = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.signer.url_env)
            .unwrap_or_else(|| "CSD_POOL_SIGNER_URL".to_owned())
    } else {
        "CSD_POOL_SIGNER_URL".to_owned()
    };
    Ok(std::env::var(env_name)
        .ok()
        .filter(|value| !value.is_empty()))
}

fn signer_token() -> Result<Option<String>> {
    let env_name = if let Ok(path) = std::env::var("CSD_POOL_CONFIG") {
        csd_pool_config::PoolConfig::from_file(path)
            .ok()
            .map(|config| config.signer.token_env)
            .unwrap_or_else(|| "CSD_POOL_SIGNER_TOKEN".to_owned())
    } else {
        "CSD_POOL_SIGNER_TOKEN".to_owned()
    };
    Ok(std::env::var(env_name)
        .ok()
        .filter(|value| !value.is_empty()))
}

fn signer_wallet_address() -> Option<String> {
    std::env::var("CSD_POOL_SIGNER_WALLET_ADDRESS")
        .ok()
        .and_then(|value| normalize_addr20(&value))
}

fn normalize_addr20(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let normalized = trimmed
        .strip_prefix("0x")
        .unwrap_or(trimmed)
        .to_ascii_lowercase();
    if is_addr20_hex(&normalized) {
        Some(normalized)
    } else {
        None
    }
}

#[derive(Clone)]
struct PayoutSignerClient {
    base_url: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl PayoutSignerClient {
    fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            token,
            http: reqwest::Client::new(),
        }
    }

    async fn sign_request(&self, payload: &SignPayoutRequest) -> Result<SignedPayoutResponse> {
        let url = format!("{}/api/payout/sign", self.base_url);
        let mut request = self.http.post(url).json(payload);
        if let Some(token) = self.token.as_deref() {
            request = request.bearer_auth(token);
        }
        Ok(request.send().await?.error_for_status()?.json().await?)
    }

    async fn health(&self) -> Result<SignerHealthResponse> {
        let url = format!("{}/health", self.base_url);
        Ok(self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
}

fn signer_contract_request(expected_wallet_address: Option<&str>) -> SignPayoutRequest {
    SignPayoutRequest {
        batch_id: format!("contract-check-{}", now_ts()),
        total_base_units: 546,
        outputs: vec![SignPayoutOutput {
            address: expected_wallet_address
                .unwrap_or("0123456789abcdef0123456789abcdef01234567")
                .to_owned(),
            amount_base_units: 546,
        }],
    }
}

fn validate_signed_payout_response(
    response: &SignedPayoutResponse,
    request: &SignPayoutRequest,
) -> std::result::Result<SignedPayoutValidation, String> {
    let txid = response.txid.strip_prefix("0x").unwrap_or(&response.txid);
    if txid.len() != 64 || !txid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("txid must be 64 hex chars".to_owned());
    }
    if let Some(node_tx) = response.node_tx.as_ref() {
        validate_official_node_tx(node_tx)?;
        let outputs_match_request = node_tx_outputs_match_request(node_tx, request);
        if !outputs_match_request {
            return Err("node_tx outputs do not match payout request".to_owned());
        }
        return Ok(SignedPayoutValidation {
            node_tx_present: true,
            node_tx_valid: true,
            node_tx_outputs_match_request: true,
        });
    }
    let raw_tx_hex = response.raw_tx_hex.as_deref().unwrap_or("");
    if raw_tx_hex.is_empty() {
        return Err("signed response must include node_tx or raw_tx_hex".to_owned());
    }
    if !raw_tx_hex.len().is_multiple_of(2)
        || !raw_tx_hex.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("raw_tx_hex must be even-length hex".to_owned());
    }
    Ok(SignedPayoutValidation::default())
}

#[derive(Clone, Copy, Debug, Default)]
struct SignedPayoutValidation {
    node_tx_present: bool,
    node_tx_valid: bool,
    node_tx_outputs_match_request: bool,
}

fn byte_array(value: Option<&serde_json::Value>, expected_len: usize) -> bool {
    value
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| {
            items.len() == expected_len
                && items
                    .iter()
                    .all(|item| item.as_u64().is_some_and(|byte| byte <= 255))
        })
}

fn validate_official_node_tx(node_tx: &serde_json::Value) -> std::result::Result<(), String> {
    let tx = node_tx
        .as_object()
        .ok_or_else(|| "node_tx must be an object".to_owned())?;
    if tx.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err("node_tx version must be 1".to_owned());
    }
    let inputs = tx
        .get("inputs")
        .and_then(serde_json::Value::as_array)
        .filter(|inputs| !inputs.is_empty() && inputs.len() <= 512)
        .ok_or_else(|| "node_tx inputs must contain 1..512 entries".to_owned())?;
    for input in inputs {
        let prevout = input
            .get("prevout")
            .ok_or_else(|| "node_tx input prevout is required".to_owned())?;
        if !byte_array(prevout.get("txid"), 32)
            || prevout
                .get("vout")
                .and_then(serde_json::Value::as_u64)
                .is_none_or(|vout| vout > u64::from(u32::MAX))
            || !byte_array(input.get("script_sig"), 99)
        {
            return Err(
                "node_tx input must have a 32-byte txid, u32 vout, and 99-byte script_sig"
                    .to_owned(),
            );
        }
    }
    let outputs = tx
        .get("outputs")
        .and_then(serde_json::Value::as_array)
        .filter(|outputs| !outputs.is_empty() && outputs.len() <= 512)
        .ok_or_else(|| "node_tx outputs must contain 1..512 entries".to_owned())?;
    for output in outputs {
        if output
            .get("value")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
            == 0
            || !byte_array(output.get("script_pubkey"), 20)
        {
            return Err(
                "node_tx output must have positive u64 value and 20-byte script_pubkey".to_owned(),
            );
        }
    }
    if tx
        .get("locktime")
        .and_then(serde_json::Value::as_u64)
        .is_none_or(|locktime| locktime > u64::from(u32::MAX))
        || tx.get("app").and_then(serde_json::Value::as_str) != Some("None")
    {
        return Err("node_tx must have u32 locktime and app=None".to_owned());
    }
    Ok(())
}

fn node_tx_outputs_match_request(node_tx: &serde_json::Value, request: &SignPayoutRequest) -> bool {
    let Some(outputs) = node_tx.get("outputs").and_then(serde_json::Value::as_array) else {
        return false;
    };
    let mut available = outputs
        .iter()
        .filter_map(|output| {
            let address = output
                .get("script_pubkey")?
                .as_array()?
                .iter()
                .map(|byte| byte.as_u64().and_then(|value| u8::try_from(value).ok()))
                .collect::<Option<Vec<_>>>()?;
            let value = output.get("value")?.as_u64()?;
            Some((hex::encode(address), u128::from(value)))
        })
        .collect::<Vec<_>>();
    request.outputs.iter().all(|expected| {
        available
            .iter()
            .position(|actual| {
                actual
                    .0
                    .eq_ignore_ascii_case(expected.address.trim_start_matches("0x"))
                    && actual.1 == expected.amount_base_units
            })
            .map(|index| available.swap_remove(index))
            .is_some()
    })
}

const NODE_TX_STORAGE_PREFIX: &str = "csd-node-json-v1:";

fn signed_payout_storage_value(
    response: &SignedPayoutResponse,
) -> std::result::Result<String, String> {
    if let Some(node_tx) = response.node_tx.as_ref() {
        return serde_json::to_string(node_tx)
            .map(|json| format!("{NODE_TX_STORAGE_PREFIX}{json}"))
            .map_err(|err| format!("failed to serialize node_tx: {err}"));
    }
    response
        .raw_tx_hex
        .clone()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "signed response has no transaction payload".to_owned())
}

fn signed_payout_has_mock_prefix(response: &SignedPayoutResponse) -> bool {
    hex::decode(response.raw_tx_hex.as_deref().unwrap_or(""))
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|raw| raw.starts_with("csd-payout-mock-v1:"))
        .unwrap_or(false)
}

#[derive(Serialize)]
struct MigrationRun {
    applied_versions: Vec<i64>,
    known_versions: Vec<i64>,
    database_versions: Vec<i64>,
    latest_known_version: i64,
    latest_database_version: i64,
    known_migration_count: usize,
    complete: bool,
}

#[derive(Serialize)]
struct DatabaseRuntimeCheckRun {
    passed: bool,
    failed_checks: usize,
    database_url_present: bool,
    database_name: String,
    database_user: String,
    server_version: String,
    connect_ms: f64,
    ping_ms: f64,
    identity_ms: f64,
    migrations_ms: f64,
    table_counts_ms: f64,
    transaction_ms: f64,
    max_query_ms: f64,
    max_transaction_ms: f64,
    max_observed_query_ms: f64,
    ping_ok: bool,
    known_versions: Vec<i64>,
    database_versions: Vec<i64>,
    latest_known_version: i64,
    latest_database_version: i64,
    migrations_complete: bool,
    latest_database_matches_known: bool,
    table_counts: Vec<DatabaseTableRuntimeCheck>,
    transaction_write_ok: bool,
    transaction_rollback_ok: bool,
    query_latency_ok: bool,
    transaction_latency_ok: bool,
}

#[derive(Serialize)]
struct DatabaseTableRuntimeCheck {
    table: String,
    row_count: i64,
}

#[derive(Serialize)]
struct BackupRun {
    path: String,
    command: String,
    size_bytes: u64,
}

#[derive(Serialize)]
struct RestoreRun {
    path: String,
    command: String,
}

#[derive(Serialize)]
struct AccountingExportRun {
    path: Option<String>,
    exported_entries: usize,
}

#[derive(Serialize)]
struct ConfigCheckRun {
    passed: bool,
    config_path: String,
    require_env: bool,
    pool_id: String,
    mining_address: String,
    fee_percent: f64,
    confirm_depth: u64,
    stratum_listen: String,
    api_listen: String,
    signer_listen: String,
    minimum_payout_base_units: Option<u128>,
    manual_payout_approval_base_units: Option<u128>,
    max_payout_batch_base_units: Option<u128>,
    max_daily_payout_base_units: Option<u128>,
    nodes: Vec<ConfigNodeSummary>,
    env: Vec<ConfigEnvCheck>,
    warnings: Vec<String>,
    errors: Vec<String>,
}

#[derive(Serialize)]
struct ConfigNodeSummary {
    name: String,
    rpc_url: String,
    roles: Vec<String>,
}

#[derive(Serialize)]
struct ConfigEnvCheck {
    name: String,
    present: bool,
    length: usize,
    placeholder: bool,
}

struct StratumSmokeConfig {
    endpoint: String,
    clients: usize,
    malformed: bool,
}

struct StratumLoadTestConfig {
    smoke: StratumSmokeConfig,
    min_success: usize,
}

#[derive(Clone, Copy)]
enum SubmitProbeMode {
    LowDifficulty,
    KnownAcceptedStatic,
}

impl SubmitProbeMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::LowDifficulty => "low-difficulty-response",
            Self::KnownAcceptedStatic => "known-accepted-static",
        }
    }
}

#[derive(Serialize)]
struct StratumSmokeRun {
    endpoint: String,
    requested_clients: usize,
    succeeded_clients: usize,
    failed_clients: usize,
    malformed_sent: bool,
    elapsed_ms: f64,
    min_client_ms: Option<f64>,
    avg_client_ms: Option<f64>,
    max_client_ms: Option<f64>,
    successes: Vec<StratumSmokeSuccess>,
    failures: Vec<StratumSmokeFailure>,
}

#[derive(Serialize)]
struct StratumSmokeSuccess {
    client_index: usize,
    worker: String,
    elapsed_ms: f64,
    extranonce1_hex: Option<String>,
    difficulty_seen: bool,
    notify_seen: bool,
}

#[derive(Serialize)]
struct StratumSmokeFailure {
    client_index: usize,
    worker: String,
    error: String,
}

#[derive(Serialize)]
struct StratumLoadTestRun {
    endpoint: String,
    requested_clients: usize,
    min_success_clients: usize,
    succeeded_clients: usize,
    failed_clients: usize,
    passed: bool,
    elapsed_ms: f64,
    connections_per_sec: f64,
    min_client_ms: Option<f64>,
    avg_client_ms: Option<f64>,
    max_client_ms: Option<f64>,
    failures: Vec<StratumSmokeFailure>,
}

struct StratumSmokeClientResult {
    client_index: usize,
    worker: String,
    ok: bool,
    elapsed_ms: f64,
    extranonce1_hex: Option<String>,
    difficulty_seen: bool,
    notify_seen: bool,
    error: Option<String>,
}

#[derive(Serialize)]
struct StratumSubmitProbeRun {
    endpoint: String,
    worker: String,
    mode: String,
    passed: bool,
    elapsed_ms: f64,
    extranonce1_hex: Option<String>,
    extranonce2_size: Option<usize>,
    difficulty_seen: bool,
    notify_seen: bool,
    difficulty: Option<f64>,
    job_id: Option<String>,
    submit_result: Option<bool>,
    submit_error_code: Option<i64>,
    submit_error_message: Option<String>,
    submit_response_received: bool,
    submit_response_standard: bool,
    error: Option<String>,
}

impl StratumSmokeClientResult {
    fn failed(mut self, started: Instant, error: String) -> Self {
        self.ok = false;
        self.elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.error = Some(error);
        self
    }
}

impl StratumSubmitProbeRun {
    fn failed(mut self, started: Instant, error: String) -> Self {
        self.passed = false;
        self.elapsed_ms = elapsed_instant_ms(started);
        self.error = Some(error);
        self
    }
}

struct CommandPlan {
    display: String,
}

#[derive(Serialize)]
struct NodeTemplateCheckRun {
    passed: bool,
    template_node_url: String,
    pool_address: String,
    adapter_auth_required: bool,
    adapter_auth_boundary_ok: bool,
    unauthenticated_template_status: Option<u16>,
    adapter_auth_error: Option<String>,
    health_ok: bool,
    health_ms: f64,
    health_error: Option<String>,
    network_ok: bool,
    network_ms: f64,
    network_hashrate_hs: Option<f64>,
    target_block_secs: Option<u64>,
    network_error: Option<String>,
    template_ok: bool,
    template_ms: f64,
    template_error: Option<String>,
    job_id: Option<String>,
    clean_jobs: Option<bool>,
    merkle_branches: usize,
    coinbase_prefix_bytes: usize,
    coinbase_suffix_bytes: usize,
    share_target_hex: Option<String>,
    network_target_hex: Option<String>,
    submit_node_url: Option<String>,
    submit_health: Option<NodeEndpointCheck>,
}

#[derive(Serialize)]
struct NodeCandidateCanaryRun {
    passed: bool,
    template_node_url: String,
    submit_node_url: String,
    pool_address: String,
    job_id: String,
    hash_hex: String,
    nonce: u32,
    attempted_hashes: u64,
    search_threads: usize,
    search_elapsed_ms: f64,
    search_hashrate_hs: f64,
    status: String,
    confirmations: u64,
    height: Option<u64>,
    reward_base_units: Option<u128>,
}

#[derive(Serialize)]
struct NodeEndpointCheck {
    node_url: String,
    ok: bool,
    latency_ms: f64,
    error: Option<String>,
}

#[derive(Serialize)]
struct NodeRuntimeCheckRun {
    passed: bool,
    failed_checks: Vec<String>,
    config_node_count: usize,
    configured_template_nodes: usize,
    configured_submit_nodes: usize,
    configured_watch_nodes: usize,
    healthy_template_nodes: usize,
    healthy_submit_nodes: usize,
    healthy_watch_nodes: usize,
    min_template_nodes: usize,
    min_submit_nodes: usize,
    min_watch_nodes: usize,
    max_health_ms: u64,
    max_network_ms: u64,
    max_template_ms: u64,
    role_quorum_ok: bool,
    health_quorum_ok: bool,
    network_ok: bool,
    template_contract_ok: bool,
    latency_ok: bool,
    nodes: Vec<NodeRuntimeNodeCheck>,
}

#[derive(Serialize)]
struct NodeRuntimeNodeCheck {
    name: String,
    rpc_url: String,
    role: String,
    health_ok: bool,
    health_ms: f64,
    health_error: Option<String>,
    network_ok: bool,
    network_ms: f64,
    network_hashrate_hs: Option<f64>,
    target_block_secs: Option<u64>,
    network_error: Option<String>,
    template_ok: bool,
    template_ms: Option<f64>,
    template_error: Option<String>,
    job_id: Option<String>,
    share_target_hex: Option<String>,
    network_target_hex: Option<String>,
}

impl NodeRuntimeNodeCheck {
    fn template_role(&self) -> bool {
        role_includes(&self.role, "template")
    }

    fn submit_role(&self) -> bool {
        role_includes(&self.role, "submit")
    }

    fn watch_role(&self) -> bool {
        role_includes(&self.role, "watch")
    }
}

#[derive(Default)]
struct NodeTemplateSummary {
    ok: bool,
    error: Option<String>,
    job_id: Option<String>,
    clean_jobs: Option<bool>,
    merkle_branches: usize,
    coinbase_prefix_bytes: usize,
    coinbase_suffix_bytes: usize,
    share_target_hex: Option<String>,
    network_target_hex: Option<String>,
}

#[derive(Serialize)]
struct SignerCheckRun {
    passed: bool,
    signer_url: String,
    health_ok: bool,
    health_ms: f64,
    health_service: Option<String>,
    health_mode: Option<String>,
    health_wallet_address: Option<String>,
    expected_wallet_address: Option<String>,
    health_error: Option<String>,
    sign_ok: bool,
    sign_ms: f64,
    sign_error: Option<String>,
    test_batch_id: String,
    test_outputs: usize,
    test_total_base_units: u128,
    raw_tx_hex: Option<String>,
    raw_tx_hex_len: usize,
    raw_tx_mock_prefix_present: bool,
    node_tx: Option<serde_json::Value>,
    node_tx_present: bool,
    node_tx_valid: bool,
    node_tx_outputs_match_request: bool,
    txid: Option<String>,
}

#[derive(Deserialize)]
struct SignerHealthResponse {
    #[serde(default)]
    service: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    wallet_address: Option<String>,
}

#[derive(Serialize)]
struct ReconcileBlocksRun {
    reconciled_count: usize,
    updates: Vec<ReconciledBlock>,
}

#[derive(Serialize)]
struct ReconciledBlock {
    hash_hex: String,
    status: String,
    height: Option<u64>,
    confirmations: u64,
    reward_base_units: u128,
    updated: bool,
}

#[derive(Serialize)]
struct SettleRewardsRun {
    fee_bps: u16,
    processed_count: usize,
    settlements: Vec<SettleRewardOutcome>,
}

#[derive(Serialize)]
struct MatureRewardsRun {
    confirm_depth: u64,
    matured_count: usize,
    total_base_units: u128,
    ledger_entries: Vec<LedgerEntry>,
}

#[derive(Serialize)]
struct ReverseOrphansRun {
    reversed_count: usize,
    total_reversed_base_units: u128,
    ledger_entries: Vec<LedgerEntry>,
}

#[derive(Serialize)]
struct PayoutPreviewRun {
    payouts_enabled: bool,
    minimum_payout_base_units: u128,
    minimum_payout_csd: String,
    max_payout_batch_base_units: u128,
    max_payout_batch_csd: String,
    max_daily_payout_base_units: u128,
    max_daily_payout_csd: String,
    manual_payout_approval_base_units: u128,
    manual_payout_approval_csd: String,
    daily_payout_used_base_units: u128,
    daily_payout_used_csd: String,
    daily_remaining_base_units: u128,
    daily_remaining_csd: String,
    recipient_count: usize,
    total_base_units: u128,
    total_csd: String,
    would_create_batch: bool,
    cap_exceeded: bool,
    daily_cap_exceeded: bool,
    manual_approval_required: bool,
    recipients: Vec<PayoutRecipient>,
}

#[derive(Serialize)]
struct CreatePayoutsRun {
    minimum_payout_base_units: u128,
    max_payout_batch_base_units: u128,
    max_daily_payout_base_units: u128,
    manual_payout_approval_base_units: u128,
    daily_payout_used_base_units: u128,
    payouts_enabled: bool,
    created: bool,
    selected_recipients: usize,
    total_base_units: u128,
    batch: Option<PayoutBatchDraft>,
    skipped_reason: Option<String>,
}

#[derive(Serialize)]
struct SignPayoutsRun {
    payouts_enabled: bool,
    blocked_by_inflight_batch: Option<String>,
    outcomes: Vec<SignPayoutOutcome>,
}

#[derive(Serialize)]
struct SignPayoutOutcome {
    batch_id: String,
    status: String,
    txid: Option<String>,
    updated: bool,
    reason: Option<String>,
}

#[derive(Serialize)]
struct SubmitPayoutsRun {
    payouts_enabled: bool,
    outcomes: Vec<SubmitPayoutOutcome>,
}

#[derive(Serialize)]
struct SubmitPayoutOutcome {
    batch_id: String,
    status: String,
    txid: Option<String>,
    updated: bool,
    reason: Option<String>,
}

#[derive(Serialize)]
struct ReconcilePayoutsRun {
    outcomes: Vec<ReconcilePayoutOutcome>,
}

#[derive(Serialize)]
struct SampleHealthRun {
    samples: Vec<NodeSampleRecord>,
}

#[derive(Serialize)]
struct CheckAlertsRun {
    active_count: usize,
    active_fingerprints: Vec<String>,
    alerts: Vec<AlertEvent>,
}

#[derive(Serialize)]
struct ReconcilePayoutOutcome {
    batch_id: String,
    status: String,
    confirmations: u64,
    updated: bool,
    reason: Option<String>,
}

#[derive(Serialize)]
struct SignPayoutRequest {
    batch_id: String,
    total_base_units: u128,
    outputs: Vec<SignPayoutOutput>,
}

impl From<&PayoutBatchRecord> for SignPayoutRequest {
    fn from(batch: &PayoutBatchRecord) -> Self {
        Self {
            batch_id: batch.batch_id.clone(),
            total_base_units: batch.total_base_units,
            outputs: batch
                .recipients
                .iter()
                .map(SignPayoutOutput::from)
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct SignPayoutOutput {
    address: String,
    amount_base_units: u128,
}

impl From<&PayoutRecipient> for SignPayoutOutput {
    fn from(recipient: &PayoutRecipient) -> Self {
        Self {
            address: recipient.address.clone(),
            amount_base_units: recipient.amount_base_units,
        }
    }
}

#[derive(Deserialize)]
struct SignedPayoutResponse {
    #[serde(default)]
    raw_tx_hex: Option<String>,
    #[serde(default)]
    node_tx: Option<serde_json::Value>,
    txid: String,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum SettleRewardOutcome {
    Settled { block: SettledRewardBlock },
    Skipped { block_hash: String, reason: String },
}

#[derive(Serialize)]
struct SettledRewardBlock {
    block_hash: String,
    job_id: String,
    reward_base_units: u128,
    share_count: usize,
    result: csd_pool_accounting::PplnsResult,
    ledger_entries: Vec<LedgerEntry>,
}

#[derive(Serialize)]
struct RewardDryRun {
    repository: String,
    shares: Vec<ShareWeight>,
    result: csd_pool_accounting::PplnsResult,
    ledger_entries: Vec<LedgerEntry>,
    persisted_ledger_entries: Vec<LedgerEntry>,
}

#[derive(Serialize)]
struct PayoutDryRun {
    repository: String,
    minimum_payout_base_units: u128,
    balances: Vec<MinerBalance>,
    persisted_balances: Vec<MinerBalance>,
    selection: csd_pool_accounting::PayoutSelection,
    draft: PayoutBatchDraft,
    persisted_batches: Vec<PayoutBatchDraft>,
    persisted_ledger_entries: Vec<LedgerEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reward_dry_run_preserves_totals() {
        let dry_run = reward_dry_run().await.unwrap();
        assert_eq!(dry_run.repository, "memory");
        assert_eq!(
            dry_run
                .result
                .allocations
                .iter()
                .map(|allocation| allocation.amount_base_units)
                .sum::<u128>(),
            dry_run.result.miner_total_base_units
        );
        assert_eq!(dry_run.ledger_entries.len(), 3);
        assert_eq!(dry_run.persisted_ledger_entries, dry_run.ledger_entries);
    }

    #[tokio::test]
    async fn payout_dry_run_selects_only_above_threshold() {
        let dry_run = payout_dry_run().await.unwrap();
        assert_eq!(dry_run.repository, "memory");
        assert_eq!(dry_run.selection.recipients.len(), 1);
        assert_eq!(dry_run.selection.total_base_units, 250_000_000);
        assert_eq!(dry_run.draft.lock_entries.len(), 1);
        assert_eq!(dry_run.persisted_batches.len(), 1);
        assert_eq!(dry_run.persisted_ledger_entries.len(), 1);
    }

    #[test]
    fn maps_adapter_block_status_to_update() {
        let update = block_update_from_status(
            "11",
            &BlockStatusResponse {
                hash: String::new(),
                status: "orphan".to_owned(),
                height: Some(42),
                confirmations: 0,
                reward_base_units: Some(5_000_000_000),
                extra: serde_json::json!({}),
            },
            10,
        );

        assert_eq!(update.hash_hex, "11");
        assert_eq!(update.status, "orphaned");
        assert_eq!(update.height, Some(42));
        assert_eq!(update.reward_base_units, 5_000_000_000);
    }

    #[test]
    fn normalizes_unknown_block_status_as_submitted() {
        assert_eq!(normalize_block_status("mystery", 0, 10), "submitted");
        assert_eq!(normalize_block_status("seen", 0, 10), "seen_on_chain");
        assert_eq!(normalize_block_status("confirmed", 3, 10), "immature");
        assert_eq!(normalize_block_status("submitted", 10, 10), "confirmed");
        assert_eq!(normalize_block_status("orphaned", 20, 10), "orphaned");
    }

    #[test]
    fn parallel_candidate_search_finds_easy_target() {
        let header = [0u8; 84];
        let (nonce, hash, attempts) =
            search_candidate_nonce(header, [0xff; 32], 10, 100, 4).unwrap();
        assert!((10..110).contains(&nonce));
        assert!(hash_leq_target(&hash, &[0xff; 32]));
        assert!((1..=100).contains(&attempts));
    }

    #[test]
    fn candidate_search_reports_exhausted_range() {
        assert!(search_candidate_nonce([0u8; 84], [0u8; 32], 0, 1, 2).is_none());
    }

    #[test]
    fn settles_reward_block_into_immutable_ledger_entries() {
        let block = RewardBlock {
            hash_hex: "11".repeat(32),
            job_id: "job-1".to_owned(),
            reward_base_units: 5_000_000_000,
        };
        let settled = settle_reward_block(
            &block,
            100,
            &[
                ShareWeight {
                    miner: "a".to_owned(),
                    difficulty: 1,
                },
                ShareWeight {
                    miner: "b".to_owned(),
                    difficulty: 3,
                },
            ],
        )
        .unwrap();

        assert_eq!(settled.result.fee_base_units, 50_000_000);
        assert_eq!(settled.ledger_entries.len(), 3);
        assert_eq!(settled.ledger_entries[0].ref_id, block.hash_hex);
    }

    #[test]
    fn default_confirm_depth_is_ten_without_config() {
        if std::env::var("CSD_POOL_CONFIG").is_err() {
            assert_eq!(confirm_depth(), 10);
        }
    }

    #[test]
    fn parses_csd_amounts_to_base_units() {
        assert_eq!(parse_csd_base_units("1").unwrap(), 100_000_000);
        assert_eq!(parse_csd_base_units("1.25").unwrap(), 125_000_000);
        assert_eq!(parse_csd_base_units("0.00000001").unwrap(), 1);
        assert!(parse_csd_base_units("1.000000001").is_err());
        assert!(parse_csd_base_units("bad").is_err());
    }

    #[test]
    fn formats_unsigned_csd_amounts_for_payout_preview() {
        assert_eq!(format_unsigned_csd(0), "0.00000000");
        assert_eq!(format_unsigned_csd(125_000_000), "1.25000000");
        assert_eq!(format_unsigned_csd(1), "0.00000001");
    }

    #[test]
    fn default_max_payout_batch_is_large_safety_cap() {
        if std::env::var("CSD_POOL_CONFIG").is_err()
            && std::env::var("CSD_POOL_MAX_PAYOUT_BATCH_CSD").is_err()
        {
            assert_eq!(max_payout_batch_base_units().unwrap(), 100_000_000_000);
        }
    }

    #[test]
    fn default_max_daily_payout_is_larger_safety_cap() {
        if std::env::var("CSD_POOL_CONFIG").is_err()
            && std::env::var("CSD_POOL_MAX_DAILY_PAYOUT_CSD").is_err()
        {
            assert_eq!(max_daily_payout_base_units().unwrap(), 500_000_000_000);
        }
    }

    #[test]
    fn default_manual_payout_approval_threshold_is_conservative() {
        if std::env::var("CSD_POOL_CONFIG").is_err()
            && std::env::var("CSD_POOL_MANUAL_PAYOUT_APPROVAL_CSD").is_err()
        {
            assert_eq!(manual_payout_approval_base_units().unwrap(), 25_000_000_000);
        }
    }

    #[test]
    fn signer_request_preserves_exact_outputs() {
        let batch = PayoutBatchRecord {
            batch_id: "batch-1".to_owned(),
            status: "created".to_owned(),
            total_base_units: 125,
            txid: None,
            raw_tx_hash: None,
            recipients: vec![PayoutRecipient {
                miner: "a".to_owned(),
                address: "addr-a".to_owned(),
                amount_base_units: 125,
            }],
        };
        let request = SignPayoutRequest::from(&batch);
        assert_eq!(request.batch_id, "batch-1");
        assert_eq!(request.outputs[0].amount_base_units, 125);
    }

    #[test]
    fn signer_contract_request_is_safe_and_exact() {
        let wallet = "abcdef0123456789abcdef0123456789abcdef01";
        let request = signer_contract_request(Some(wallet));
        assert!(request.batch_id.starts_with("contract-check-"));
        assert_eq!(request.total_base_units, 546);
        assert_eq!(request.outputs.len(), 1);
        assert_eq!(request.outputs[0].address, wallet);
        assert_eq!(request.outputs[0].amount_base_units, 546);
    }

    fn official_node_tx(address: &str, value: u64) -> serde_json::Value {
        serde_json::json!({
            "version": 1,
            "inputs": [{
                "prevout": { "txid": vec![17u8; 32], "vout": 0 },
                "script_sig": vec![34u8; 99]
            }],
            "outputs": [{
                "value": value,
                "script_pubkey": hex::decode(address).unwrap()
            }],
            "locktime": 0,
            "app": "None"
        })
    }

    #[test]
    fn validates_signed_payout_response_shape() {
        let request = signer_contract_request(Some("abcdef0123456789abcdef0123456789abcdef01"));
        assert!(
            validate_signed_payout_response(
                &SignedPayoutResponse {
                    raw_tx_hex: Some("abcd".to_owned()),
                    node_tx: None,
                    txid: "12".repeat(32),
                },
                &request
            )
            .is_ok()
        );
        assert_eq!(
            validate_signed_payout_response(
                &SignedPayoutResponse {
                    raw_tx_hex: None,
                    node_tx: None,
                    txid: "12".repeat(32),
                },
                &request
            )
            .unwrap_err(),
            "signed response must include node_tx or raw_tx_hex"
        );
        assert_eq!(
            validate_signed_payout_response(
                &SignedPayoutResponse {
                    raw_tx_hex: Some("abc".to_owned()),
                    node_tx: None,
                    txid: "12".repeat(32),
                },
                &request
            )
            .unwrap_err(),
            "raw_tx_hex must be even-length hex"
        );
        assert_eq!(
            validate_signed_payout_response(
                &SignedPayoutResponse {
                    raw_tx_hex: Some("abcd".to_owned()),
                    node_tx: None,
                    txid: "12".repeat(31),
                },
                &request
            )
            .unwrap_err(),
            "txid must be 64 hex chars"
        );

        let signed = SignedPayoutResponse {
            raw_tx_hex: None,
            node_tx: Some(official_node_tx(
                "abcdef0123456789abcdef0123456789abcdef01",
                546,
            )),
            txid: format!("0x{}", "12".repeat(32)),
        };
        let validation = validate_signed_payout_response(&signed, &request).unwrap();
        assert!(validation.node_tx_present);
        assert!(validation.node_tx_valid);
        assert!(validation.node_tx_outputs_match_request);
        assert!(
            signed_payout_storage_value(&signed)
                .unwrap()
                .starts_with(NODE_TX_STORAGE_PREFIX)
        );

        let mismatched = SignedPayoutResponse {
            node_tx: Some(official_node_tx(
                "abcdef0123456789abcdef0123456789abcdef01",
                547,
            )),
            ..signed
        };
        assert_eq!(
            validate_signed_payout_response(&mismatched, &request).unwrap_err(),
            "node_tx outputs do not match payout request"
        );
    }

    #[test]
    fn detects_mock_signer_raw_transaction_prefix() {
        let raw_tx_hex = hex::encode("csd-payout-mock-v1:{\"batch_id\":\"contract-check\"}");
        assert!(signed_payout_has_mock_prefix(&SignedPayoutResponse {
            raw_tx_hex: Some(raw_tx_hex),
            node_tx: None,
            txid: "12".repeat(32),
        }));
        assert!(!signed_payout_has_mock_prefix(&SignedPayoutResponse {
            raw_tx_hex: Some("abcd".to_owned()),
            node_tx: None,
            txid: "12".repeat(32),
        }));
    }

    #[test]
    fn signer_check_run_serializes_contract_fields() {
        let run = SignerCheckRun {
            passed: true,
            signer_url: "http://127.0.0.1:8890".to_owned(),
            health_ok: true,
            health_ms: 1.0,
            health_service: Some("csd-pool-signer".to_owned()),
            health_mode: Some("mock".to_owned()),
            health_wallet_address: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            expected_wallet_address: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
            health_error: None,
            sign_ok: true,
            sign_ms: 2.0,
            sign_error: None,
            test_batch_id: "contract-check-1".to_owned(),
            test_outputs: 1,
            test_total_base_units: 1,
            raw_tx_hex: Some("abcd".to_owned()),
            raw_tx_hex_len: 16,
            raw_tx_mock_prefix_present: false,
            node_tx: Some(official_node_tx(
                "0123456789abcdef0123456789abcdef01234567",
                1,
            )),
            node_tx_present: true,
            node_tx_valid: true,
            node_tx_outputs_match_request: true,
            txid: Some("12".repeat(32)),
        };

        let json = serde_json::to_value(run).unwrap();
        assert_eq!(json["passed"], true);
        assert_eq!(json["health_service"], "csd-pool-signer");
        assert_eq!(
            json["health_wallet_address"],
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(
            json["expected_wallet_address"],
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(json["test_outputs"], 1);
        assert_eq!(json["raw_tx_hex"], "abcd");
        assert_eq!(json["raw_tx_hex_len"], 16);
        assert_eq!(json["raw_tx_mock_prefix_present"], false);
        assert_eq!(json["node_tx_present"], true);
        assert_eq!(json["node_tx_valid"], true);
        assert_eq!(json["node_tx_outputs_match_request"], true);
    }

    #[test]
    fn worker_offline_fingerprint_is_stable() {
        assert_eq!(
            worker_offline_fingerprint("0123456789abcdef0123456789abcdef01234567", "rig-a"),
            "worker_offline:0123456789abcdef0123456789abcdef01234567:rig-a"
        );
    }

    #[test]
    fn worker_offline_exclusion_only_matches_configured_canary_prefixes() {
        let prefixes = vec!["canary-".to_owned(), "probe-".to_owned()];
        assert!(worker_offline_alert_excluded("canary-probe", &prefixes));
        assert!(worker_offline_alert_excluded("probe-v100", &prefixes));
        assert!(!worker_offline_alert_excluded(
            "V100-JN-20260719-01-CSD",
            &prefixes
        ));
        assert!(!worker_offline_alert_excluded("canary", &prefixes));
    }

    #[test]
    fn share_quality_alert_contains_rates_and_counts() {
        let quality = ShareQualityAlertRecord {
            miner: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            worker_name: "rig-a".to_owned(),
            accepted_count: 80,
            rejected_count: 20,
            stale_count: 0,
            reject_rate: 0.2,
            stale_rate: 0.0,
            window_minutes: 10,
        };
        let fingerprint = share_quality_fingerprint("high_reject_rate", &quality.miner, "rig-a");
        assert_eq!(
            fingerprint,
            "high_reject_rate:0123456789abcdef0123456789abcdef01234567:rig-a"
        );
        let alert = share_quality_alert(
            fingerprint,
            "high_reject_rate",
            "warning",
            &quality,
            quality.reject_rate,
            0.05,
        );
        assert_eq!(alert.kind, "high_reject_rate");
        assert_eq!(alert.details["rejected_count"], 20);
        assert_eq!(alert.details["threshold"], 0.05);
    }

    #[test]
    fn template_age_alert_contains_job_age_and_threshold() {
        let job = csd_pool_db::LatestJobRecord {
            job_id: "job-old".to_owned(),
            prev_hash: "11".repeat(32),
            created_ts: 100,
            created_at: Some("2026-06-16 01:00:00+00".to_owned()),
            age_seconds: 180,
        };
        let alert = template_age_alert("template_age".to_owned(), &job, 120);

        assert_eq!(alert.kind, "template_age");
        assert_eq!(alert.severity, "critical");
        assert_eq!(alert.subject, "job-old");
        assert_eq!(alert.details["age_seconds"], 180);
        assert_eq!(alert.details["threshold_seconds"], 120);
        assert_eq!(alert.details["prev_hash"], "11".repeat(32));
    }

    #[test]
    fn node_tip_matching_handles_stratum_byte_order() {
        let node_tip = format!("0x{}", "01".repeat(32));
        assert!(node_tip_matches_job_prev_hash(&node_tip, &"01".repeat(32)));

        let mut distinct_tip = [0_u8; 32];
        distinct_tip[0] = 0x12;
        distinct_tip[31] = 0x34;
        let mut expected_prev_hash = distinct_tip;
        expected_prev_hash.reverse();
        assert!(node_tip_matches_job_prev_hash(
            &format!("0x{}", hex::encode(distinct_tip)),
            &hex::encode(expected_prev_hash)
        ));
        assert!(!node_tip_matches_job_prev_hash("not-hex", &"00".repeat(32)));
    }

    #[test]
    fn block_submission_alert_contains_submission_failure_details() {
        let block = csd_pool_db::BlockSubmissionAlertRecord {
            hash_hex: "33".repeat(32),
            job_id: "job-failed".to_owned(),
            status: "submitted".to_owned(),
            submitted_ts: 1781580000,
            submitted_at: Some("2026-06-16 01:00:00+00".to_owned()),
            age_seconds: 600,
            submit_ok: Some(false),
            reason: "submit_response_not_ok".to_owned(),
        };
        let fingerprint = block_submission_fingerprint(&block.hash_hex);
        let alert = block_submission_alert(fingerprint.clone(), &block);

        assert_eq!(fingerprint, format!("block_submission:{}", "33".repeat(32)));
        assert_eq!(alert.kind, "block_submission");
        assert_eq!(alert.severity, "critical");
        assert_eq!(alert.details["reason"], "submit_response_not_ok");
        assert_eq!(alert.details["submit_ok"], false);
    }

    #[test]
    fn backup_and_restore_command_plans_redact_database_url() {
        let backup = backup_command_plan(Path::new("backups/test.dump"));
        assert_eq!(
            backup.display,
            "pg_dump -Fc --no-owner --no-privileges -f backups/test.dump <database-url>"
        );
        assert!(!backup.display.contains("postgres://"));

        let restore = restore_command_plan(Path::new("backups/test.dump"));
        assert_eq!(
            restore.display,
            "pg_restore --clean --if-exists --no-owner --no-privileges --dbname <database-url> backups/test.dump"
        );
        assert!(!restore.display.contains("postgres://"));
    }

    #[test]
    fn backup_and_restore_paths_accept_explicit_arguments() {
        assert_eq!(
            backup_path(Some("backups/test.dump")).unwrap(),
            PathBuf::from("backups/test.dump")
        );
        assert_eq!(
            restore_path(Some("backups/test.dump")).unwrap(),
            PathBuf::from("backups/test.dump")
        );
    }

    #[test]
    fn smoke_worker_names_use_authorizable_addr20_format() {
        let worker = smoke_worker_name(0);
        assert_eq!(worker.len(), 40);
        assert!(worker.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(!worker.contains('.'));

        assert_ne!(worker, smoke_worker_name(1));
    }

    #[test]
    fn smoke_run_serializes_failure_summary() {
        let run = StratumSmokeRun {
            endpoint: "127.0.0.1:3333".to_owned(),
            requested_clients: 2,
            succeeded_clients: 1,
            failed_clients: 1,
            malformed_sent: true,
            elapsed_ms: 12.5,
            min_client_ms: Some(5.0),
            avg_client_ms: Some(5.0),
            max_client_ms: Some(5.0),
            successes: vec![StratumSmokeSuccess {
                client_index: 0,
                worker: smoke_worker_name(0),
                elapsed_ms: 5.0,
                extranonce1_hex: Some("00000001".to_owned()),
                difficulty_seen: true,
                notify_seen: true,
            }],
            failures: vec![StratumSmokeFailure {
                client_index: 1,
                worker: smoke_worker_name(1),
                error: "connect failed".to_owned(),
            }],
        };

        let json = serde_json::to_value(run).unwrap();
        assert_eq!(json["requested_clients"], 2);
        assert_eq!(json["malformed_sent"], true);
        assert_eq!(json["successes"][0]["worker"], smoke_worker_name(0));
        assert_eq!(json["successes"][0]["difficulty_seen"], true);
        assert_eq!(json["successes"][0]["notify_seen"], true);
        assert_eq!(json["failures"][0]["worker"], smoke_worker_name(1));
    }

    #[test]
    fn role_includes_matches_comma_separated_roles_case_insensitively() {
        assert!(role_includes("template, submit,watch", "submit"));
        assert!(role_includes("Template", "template"));
        assert!(!role_includes("watcher", "watch"));
    }

    #[test]
    fn recognizes_only_matching_already_present_submit_responses() {
        let txid = "12".repeat(32);
        let prior = SubmitTxResponse {
            ok: false,
            txid: Some(format!("0x{txid}")),
            extra: serde_json::json!({ "err": "already present or mempool conflict" }),
        };
        assert!(possible_prior_submit(&prior, Some(&txid)));

        let rejected = SubmitTxResponse {
            ok: false,
            txid: Some(txid.clone()),
            extra: serde_json::json!({ "err": "signature verification failed" }),
        };
        assert!(!possible_prior_submit(&rejected, Some(&txid)));
        assert!(!possible_prior_submit(&prior, Some(&"34".repeat(32))));
        assert!(!possible_prior_submit(&prior, None));
    }

    #[test]
    fn node_template_summary_accepts_valid_template() {
        let summary = node_template_summary(NodeMiningTemplate {
            job_id: "job-live-1".to_owned(),
            prev_hash_be_hex: "00".repeat(32),
            coinb1_hex: "aa".to_owned(),
            coinb2_hex: "bbcc".to_owned(),
            merkle_branches_hex: vec!["11".repeat(32)],
            version_hex: "20000000".to_owned(),
            nbits_hex: "207fffff".to_owned(),
            ntime_hex: "665544cc".to_owned(),
            clean_jobs: true,
            share_target_hex: "ff".repeat(32),
            network_target_hex: "01".repeat(32),
        });

        assert!(summary.ok);
        assert_eq!(summary.job_id.as_deref(), Some("job-live-1"));
        assert_eq!(summary.merkle_branches, 1);
        assert_eq!(summary.coinbase_prefix_bytes, 1);
        assert_eq!(summary.coinbase_suffix_bytes, 2);
        assert!(summary.error.is_none());
    }

    #[test]
    fn node_template_summary_reports_invalid_template() {
        let summary = node_template_summary(NodeMiningTemplate {
            job_id: "bad-template".to_owned(),
            prev_hash_be_hex: "not-hex".to_owned(),
            coinb1_hex: "aa".to_owned(),
            coinb2_hex: "bb".to_owned(),
            merkle_branches_hex: Vec::new(),
            version_hex: "20000000".to_owned(),
            nbits_hex: "207fffff".to_owned(),
            ntime_hex: "665544cc".to_owned(),
            clean_jobs: true,
            share_target_hex: "ff".repeat(32),
            network_target_hex: "01".repeat(32),
        });

        assert!(!summary.ok);
        assert_eq!(summary.job_id.as_deref(), Some("bad-template"));
        assert!(summary.error.unwrap().contains("prev_hash"));
    }

    #[test]
    fn node_template_check_run_serializes_contract_fields() {
        let run = NodeTemplateCheckRun {
            passed: true,
            template_node_url: "http://127.0.0.1:8790".to_owned(),
            pool_address: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            adapter_auth_required: true,
            adapter_auth_boundary_ok: true,
            unauthenticated_template_status: Some(401),
            adapter_auth_error: None,
            health_ok: true,
            health_ms: 1.0,
            health_error: None,
            network_ok: true,
            network_ms: 2.0,
            network_hashrate_hs: Some(1_000.0),
            target_block_secs: Some(60),
            network_error: None,
            template_ok: true,
            template_ms: 3.0,
            template_error: None,
            job_id: Some("job-live-1".to_owned()),
            clean_jobs: Some(true),
            merkle_branches: 0,
            coinbase_prefix_bytes: 1,
            coinbase_suffix_bytes: 1,
            share_target_hex: Some("ff".repeat(32)),
            network_target_hex: Some("01".repeat(32)),
            submit_node_url: Some("http://127.0.0.1:8791".to_owned()),
            submit_health: Some(NodeEndpointCheck {
                node_url: "http://127.0.0.1:8791".to_owned(),
                ok: true,
                latency_ms: 4.0,
                error: None,
            }),
        };

        let json = serde_json::to_value(run).unwrap();
        assert_eq!(json["passed"], true);
        assert_eq!(json["template_ok"], true);
        assert_eq!(json["adapter_auth_boundary_ok"], true);
        assert_eq!(json["job_id"], "job-live-1");
        assert_eq!(json["submit_health"]["ok"], true);
    }

    #[test]
    fn load_test_run_passes_only_when_all_clients_succeed() {
        let run = stratum_load_test_run_from_smoke(
            StratumSmokeRun {
                endpoint: "127.0.0.1:3333".to_owned(),
                requested_clients: 100,
                succeeded_clients: 100,
                failed_clients: 0,
                malformed_sent: false,
                elapsed_ms: 2_000.0,
                min_client_ms: Some(10.0),
                avg_client_ms: Some(20.0),
                max_client_ms: Some(40.0),
                successes: Vec::new(),
                failures: Vec::new(),
            },
            100,
        );

        assert!(run.passed);
        assert_eq!(run.requested_clients, 100);
        assert_eq!(run.min_success_clients, 100);
        assert_eq!(run.connections_per_sec, 50.0);
    }

    #[test]
    fn load_test_run_fails_when_any_client_fails() {
        let run = stratum_load_test_run_from_smoke(
            StratumSmokeRun {
                endpoint: "127.0.0.1:3333".to_owned(),
                requested_clients: 100,
                succeeded_clients: 99,
                failed_clients: 1,
                malformed_sent: false,
                elapsed_ms: 1_000.0,
                min_client_ms: Some(10.0),
                avg_client_ms: Some(20.0),
                max_client_ms: Some(40.0),
                successes: Vec::new(),
                failures: vec![StratumSmokeFailure {
                    client_index: 99,
                    worker: smoke_worker_name(99),
                    error: "connect failed".to_owned(),
                }],
            },
            90,
        );

        assert!(!run.passed);
        assert_eq!(run.succeeded_clients, 99);
        assert_eq!(run.failed_clients, 1);
        assert_eq!(run.failures[0].worker, smoke_worker_name(99));
    }

    #[test]
    fn stratum_gate_errors_are_descriptive() {
        let smoke_error = WorkerError::StratumSmokeFailed {
            requested_clients: 3,
            failed_clients: 1,
        };
        assert_eq!(
            smoke_error.to_string(),
            "stratum smoke failed: 1 of 3 clients failed"
        );

        let load_error = WorkerError::StratumLoadTestFailed {
            requested_clients: 100,
            min_success_clients: 100,
            succeeded_clients: 99,
            failed_clients: 1,
        };
        assert_eq!(
            load_error.to_string(),
            "stratum load test failed: 99/100 clients succeeded, 1 failed, minimum success is 100"
        );
    }

    #[test]
    fn ledger_entries_csv_exports_signed_amounts_and_pool_rows() {
        let csv = ledger_entries_csv(&[
            LedgerEntry {
                miner: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
                amount_base_units: 125_000_000,
                kind: csd_pool_accounting::LedgerKind::RewardMature,
                ref_type: "block".to_owned(),
                ref_id: "hash,quoted".to_owned(),
            },
            LedgerEntry {
                miner: None,
                amount_base_units: -25_000_000,
                kind: csd_pool_accounting::LedgerKind::PoolFee,
                ref_type: "block".to_owned(),
                ref_id: "pool-fee".to_owned(),
            },
        ]);

        assert!(
            csv.starts_with("entry_index,miner,amount_base_units,amount_csd,kind,ref_type,ref_id")
        );
        assert!(csv.contains("125000000,1.25000000,reward_mature"));
        assert!(csv.contains("pool,-25000000,-0.25000000,pool_fee"));
        assert!(csv.contains("\"hash,quoted\""));
    }
}
