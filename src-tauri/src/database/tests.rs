//! 数据库模块测试
//!
//! 包含 Schema 迁移和基本功能的测试。

use super::*;
use crate::app_config::MultiAppConfig;
use crate::provider::{Provider, ProviderManager};
use indexmap::IndexMap;
use rusqlite::{params, Connection};
use serde_json::json;
use std::collections::HashMap;
use tempfile::NamedTempFile;

const LEGACY_SCHEMA_SQL: &str = r#"
    CREATE TABLE providers (
        id TEXT NOT NULL,
        app_type TEXT NOT NULL,
        name TEXT NOT NULL,
        settings_config TEXT NOT NULL,
        PRIMARY KEY (id, app_type)
    );
    CREATE TABLE provider_endpoints (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        provider_id TEXT NOT NULL,
        app_type TEXT NOT NULL,
        url TEXT NOT NULL
    );
    CREATE TABLE mcp_servers (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        server_config TEXT NOT NULL
    );
    CREATE TABLE prompts (
        id TEXT NOT NULL,
        app_type TEXT NOT NULL,
        name TEXT NOT NULL,
        content TEXT NOT NULL,
        PRIMARY KEY (id, app_type)
    );
    CREATE TABLE skills (
        key TEXT PRIMARY KEY,
        installed BOOLEAN NOT NULL DEFAULT 0
    );
    CREATE TABLE skill_repos (
        owner TEXT NOT NULL,
        name TEXT NOT NULL,
        PRIMARY KEY (owner, name)
    );
    CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT
    );
"#;

// v3.8.x（schema v1）的真实表结构快照：用于验证从 v3.8.* 升级到当前版本的迁移链路
// 参考：tag v3.8.3 的 src-tauri/src/database/schema.rs
pub(super) const V3_8_SCHEMA_V1_SQL: &str = r#"
    CREATE TABLE providers (
        id TEXT NOT NULL,
        app_type TEXT NOT NULL,
        name TEXT NOT NULL,
        settings_config TEXT NOT NULL,
        website_url TEXT,
        category TEXT,
        created_at INTEGER,
        sort_index INTEGER,
        notes TEXT,
        icon TEXT,
        icon_color TEXT,
        meta TEXT NOT NULL DEFAULT '{}',
        is_current BOOLEAN NOT NULL DEFAULT 0,
        PRIMARY KEY (id, app_type)
    );
    CREATE TABLE provider_endpoints (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        provider_id TEXT NOT NULL,
        app_type TEXT NOT NULL,
        url TEXT NOT NULL,
        added_at INTEGER,
        FOREIGN KEY (provider_id, app_type) REFERENCES providers(id, app_type) ON DELETE CASCADE
    );
    CREATE TABLE mcp_servers (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        server_config TEXT NOT NULL,
        description TEXT,
        homepage TEXT,
        docs TEXT,
        tags TEXT NOT NULL DEFAULT '[]',
        enabled_claude BOOLEAN NOT NULL DEFAULT 0,
        enabled_codex BOOLEAN NOT NULL DEFAULT 0,
        enabled_gemini BOOLEAN NOT NULL DEFAULT 0
    );
    CREATE TABLE prompts (
        id TEXT NOT NULL,
        app_type TEXT NOT NULL,
        name TEXT NOT NULL,
        content TEXT NOT NULL,
        description TEXT,
        enabled BOOLEAN NOT NULL DEFAULT 1,
        created_at INTEGER,
        updated_at INTEGER,
        PRIMARY KEY (id, app_type)
    );
    CREATE TABLE skills (
        key TEXT PRIMARY KEY,
        installed BOOLEAN NOT NULL DEFAULT 0,
        installed_at INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE skill_repos (
        owner TEXT NOT NULL,
        name TEXT NOT NULL,
        branch TEXT NOT NULL DEFAULT 'main',
        enabled BOOLEAN NOT NULL DEFAULT 1,
        PRIMARY KEY (owner, name)
    );
    CREATE TABLE settings (
        key TEXT PRIMARY KEY,
        value TEXT
    );
"#;

#[derive(Debug)]
struct ColumnInfo {
    r#type: String,
    notnull: i64,
    default: Option<String>,
}

fn get_column_info(conn: &Connection, table: &str, column: &str) -> ColumnInfo {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{table}\");"))
        .expect("prepare pragma");
    let mut rows = stmt.query([]).expect("query pragma");
    while let Some(row) = rows.next().expect("read row") {
        let column_name: String = row.get(1).expect("name");
        if column_name.eq_ignore_ascii_case(column) {
            return ColumnInfo {
                r#type: row.get::<_, String>(2).expect("type"),
                notnull: row.get::<_, i64>(3).expect("notnull"),
                default: row.get::<_, Option<String>>(4).ok().flatten(),
            };
        }
    }
    panic!("column {table}.{column} not found");
}

fn normalize_default(default: &Option<String>) -> Option<String> {
    default
        .as_ref()
        .map(|s| s.trim_matches('\'').trim_matches('"').to_string())
}

#[test]
fn deleted_default_skill_repo_is_not_restored() {
    let db = Database::memory().expect("create memory db");

    assert_eq!(db.init_default_skill_repos().expect("initialize repos"), 4);
    for repo in db.get_skill_repos().expect("get initialized repos") {
        db.delete_skill_repo(&repo.owner, &repo.name)
            .expect("delete repo");
    }
    assert!(db.get_skill_repos().expect("get deleted repos").is_empty());

    assert_eq!(
        db.init_default_skill_repos().expect("reinitialize repos"),
        0
    );
    assert!(db.get_skill_repos().expect("get repos").is_empty());
}

#[test]
fn existing_skill_repo_selection_is_not_supplemented() {
    let db = Database::memory().expect("create memory db");
    let default_store = crate::services::skill::SkillStore::default();
    db.save_skill_repo(&default_store.repos[0])
        .expect("save existing repo");

    assert_eq!(db.init_default_skill_repos().expect("initialize repos"), 0);
    assert_eq!(db.get_skill_repos().expect("get repos").len(), 1);
    assert!(db
        .get_bool_flag("default_skill_repos_initialized")
        .expect("get initialized flag"));
}

#[test]
fn schema_migration_sets_user_version_when_missing() {
    let conn = Connection::open_in_memory().expect("open memory db");

    Database::create_tables_on_conn(&conn).expect("create tables");
    assert_eq!(
        Database::get_user_version(&conn).expect("read version before"),
        0
    );

    Database::apply_schema_migrations_on_conn(&conn).expect("apply migration");

    assert_eq!(
        Database::get_user_version(&conn).expect("read version after"),
        SCHEMA_VERSION
    );
}

#[test]
fn schema_migration_rejects_future_version() {
    let conn = Connection::open_in_memory().expect("open memory db");
    Database::create_tables_on_conn(&conn).expect("create tables");
    Database::set_user_version(&conn, SCHEMA_VERSION + 1).expect("set future version");

    let err =
        Database::apply_schema_migrations_on_conn(&conn).expect_err("should reject higher version");
    assert!(
        err.to_string().contains("数据库版本过新"),
        "unexpected error: {err}"
    );
}

#[test]
fn schema_migration_adds_missing_columns_for_providers() {
    let conn = Connection::open_in_memory().expect("open memory db");

    // 创建旧版 providers 表，缺少新增列
    conn.execute_batch(LEGACY_SCHEMA_SQL)
        .expect("seed old schema");

    Database::apply_schema_migrations_on_conn(&conn).expect("apply migrations");

    // 验证关键新增列已补齐
    for (table, column) in [
        ("providers", "meta"),
        ("providers", "is_current"),
        ("provider_endpoints", "added_at"),
        ("mcp_servers", "enabled_gemini"),
        ("prompts", "updated_at"),
        ("skills", "installed_at"),
        ("skill_repos", "enabled"),
    ] {
        assert!(
            Database::has_column(&conn, table, column).expect("check column"),
            "{table}.{column} should exist after migration"
        );
    }

    // 验证 meta 列约束保持一致
    let meta = get_column_info(&conn, "providers", "meta");
    assert_eq!(meta.notnull, 1, "meta should be NOT NULL");
    assert_eq!(
        normalize_default(&meta.default).as_deref(),
        Some("{}"),
        "meta default should be '{{}}'"
    );

    assert_eq!(
        Database::get_user_version(&conn).expect("version after migration"),
        SCHEMA_VERSION
    );
}

#[test]
fn schema_migration_aligns_column_defaults_and_types() {
    let conn = Connection::open_in_memory().expect("open memory db");
    conn.execute_batch(LEGACY_SCHEMA_SQL)
        .expect("seed old schema");

    Database::apply_schema_migrations_on_conn(&conn).expect("apply migrations");

    let is_current = get_column_info(&conn, "providers", "is_current");
    assert_eq!(is_current.r#type, "BOOLEAN");
    assert_eq!(is_current.notnull, 1);
    assert_eq!(normalize_default(&is_current.default).as_deref(), Some("0"));

    let tags = get_column_info(&conn, "mcp_servers", "tags");
    assert_eq!(tags.r#type, "TEXT");
    assert_eq!(tags.notnull, 1);
    assert_eq!(normalize_default(&tags.default).as_deref(), Some("[]"));

    let enabled = get_column_info(&conn, "prompts", "enabled");
    assert_eq!(enabled.r#type, "BOOLEAN");
    assert_eq!(enabled.notnull, 1);
    assert_eq!(normalize_default(&enabled.default).as_deref(), Some("1"));

    let installed_at = get_column_info(&conn, "skills", "installed_at");
    assert_eq!(installed_at.r#type, "INTEGER");
    assert_eq!(installed_at.notnull, 1);
    assert_eq!(
        normalize_default(&installed_at.default).as_deref(),
        Some("0")
    );

    let branch = get_column_info(&conn, "skill_repos", "branch");
    assert_eq!(branch.r#type, "TEXT");
    assert_eq!(normalize_default(&branch.default).as_deref(), Some("main"));

    let skill_repo_enabled = get_column_info(&conn, "skill_repos", "enabled");
    assert_eq!(skill_repo_enabled.r#type, "BOOLEAN");
    assert_eq!(skill_repo_enabled.notnull, 1);
    assert_eq!(
        normalize_default(&skill_repo_enabled.default).as_deref(),
        Some("1")
    );
}

#[test]
fn schema_create_tables_include_pricing_model_columns() {
    let conn = Connection::open_in_memory().expect("open memory db");
    Database::create_tables_on_conn(&conn).expect("create tables");

    let multiplier = get_column_info(&conn, "proxy_config", "default_cost_multiplier");
    assert_eq!(multiplier.r#type, "TEXT");
    assert_eq!(multiplier.notnull, 1);
    assert_eq!(normalize_default(&multiplier.default).as_deref(), Some("1"));

    let pricing_source = get_column_info(&conn, "proxy_config", "pricing_model_source");
    assert_eq!(pricing_source.r#type, "TEXT");
    assert_eq!(pricing_source.notnull, 1);
    assert_eq!(
        normalize_default(&pricing_source.default).as_deref(),
        Some("response")
    );

    let request_model = get_column_info(&conn, "proxy_request_logs", "request_model");
    assert_eq!(request_model.r#type, "TEXT");
    assert_eq!(request_model.notnull, 0);
}

#[test]
fn schema_migration_v4_adds_pricing_model_columns() {
    let conn = Connection::open_in_memory().expect("open memory db");
    conn.execute_batch(
        r#"
        CREATE TABLE providers (
            id TEXT NOT NULL,
            app_type TEXT NOT NULL,
            name TEXT NOT NULL,
            settings_config TEXT NOT NULL DEFAULT '{}',
            meta TEXT NOT NULL DEFAULT '{}',
            PRIMARY KEY (id, app_type)
        );
        CREATE TABLE proxy_config (app_type TEXT PRIMARY KEY);
        CREATE TABLE proxy_request_logs (request_id TEXT PRIMARY KEY, model TEXT NOT NULL);
        CREATE TABLE mcp_servers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            server_config TEXT NOT NULL,
            enabled_claude INTEGER NOT NULL DEFAULT 0,
            enabled_codex INTEGER NOT NULL DEFAULT 0,
            enabled_gemini INTEGER NOT NULL DEFAULT 0,
            enabled_opencode INTEGER NOT NULL DEFAULT 0
        );
        "#,
    )
    .expect("seed v4 schema");

    Database::set_user_version(&conn, 4).expect("set user_version=4");
    Database::apply_schema_migrations_on_conn(&conn).expect("apply migrations");

    let multiplier = get_column_info(&conn, "proxy_config", "default_cost_multiplier");
    assert_eq!(multiplier.r#type, "TEXT");
    assert_eq!(multiplier.notnull, 1);
    assert_eq!(normalize_default(&multiplier.default).as_deref(), Some("1"));

    let pricing_source = get_column_info(&conn, "proxy_config", "pricing_model_source");
    assert_eq!(pricing_source.r#type, "TEXT");
    assert_eq!(pricing_source.notnull, 1);
    assert_eq!(
        normalize_default(&pricing_source.default).as_deref(),
        Some("response")
    );

    let request_model = get_column_info(&conn, "proxy_request_logs", "request_model");
    assert_eq!(request_model.r#type, "TEXT");
    assert_eq!(request_model.notnull, 0);

    assert_eq!(
        Database::get_user_version(&conn).expect("version after migration"),
        SCHEMA_VERSION
    );
}

#[test]
fn shared_keys_migration_from_local_v12_to_v17_centralizes_and_deduplicates() {
    let conn = Connection::open_in_memory().expect("open db");
    conn.execute("PRAGMA foreign_keys = ON", [])
        .expect("enable foreign keys");
    Database::create_tables_on_conn(&conn).expect("create current tables");

    let claude_meta = json!({
        "apiKeys": [
            {"id": "claude-a", "label": "A", "key": "sk-a", "strategy": "anthropic"},
            {"id": "claude-shared", "label": "", "key": "sk-shared", "strategy": "anthropic"}
        ],
        "selectedKeyId": "claude-a"
    });
    let codex_meta = json!({
        "apiKeys": [
            {"id": "codex-shared", "label": "Shared", "key": "sk-shared", "strategy": "bearer"},
            {"id": "codex-b", "label": "B", "key": "sk-b", "strategy": "bearer"}
        ],
        "selectedKeyId": "codex-b"
    });
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, meta)
         VALUES (?1, 'claude', ?2, ?3, ?4)",
        params![
            "claude-provider",
            " Same Vendor ",
            json!({"env": {"ANTHROPIC_BASE_URL": "https://claude.vendor.example"}}).to_string(),
            claude_meta.to_string()
        ],
    )
    .expect("insert Claude provider");
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, meta)
         VALUES (?1, 'codex', ?2, ?3, ?4)",
        params![
            "codex-provider",
            "same vendor",
            json!({"config": "base_url = \"https://codex.vendor.example\""}).to_string(),
            codex_meta.to_string()
        ],
    )
    .expect("insert Codex provider");
    // Local v3.16.5 used Schema v12 for shared keys while upstream later reused
    // v12 for Profiles. Starting at 12 verifies that create_tables + v12..v17
    // safely upgrades an existing local database without losing its key lists.
    Database::set_user_version(&conn, 12).expect("set local user_version=12");

    Database::apply_schema_migrations_on_conn(&conn).expect("migrate to current schema");

    let (key_count, link_count, group_count): (i64, i64, i64) = conn
        .query_row(
            "SELECT
                (SELECT COUNT(*) FROM shared_api_keys),
                (SELECT COUNT(*) FROM provider_shared_key_links),
                (SELECT COUNT(DISTINCT group_id) FROM provider_shared_key_links)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("read shared key counts");
    assert_eq!(key_count, 3);
    assert_eq!(link_count, 2);
    assert_eq!(group_count, 1);

    let mut stmt = conn
        .prepare("SELECT meta FROM providers ORDER BY app_type")
        .expect("prepare provider metas");
    let metas = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query provider metas")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect provider metas");
    assert!(metas.iter().all(|raw| {
        let meta: serde_json::Value = serde_json::from_str(raw).expect("valid meta");
        meta.get("apiKeys").is_none() && meta.get("selectedKeyId").is_some()
    }));
    assert_eq!(
        Database::get_user_version(&conn).expect("version after migration"),
        SCHEMA_VERSION
    );
}

#[test]
fn shared_keys_migration_preserves_non_text_provider_configs() {
    let conn = Connection::open_in_memory().expect("open db");
    Database::create_tables_on_conn(&conn).expect("create current tables");
    conn.execute(
        "INSERT INTO providers (id, app_type, name, settings_config, meta)
         VALUES ('blob-provider', 'claude', 'Blob Provider', X'00FF10', '{}')",
        [],
    )
    .expect("insert provider with blob config");
    Database::set_user_version(&conn, 16).expect("set user_version=16");

    Database::apply_schema_migrations_on_conn(&conn).expect("migrate to current schema");

    let config: Vec<u8> = conn
        .query_row(
            "SELECT settings_config FROM providers WHERE id = 'blob-provider'",
            [],
            |row| row.get(0),
        )
        .expect("read preserved blob config");
    assert_eq!(config, vec![0x00, 0xFF, 0x10]);
    let link_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM provider_shared_key_links
             WHERE provider_id = 'blob-provider'",
            [],
            |row| row.get(0),
        )
        .expect("count shared key links");
    assert_eq!(link_count, 0);
    assert_eq!(
        Database::get_user_version(&conn).expect("version after migration"),
        SCHEMA_VERSION
    );
}

#[test]
fn migration_v10_to_v11_rebuilds_rollups_with_request_model_dimension() {
    let conn = Connection::open_in_memory().expect("open memory db");

    // 模拟 v10 形状的 rollup 表（主键不含 request_model）+ 一行历史聚合数据，
    // 以及 v10 形状的明细表（无 pricing_model 列）
    conn.execute_batch(
        r#"
        CREATE TABLE proxy_request_logs (
            request_id TEXT PRIMARY KEY,
            model TEXT NOT NULL,
            request_model TEXT
        );
        CREATE TABLE usage_daily_rollups (
            date TEXT NOT NULL,
            app_type TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            model TEXT NOT NULL,
            request_count INTEGER NOT NULL DEFAULT 0,
            success_count INTEGER NOT NULL DEFAULT 0,
            input_tokens INTEGER NOT NULL DEFAULT 0,
            output_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0,
            cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
            total_cost_usd TEXT NOT NULL DEFAULT '0',
            avg_latency_ms INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (date, app_type, provider_id, model)
        );
        INSERT INTO usage_daily_rollups
            (date, app_type, provider_id, model, request_count, success_count,
             input_tokens, output_tokens, total_cost_usd, avg_latency_ms)
        VALUES ('2026-05-01', 'claude', 'p1', 'kimi-k2', 7, 7, 1000, 500, '0.07', 120);
        "#,
    )
    .expect("seed v10 rollup table");

    Database::set_user_version(&conn, 10).expect("set user_version=10");
    Database::apply_schema_migrations_on_conn(&conn).expect("apply migrations");

    // 新列存在且 NOT NULL DEFAULT ''
    let request_model = get_column_info(&conn, "usage_daily_rollups", "request_model");
    assert_eq!(request_model.r#type, "TEXT");
    assert_eq!(request_model.notnull, 1);
    let rollup_pricing_model = get_column_info(&conn, "usage_daily_rollups", "pricing_model");
    assert_eq!(rollup_pricing_model.r#type, "TEXT");
    assert_eq!(rollup_pricing_model.notnull, 1);

    // 明细表补上 pricing_model 列（可空，历史行 NULL）
    let pricing_model = get_column_info(&conn, "proxy_request_logs", "pricing_model");
    assert_eq!(pricing_model.r#type, "TEXT");
    assert_eq!(pricing_model.notnull, 0);

    // 历史行保留，request_model 填 ''（未知）
    let (rm, count, input, cost): (String, i64, i64, String) = conn
        .query_row(
            "SELECT request_model, request_count, input_tokens, total_cost_usd
             FROM usage_daily_rollups WHERE model = 'kimi-k2'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("migrated row");
    assert_eq!(rm, "");
    assert_eq!(count, 7);
    assert_eq!(input, 1000);
    assert_eq!(cost, "0.07");

    // 主键包含 request_model：同 model 不同别名可共存
    conn.execute(
        "INSERT INTO usage_daily_rollups
            (date, app_type, provider_id, model, request_model, request_count)
         VALUES ('2026-05-01', 'claude', 'p1', 'kimi-k2', 'claude-sonnet-4-6', 1)",
        [],
    )
    .expect("insert row with same model but different request_model");

    assert_eq!(
        Database::get_user_version(&conn).expect("version after migration"),
        SCHEMA_VERSION
    );
}

#[test]
fn schema_create_tables_repairs_dev_global_profile_marker() {
    let conn = Connection::open_in_memory().expect("open memory db");

    // 模拟跑过未发布开发版的库：user_version 已是 12（迁移不会再跑），
    // 但 current 标记还是全局 key（现按应用分组）
    conn.execute_batch(
        r#"
        CREATE TABLE profiles (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            payload TEXT NOT NULL,
            sort_order INTEGER,
            created_at INTEGER,
            updated_at INTEGER
        );
        INSERT INTO profiles (id, name, payload) VALUES ('p1', 'Project A', '{}');
        CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
        INSERT INTO settings (key, value) VALUES ('current_profile_id', 'p1');
        "#,
    )
    .expect("seed dev v12 shape");
    Database::set_user_version(&conn, 12).expect("set user_version=12");

    Database::create_tables_on_conn(&conn).expect("create tables should repair marker");

    // 全局 current 标记改名为 claude 组标记，旧 key 删除
    let claude_marker: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'current_profile_id_claude'",
            [],
            |row| row.get(0),
        )
        .expect("scoped current marker");
    assert_eq!(claude_marker, "p1");
    let old_marker: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM settings WHERE key = 'current_profile_id'",
            [],
            |row| row.get(0),
        )
        .expect("count old marker");
    assert_eq!(old_marker, 0);

    // 修复必须幂等：再跑一遍不应破坏已迁移的标记
    Database::create_tables_on_conn(&conn).expect("repair is idempotent");
    let claude_marker: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'current_profile_id_claude'",
            [],
            |row| row.get(0),
        )
        .expect("scoped current marker survives");
    assert_eq!(claude_marker, "p1");
}

#[test]
fn schema_create_tables_repairs_legacy_proxy_config_singleton_to_per_app() {
    let conn = Connection::open_in_memory().expect("open memory db");

    // 模拟测试版 v2：user_version=2，但 proxy_config 仍是单例结构（无 app_type）
    Database::set_user_version(&conn, 2).expect("set user_version");
    conn.execute_batch(
        r#"
        CREATE TABLE proxy_config (
            id INTEGER PRIMARY KEY,
            enabled INTEGER NOT NULL DEFAULT 0,
            listen_address TEXT NOT NULL DEFAULT '127.0.0.1',
            listen_port INTEGER NOT NULL DEFAULT 5000,
            max_retries INTEGER NOT NULL DEFAULT 3,
            request_timeout INTEGER NOT NULL DEFAULT 300,
            enable_logging INTEGER NOT NULL DEFAULT 1,
            target_app TEXT NOT NULL DEFAULT 'claude',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT INTO proxy_config (id, enabled) VALUES (1, 1);
        "#,
    )
    .expect("seed legacy proxy_config");

    Database::create_tables_on_conn(&conn).expect("create tables should repair proxy_config");

    assert!(
        Database::has_column(&conn, "proxy_config", "app_type").expect("check app_type"),
        "proxy_config should be migrated to per-app structure"
    );

    let count: i32 = conn
        .query_row("SELECT COUNT(*) FROM proxy_config", [], |r| r.get(0))
        .expect("count rows");
    assert_eq!(count, 4, "per-app proxy_config should have 4 rows");

    // 新结构下应能按 app_type 查询
    let _: i32 = conn
        .query_row(
            "SELECT COUNT(*) FROM proxy_config WHERE app_type = 'claude'",
            [],
            |r| r.get(0),
        )
        .expect("query by app_type");
}

#[test]
fn migration_from_v3_8_schema_v1_to_current_schema_v3() {
    let conn = Connection::open_in_memory().expect("open memory db");
    conn.execute("PRAGMA foreign_keys = ON;", [])
        .expect("enable foreign keys");

    // 模拟 v3.8.* 用户的数据库（schema v1）
    conn.execute_batch(V3_8_SCHEMA_V1_SQL)
        .expect("seed v3.8 schema v1");
    Database::set_user_version(&conn, 1).expect("set user_version=1");

    // 插入一条旧版 Provider + Skill（用于验证迁移不会破坏既有数据）
    conn.execute(
        "INSERT INTO providers (
            id, app_type, name, settings_config, website_url, category,
            created_at, sort_index, notes, icon, icon_color, meta, is_current
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            "p1",
            "claude",
            "Test Provider",
            serde_json::to_string(&json!({ "anthropicApiKey": "sk-test" })).unwrap(),
            Option::<String>::None,
            Option::<String>::None,
            Option::<i64>::None,
            Option::<usize>::None,
            Option::<String>::None,
            Option::<String>::None,
            Option::<String>::None,
            "{}",
            1,
        ],
    )
    .expect("seed provider");

    conn.execute(
        "INSERT INTO skills (key, installed, installed_at) VALUES (?1, ?2, ?3)",
        params!["claude:demo-skill", 1, 1700000000i64],
    )
    .expect("seed legacy skill");

    // 按应用启动流程：先 create_tables（补齐新增表），再 apply_schema_migrations（按 user_version 迁移）
    Database::create_tables_on_conn(&conn).expect("create tables");
    Database::apply_schema_migrations_on_conn(&conn).expect("apply migrations");

    assert_eq!(
        Database::get_user_version(&conn).expect("user_version after migration"),
        SCHEMA_VERSION
    );

    // v1 -> v2：providers 新增字段必须补齐
    for column in [
        "cost_multiplier",
        "limit_daily_usd",
        "limit_monthly_usd",
        "provider_type",
        "in_failover_queue",
    ] {
        assert!(
            Database::has_column(&conn, "providers", column).expect("check column"),
            "providers.{column} should exist after migration"
        );
    }

    // 旧 provider 不应丢失，且新增字段应有默认值
    let provider_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM providers WHERE id = 'p1' AND app_type = 'claude'",
            [],
            |r| r.get(0),
        )
        .expect("count providers");
    assert_eq!(provider_count, 1);

    let cost_multiplier: String = conn
        .query_row(
            "SELECT cost_multiplier FROM providers WHERE id = 'p1' AND app_type = 'claude'",
            [],
            |r| r.get(0),
        )
        .expect("read cost_multiplier");
    assert_eq!(cost_multiplier, "1.0");

    // v2 -> v3：skills 表重建为统一结构，并设置 pending 标记（后续由启动时扫描文件系统重建数据）
    assert!(
        Database::has_column(&conn, "skills", "enabled_claude").expect("check skills v3 column"),
        "skills table should be migrated to v3 structure"
    );
    let skills_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM skills", [], |r| r.get(0))
        .expect("count skills");
    assert_eq!(skills_count, 0, "skills table should be rebuilt empty");

    let pending: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'skills_ssot_migration_pending'",
            [],
            |r| r.get(0),
        )
        .ok();
    assert!(
        matches!(pending.as_deref(), Some("true") | Some("1")),
        "skills_ssot_migration_pending should be set after v2->v3 migration"
    );
    let snapshot: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'skills_ssot_migration_snapshot'",
            [],
            |r| r.get(0),
        )
        .ok();
    let snapshot = snapshot.expect("skills migration snapshot should be recorded");
    let snapshot_rows: serde_json::Value =
        serde_json::from_str(&snapshot).expect("parse skills migration snapshot");
    assert!(
        snapshot_rows
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| {
                row.get("directory").and_then(|v| v.as_str()) == Some("demo-skill")
                    && row.get("app_type").and_then(|v| v.as_str()) == Some("claude")
            })),
        "skills migration snapshot should preserve legacy app mapping"
    );

    // v3.9+ 新增：proxy_config 三行 seed 必须存在（否则 UI 会查不到默认值）
    let proxy_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM proxy_config", [], |r| r.get(0))
        .expect("count proxy_config rows");
    assert_eq!(proxy_rows, 4);

    // model_pricing 应具备默认数据（迁移时会 seed）
    let pricing_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM model_pricing", [], |r| r.get(0))
        .expect("count model_pricing rows");
    assert!(pricing_rows > 0, "model_pricing should be seeded");
}

#[test]
fn schema_dry_run_does_not_write_to_disk() {
    // Create minimal valid config for migration
    let mut apps = HashMap::new();
    apps.insert("claude".to_string(), ProviderManager::default());

    let config = MultiAppConfig {
        version: 2,
        apps,
        mcp: Default::default(),
        prompts: Default::default(),
        skills: Default::default(),
        common_config_snippets: Default::default(),
        claude_common_config_snippet: None,
    };

    // Dry-run should succeed without any file I/O errors
    let result = Database::migrate_from_json_dry_run(&config);
    assert!(
        result.is_ok(),
        "Dry-run should succeed with valid config: {result:?}"
    );
}

#[test]
fn dry_run_validates_schema_compatibility() {
    // Create config with actual provider data
    let mut providers = IndexMap::new();
    providers.insert(
        "test-provider".to_string(),
        Provider {
            id: "test-provider".to_string(),
            name: "Test Provider".to_string(),
            settings_config: json!({
                "anthropicApiKey": "sk-test-123",
            }),
            website_url: None,
            category: None,
            created_at: Some(1234567890),
            sort_index: None,
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        },
    );

    let manager = ProviderManager {
        providers,
        current: "test-provider".to_string(),
    };

    let mut apps = HashMap::new();
    apps.insert("claude".to_string(), manager);

    let config = MultiAppConfig {
        version: 2,
        apps,
        mcp: Default::default(),
        prompts: Default::default(),
        skills: Default::default(),
        common_config_snippets: Default::default(),
        claude_common_config_snippet: None,
    };

    // Dry-run should validate the full migration path
    let result = Database::migrate_from_json_dry_run(&config);
    assert!(
        result.is_ok(),
        "Dry-run should succeed with provider data: {result:?}"
    );
}

#[test]
fn schema_model_pricing_is_seeded_on_init() {
    let db = Database::memory().expect("create memory db");

    let conn = db.conn.lock().expect("lock conn");

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM model_pricing", [], |row| row.get(0))
        .expect("count pricing");

    assert!(
        count > 0,
        "模型定价数据应该在初始化时自动填充，实际数量: {}",
        count
    );

    // 验证包含 Claude 模型
    let claude_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM model_pricing WHERE model_id LIKE 'claude-%'",
            [],
            |row| row.get(0),
        )
        .expect("check claude");
    assert!(
        claude_count > 0,
        "应该包含 Claude 模型定价，实际数量: {}",
        claude_count
    );

    // 验证包含 GPT 模型
    let gpt_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM model_pricing WHERE model_id LIKE 'gpt-%'",
            [],
            |row| row.get(0),
        )
        .expect("check gpt");
    assert!(
        gpt_count > 0,
        "应该包含 GPT 模型定价，实际数量: {}",
        gpt_count
    );

    // 验证包含 Gemini 模型
    let gemini_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM model_pricing WHERE model_id LIKE 'gemini-%'",
            [],
            |row| row.get(0),
        )
        .expect("check gemini");
    assert!(
        gemini_count > 0,
        "应该包含 Gemini 模型定价，实际数量: {}",
        gemini_count
    );
}

#[test]
fn model_pricing_seed_repairs_known_outdated_builtin_prices() {
    let db = Database::memory().expect("create memory db");

    {
        let conn = db.conn.lock().expect("lock conn");
        conn.execute(
            "UPDATE model_pricing
             SET input_cost_per_million = '1.68',
                 output_cost_per_million = '3.36',
                 cache_read_cost_per_million = '0.14',
                 cache_creation_cost_per_million = '0'
             WHERE model_id = 'deepseek-v4-pro'",
            [],
        )
        .expect("restore old DeepSeek price");
        conn.execute(
            "UPDATE model_pricing
             SET input_cost_per_million = '9',
                 output_cost_per_million = '9',
                 cache_read_cost_per_million = '9',
                 cache_creation_cost_per_million = '0'
             WHERE model_id = 'glm-5.1'",
            [],
        )
        .expect("set custom GLM price");
        // <v3.19 老库形态：cache_write 仍是最初 seed 的 0（07-12 条目才补成 6.25）
        conn.execute(
            "UPDATE model_pricing
             SET input_cost_per_million = '5',
                 output_cost_per_million = '30',
                 cache_read_cost_per_million = '0.50',
                 cache_creation_cost_per_million = '0'
             WHERE model_id = 'gpt-5.6-sol'",
            [],
        )
        .expect("restore pre-v3.19 GPT-5.6 Sol price");
        // 最早 seed 的 M2.5 价（bb7c83c2 时代）
        conn.execute(
            "UPDATE model_pricing
             SET input_cost_per_million = '0.12',
                 output_cost_per_million = '0.95',
                 cache_read_cost_per_million = '0.03',
                 cache_creation_cost_per_million = '0'
             WHERE model_id = 'minimax-m2.5'",
            [],
        )
        .expect("restore oldest MiniMax M2.5 price");
        // 2026-07-31 之前的 V4 Flash 形态（cache_read 尚未修正为 0.0028）
        conn.execute(
            "UPDATE model_pricing
             SET input_cost_per_million = '0.14',
                 output_cost_per_million = '0.28',
                 cache_read_cost_per_million = '0.028',
                 cache_creation_cost_per_million = '0'
             WHERE model_id = 'deepseek-v4-flash'",
            [],
        )
        .expect("restore oldest DeepSeek V4 Flash price");
    }

    db.ensure_model_pricing_seeded()
        .expect("ensure pricing seeded");

    let conn = db.conn.lock().expect("lock conn");
    let deepseek: (String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million, cache_read_cost_per_million
             FROM model_pricing WHERE model_id = 'deepseek-v4-pro'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("query DeepSeek price");
    // 从远古价 1.68/3.36/0.14 出发要连跳三级才能到位：
    //   1.68/3.36/0.14 →(2026-07 条目)→ 0.435/0.87/0.003625
    //                  →(2026-08-16 峰谷调价条目)→ 1.32/3.96/0.044
    //                  →(2026-09-14 起 V4 Pro 路由到 V4.1 Flash)→ 0.3/1.2/0.006
    // 这同时锁住了 repair 条目的顺序：新条目必须排在旧条目之后，
    // 否则老库会停在中间价位，本断言即会失败。
    assert_eq!(
        deepseek,
        ("0.3".to_string(), "1.2".to_string(), "0.006".to_string())
    );

    let glm: (String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million, cache_read_cost_per_million
             FROM model_pricing WHERE model_id = 'glm-5.1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("query GLM price");
    assert_eq!(glm, ("9".to_string(), "9".to_string(), "9".to_string()));

    // 2026-09-06 条目同样依赖顺序：
    //   gpt-5.6-sol  5/30/0.50/0 →(07-12 补 cache_write)→ 5/30/0.50/6.25 →(09-06 促销)→ 4/20/0.40/5
    //   minimax-m2.5 0.12/0.95/0.03/0 →(0.12→0.15 条目)→ 0.15/… →(09-06 官方价)→ 0.30/1.20/0.03/0.375
    let sol: (String, String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million,
                    cache_read_cost_per_million, cache_creation_cost_per_million
             FROM model_pricing WHERE model_id = 'gpt-5.6-sol'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query GPT-5.6 Sol price");
    assert_eq!(
        sol,
        (
            "4".to_string(),
            "20".to_string(),
            "0.40".to_string(),
            "5".to_string()
        )
    );
    let m25: (String, String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million,
                    cache_read_cost_per_million, cache_creation_cost_per_million
             FROM model_pricing WHERE model_id = 'minimax-m2.5'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query MiniMax M2.5 price");
    assert_eq!(
        m25,
        (
            "0.30".to_string(),
            "1.20".to_string(),
            "0.03".to_string(),
            "0.375".to_string()
        )
    );

    // 2026-09-11 条目是 DeepSeek V4 Flash 链条的第三级，从最老形态出发要连跳三级：
    //   0.14/0.28/0.028 →(2026-07 修 cache_read)→ 0.14/0.28/0.0028
    //                   →(2026-08-16 峰谷调价)→ 0.44/1.32/0.014
    //                   →(2026-09-11 V4.1 Flash 承接)→ 0.3/1.2/0.006
    // 任一条目被挪到前面，老库都会停在中间价位，本断言即失败。
    let flash: (String, String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million,
                    cache_read_cost_per_million, cache_creation_cost_per_million
             FROM model_pricing WHERE model_id = 'deepseek-v4-flash'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query DeepSeek V4 Flash price");
    assert_eq!(
        flash,
        (
            "0.3".to_string(),
            "1.2".to_string(),
            "0.006".to_string(),
            "0".to_string()
        )
    );
}

#[test]
fn model_pricing_seed_covers_deepseek_v41_flash_aliases() {
    let db = Database::memory().expect("create memory db");
    let conn = db.conn.lock().expect("lock conn");

    // 官方定价页（2026-09-11）：deepseek-flash 是唯一推荐名，两个 legacy 名仍被接受
    // 但均由 V4.1-Flash 承接并按 Flash 价计费 → 四行同价（本表统一录高峰档）。
    // 查价前缀兜底是 LIKE '{id}-%'，只命中更长的行，任一行缺失都会静默按 0 计费。
    for model_id in [
        "deepseek-flash",
        "deepseek-v4-flash",
        "deepseek-v4-flash-0731",
        "deepseek-v4-flash-vision-exp",
    ] {
        let price: (String, String, String, String) = conn
            .query_row(
                "SELECT input_cost_per_million, output_cost_per_million,
                        cache_read_cost_per_million, cache_creation_cost_per_million
                 FROM model_pricing WHERE model_id = ?1",
                [model_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("query DeepSeek V4.1 Flash family price");
        assert_eq!(
            price,
            (
                "0.3".to_string(),
                "1.2".to_string(),
                "0.006".to_string(),
                "0".to_string()
            ),
            "{model_id}"
        );
    }
}

#[test]
fn model_pricing_seed_includes_claude_5_1_and_standard_sonnet_5_prices() {
    let db = Database::memory().expect("create memory db");
    let conn = db.conn.lock().expect("lock conn");

    for model_id in ["claude-fable-5-1", "claude-mythos-5-1"] {
        let price: (String, String, String, String) = conn
            .query_row(
                "SELECT input_cost_per_million, output_cost_per_million,
                        cache_read_cost_per_million, cache_creation_cost_per_million
                 FROM model_pricing WHERE model_id = ?1",
                [model_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("query Fable 5.1 family price");
        // 缓存读 0.025x = $0.25，不是 Fable 5 的 $1
        assert_eq!(
            price,
            (
                "10".to_string(),
                "50".to_string(),
                "0.25".to_string(),
                "12.50".to_string(),
            ),
            "{model_id}"
        );
    }

    let sonnet: (String, String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million,
                    cache_read_cost_per_million, cache_creation_cost_per_million
             FROM model_pricing WHERE model_id = 'claude-sonnet-5'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query Sonnet 5 price");
    // $2/$10 介绍价已转为正式价（原定 2026-09-01 涨至 $3/$15 取消）
    assert_eq!(
        sonnet,
        (
            "2".to_string(),
            "10".to_string(),
            "0.20".to_string(),
            "2.50".to_string(),
        )
    );
}

#[test]
fn model_pricing_seed_includes_gpt_6_astra() {
    let db = Database::memory().expect("create memory db");
    let conn = db.conn.lock().expect("lock conn");

    let price: (String, String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million,
                    cache_read_cost_per_million, cache_creation_cost_per_million
             FROM model_pricing WHERE model_id = 'gpt-6-astra'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query GPT-6 Astra price");

    assert_eq!(
        price,
        (
            "10".to_string(),
            "50".to_string(),
            "1".to_string(),
            "12.5".to_string(),
        )
    );
}

#[test]
fn model_pricing_seed_includes_glm_5_3_flash() {
    let db = Database::memory().expect("create memory db");
    let conn = db.conn.lock().expect("lock conn");

    let price: (String, String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million,
                    cache_read_cost_per_million, cache_creation_cost_per_million
             FROM model_pricing WHERE model_id = 'glm-5.3-flash'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query GLM-5.3-Flash price");

    assert_eq!(
        price,
        (
            "0.15".to_string(),
            "0.50".to_string(),
            "0.03".to_string(),
            "0".to_string(),
        )
    );
}

#[test]
fn model_pricing_seed_includes_gemini_3_8_flash() {
    let db = Database::memory().expect("create memory db");
    let conn = db.conn.lock().expect("lock conn");

    let price: (String, String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million,
                    cache_read_cost_per_million, cache_creation_cost_per_million
             FROM model_pricing WHERE model_id = 'gemini-3.8-flash'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query Gemini 3.8 Flash price");

    assert_eq!(
        price,
        (
            "0.75".to_string(),
            "3.75".to_string(),
            "0.075".to_string(),
            "0".to_string(),
        )
    );
}

#[test]
fn model_pricing_seed_repairs_sonnet_5_list_price_but_keeps_custom_price() {
    let db = Database::memory().expect("create memory db");

    {
        let conn = db.conn.lock().expect("lock conn");
        // 旧 seed 按 list 价录入的行 → 应被修正
        conn.execute(
            "UPDATE model_pricing
             SET input_cost_per_million = '3',
                 output_cost_per_million = '15',
                 cache_read_cost_per_million = '0.30',
                 cache_creation_cost_per_million = '3.75'
             WHERE model_id = 'claude-sonnet-5'",
            [],
        )
        .expect("restore old Sonnet 5 list price");
    }

    db.ensure_model_pricing_seeded()
        .expect("ensure pricing seeded");

    {
        let conn = db.conn.lock().expect("lock conn");
        let sonnet: (String, String, String, String) = conn
            .query_row(
                "SELECT input_cost_per_million, output_cost_per_million,
                        cache_read_cost_per_million, cache_creation_cost_per_million
                 FROM model_pricing WHERE model_id = 'claude-sonnet-5'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("query repaired Sonnet 5 price");
        assert_eq!(
            sonnet,
            (
                "2".to_string(),
                "10".to_string(),
                "0.20".to_string(),
                "2.50".to_string(),
            )
        );

        // 用户手改过的价（不匹配旧 seed 值）不动
        conn.execute(
            "UPDATE model_pricing
             SET input_cost_per_million = '9',
                 output_cost_per_million = '9',
                 cache_read_cost_per_million = '9',
                 cache_creation_cost_per_million = '9'
             WHERE model_id = 'claude-sonnet-5'",
            [],
        )
        .expect("set custom Sonnet 5 price");
    }

    db.ensure_model_pricing_seeded()
        .expect("ensure pricing seeded again");

    let conn = db.conn.lock().expect("lock conn");
    let custom: (String, String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million,
                    cache_read_cost_per_million, cache_creation_cost_per_million
             FROM model_pricing WHERE model_id = 'claude-sonnet-5'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query custom Sonnet 5 price");
    assert_eq!(
        custom,
        (
            "9".to_string(),
            "9".to_string(),
            "9".to_string(),
            "9".to_string(),
        )
    );
}

#[test]
fn ensure_incremental_auto_vacuum_rebuilds_existing_file_db() {
    let temp = NamedTempFile::new().expect("create temp db file");
    let path = temp.path().to_path_buf();

    let conn = Connection::open(&path).expect("open temp db");
    conn.execute("PRAGMA auto_vacuum = NONE;", [])
        .expect("set none auto_vacuum");
    Database::create_tables_on_conn(&conn).expect("create tables");

    assert_eq!(
        Database::get_auto_vacuum_mode(&conn).expect("auto_vacuum before rebuild"),
        0,
        "existing file db should start with NONE auto_vacuum"
    );

    let rebuilt =
        Database::ensure_incremental_auto_vacuum_on_conn(&conn).expect("enable incremental mode");
    assert!(rebuilt, "existing db should require rebuild via VACUUM");
    drop(conn);

    let reopened = Connection::open(&path).expect("reopen temp db");
    assert_eq!(
        Database::get_auto_vacuum_mode(&reopened).expect("auto_vacuum after rebuild"),
        2,
        "file db should persist INCREMENTAL auto_vacuum after VACUUM rebuild"
    );
}

#[test]
fn migration_repairs_local_v17_missing_official_dedup_table() {
    let conn = Connection::open_in_memory().expect("open db");
    Database::create_tables_on_conn(&conn).expect("create current schema");
    // 旧本地 v17：共享 Key 已迁移，但缺少官方 v3.20.0 的 session_usage_dedup 表
    // （官方 v17 结构）。v17->v18 必须做结构探测并补齐官方表，且共享 Key 数据保持。
    conn.execute_batch(
        "DROP TABLE IF EXISTS session_usage_dedup;
         INSERT INTO providers (id, app_type, name, settings_config, meta)
         VALUES ('p', 'claude', 'P', '{\"env\":{}}', '{\"selectedKeyId\":\"k1\"}');
         INSERT INTO shared_key_groups (id, created_at) VALUES ('g1', 1);
         INSERT INTO shared_api_keys (id, group_id, label, key_value, sort_index)
         VALUES ('k1', 'g1', '', 'sk-secret', 0);
         INSERT INTO provider_shared_key_links (provider_id, app_type, group_id)
         VALUES ('p', 'claude', 'g1');",
    )
    .expect("seed local v17 state without official dedup");
    Database::set_user_version(&conn, 17).expect("set user_version=17");

    Database::apply_schema_migrations_on_conn(&conn).expect("migrate v17 -> v18");

    assert_eq!(
        Database::get_user_version(&conn).expect("version after migration"),
        SCHEMA_VERSION
    );
    assert!(
        Database::table_exists(&conn, "session_usage_dedup").expect("dedup repaired"),
        "v17->v18 should rebuild the official dedup table when missing"
    );
    let keys: i64 = conn
        .query_row("SELECT COUNT(*) FROM shared_api_keys", [], |r| r.get(0))
        .expect("shared keys preserved");
    assert_eq!(keys, 1);
    let links: i64 = conn
        .query_row("SELECT COUNT(*) FROM provider_shared_key_links", [], |r| {
            r.get(0)
        })
        .expect("provider links preserved");
    assert_eq!(links, 1);
}

#[test]
fn migration_is_idempotent_at_v18() {
    let conn = Connection::open_in_memory().expect("open db");
    Database::create_tables_on_conn(&conn).expect("create current schema");
    Database::apply_schema_migrations_on_conn(&conn).expect("migrate to v18");
    assert_eq!(
        Database::get_user_version(&conn).expect("version after first apply"),
        SCHEMA_VERSION
    );

    // 再次 apply（模拟 v18 数据库再次启动）不应有任何副作用。
    Database::apply_schema_migrations_on_conn(&conn).expect("reapply at v18");
    assert_eq!(
        Database::get_user_version(&conn).expect("version after reapply"),
        SCHEMA_VERSION
    );
    assert!(
        Database::table_exists(&conn, "session_usage_dedup").expect("dedup present"),
        "session_usage_dedup should exist after fresh v18 apply"
    );
    assert!(
        Database::table_exists(&conn, "shared_api_keys").expect("shared keys present"),
        "shared_api_keys should exist after fresh v18 apply"
    );
    assert!(
        Database::table_exists(&conn, "provider_shared_key_links").expect("links present"),
        "provider_shared_key_links should exist after fresh v18 apply"
    );
}

// ---------------------------------------------------------------------------
// v18 迁移矩阵：官方 v18 语义（会话字节游标列）∪ 本地 v18 语义（共享 Key 池）
// ---------------------------------------------------------------------------

/// 构造「官方 v17」库：有官方去重账本、无池表、`session_log_sync` 无字节游标列。
///
/// 必须先建当前 schema 再把结构退回历史形态——SQLite 不能删列，只能重建表；
/// 且不能沿用「先 create_tables 就当作官方旧库」，那会掩盖真实升级路径。
fn make_official_v17_like(conn: &Connection) {
    Database::create_tables_on_conn(conn).expect("create current schema");
    conn.execute_batch(
        "DROP TABLE IF EXISTS provider_shared_key_links;
         DROP TABLE IF EXISTS shared_api_keys;
         DROP TABLE IF EXISTS shared_key_groups;
         DROP TABLE session_log_sync;
         CREATE TABLE session_log_sync (
            file_path TEXT PRIMARY KEY,
            last_modified INTEGER NOT NULL,
            last_line_offset INTEGER NOT NULL DEFAULT 0,
            last_synced_at INTEGER NOT NULL
         );",
    )
    .expect("downgrade to official v17 shape");
    Database::set_user_version(conn, 17).expect("set user_version=17");
}

/// 判定某表是否存在名为 column 的列（缺失即视为「旧结构」）。
fn sync_table_has_byte_cursor_columns(conn: &Connection) -> bool {
    Database::has_column(conn, "session_log_sync", "last_byte_offset").unwrap_or(false)
        && Database::has_column(conn, "session_log_sync", "last_tail_fingerprint").unwrap_or(false)
}

fn shared_key_pool_tables_exist(conn: &Connection) -> bool {
    [
        "shared_key_groups",
        "shared_api_keys",
        "provider_shared_key_links",
    ]
    .iter()
    .all(|table| Database::table_exists(conn, table).unwrap_or(false))
}

#[test]
fn v18_migration_fresh_database_has_cursor_columns_and_pool() {
    let conn = Connection::open_in_memory().expect("open db");
    Database::create_tables_on_conn(&conn).expect("create schema");
    Database::apply_schema_migrations_on_conn(&conn).expect("migrate fresh db");

    assert_eq!(
        Database::get_user_version(&conn).expect("version"),
        SCHEMA_VERSION
    );
    assert_eq!(SCHEMA_VERSION, 18, "本轮方案固定为 18（不占用官方未来 19）");
    assert!(sync_table_has_byte_cursor_columns(&conn));
    assert!(shared_key_pool_tables_exist(&conn));
    assert!(
        Database::shared_key_pool_marker_present(&conn).expect("marker"),
        "全新库首次也应完成池初始化并写入 marker"
    );
}

#[test]
fn v18_migration_from_official_v17_builds_pool_and_cursor_columns() {
    let conn = Connection::open_in_memory().expect("open db");
    make_official_v17_like(&conn);
    assert!(
        !shared_key_pool_tables_exist(&conn),
        "官方 v17 夹具必须没有池表"
    );

    Database::apply_schema_migrations_on_conn(&conn).expect("migrate official v17");

    assert_eq!(Database::get_user_version(&conn).expect("version"), 18);
    assert!(sync_table_has_byte_cursor_columns(&conn));
    assert!(shared_key_pool_tables_exist(&conn));
    assert!(
        Database::table_exists(&conn, "session_usage_dedup").expect("dedup"),
        "官方 v17 夹具自带去重账本，迁移后必须仍在"
    );
    assert!(
        Database::shared_key_pool_marker_present(&conn).expect("marker"),
        "官方 v18 盖章库建池后必须写 marker，避免下次启动重跑数据迁移"
    );
}

#[test]
fn v18_migration_from_local_v17_preserves_pool_and_selection() {
    let conn = Connection::open_in_memory().expect("open db");
    make_official_v17_like(&conn);
    // 官方 v17 夹具此刻无池表：先建池并塞入数据，再退回 v17 版本号，模拟
    // 「本地魔改版已建池、随后并入官方会话游标列」的形态。
    Database::create_shared_key_tables_on_conn_for_test(&conn);
    conn.execute_batch(
        "INSERT INTO shared_key_groups (id, created_at) VALUES ('g1', 100);
         INSERT INTO shared_api_keys (id, group_id, label, key_value, sort_index)
         VALUES ('k1', 'g1', 'main', 'sk-keep', 0), ('k2', 'g1', 'spare', 'sk-spare', 1);
         INSERT INTO providers (id, app_type, name, settings_config, meta)
         VALUES ('p1', 'claude', 'P1', '{\"env\":{\"ANTHROPIC_BASE_URL\":\"https://a.example\"}}',
                 '{\"selectedKeyId\":\"k2\"}');
         INSERT INTO provider_shared_key_links (provider_id, app_type, group_id)
         VALUES ('p1', 'claude', 'g1');",
    )
    .expect("seed local pool");
    Database::set_user_version(&conn, 17).expect("set user_version=17");

    Database::apply_schema_migrations_on_conn(&conn).expect("migrate local v17");

    assert_eq!(Database::get_user_version(&conn).expect("version"), 18);
    assert!(sync_table_has_byte_cursor_columns(&conn));
    let keys: Vec<(String, String, i64)> = {
        let mut stmt = conn
            .prepare("SELECT id, key_value, sort_index FROM shared_api_keys ORDER BY id")
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect")
    };
    assert_eq!(
        keys,
        vec![
            ("k1".to_string(), "sk-keep".to_string(), 0),
            ("k2".to_string(), "sk-spare".to_string(), 1),
        ],
        "本地 v17 的池数据必须逐值保真（含 label 之外的排序）"
    );
    let selected: String = conn
        .query_row(
            "SELECT json_extract(meta, '$.selectedKeyId') FROM providers WHERE id = 'p1'",
            [],
            |r| r.get(0),
        )
        .expect("selected key");
    assert_eq!(selected, "k2");
    let provider_meta: String = conn
        .query_row("SELECT meta FROM providers WHERE id = 'p1'", [], |r| {
            r.get(0)
        })
        .expect("meta");
    assert!(
        !provider_meta.contains("apiKeys"),
        "池已是唯一真相，provider meta 不应再存 Key 列表: {provider_meta}"
    );
}

#[test]
fn v18_migration_repairs_dedup_table_without_touching_pool() {
    // 本地旧 v17：有池数据、缺官方去重账本（两侧都用过 v17 的冲突形态）。
    let conn = Connection::open_in_memory().expect("open db");
    Database::create_tables_on_conn(&conn).expect("create current schema");
    conn.execute_batch(
        "DROP TABLE IF EXISTS session_usage_dedup;
         INSERT INTO shared_key_groups (id, created_at) VALUES ('g9', 5);
         INSERT INTO shared_api_keys (id, group_id, label, key_value, sort_index)
         VALUES ('k9', 'g9', '', 'sk-old', 0);",
    )
    .expect("seed local legacy v17");
    Database::set_user_version(&conn, 17).expect("set user_version=17");

    Database::apply_schema_migrations_on_conn(&conn).expect("migrate legacy local v17");

    assert!(Database::table_exists(&conn, "session_usage_dedup").expect("dedup"));
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM shared_api_keys", [], |r| r.get(0))
        .expect("pool count");
    assert_eq!(count, 1, "补建官方表不得动池数据");
}

#[test]
fn v18_migration_from_local_v12_runs_continuous_chain() {
    let conn = Connection::open_in_memory().expect("open db");
    make_official_v17_like(&conn);
    conn.execute_batch("DROP TABLE IF EXISTS session_usage_dedup;")
        .expect("drop dedup for v12 shape");
    Database::set_user_version(&conn, 12).expect("set user_version=12");

    Database::apply_schema_migrations_on_conn(&conn).expect("migrate v12 -> v18");

    assert_eq!(Database::get_user_version(&conn).expect("version"), 18);
    assert!(Database::table_exists(&conn, "session_usage_dedup").expect("dedup"));
    assert!(sync_table_has_byte_cursor_columns(&conn));
    assert!(shared_key_pool_tables_exist(&conn));
}

#[test]
fn v18_migration_from_official_v16_runs_continuous_chain() {
    let conn = Connection::open_in_memory().expect("open db");
    make_official_v17_like(&conn);
    conn.execute_batch("DROP TABLE IF EXISTS session_usage_dedup;")
        .expect("drop dedup for v16 shape");
    Database::set_user_version(&conn, 16).expect("set user_version=16");

    Database::apply_schema_migrations_on_conn(&conn).expect("migrate v16 -> v18");

    assert_eq!(Database::get_user_version(&conn).expect("version"), 18);
    assert!(Database::table_exists(&conn, "session_usage_dedup").expect("dedup"));
    assert!(shared_key_pool_tables_exist(&conn));
    assert!(Database::shared_key_pool_marker_present(&conn).expect("marker"));
}

#[test]
fn v18_migration_recovers_from_interrupted_state() {
    // 中断态：池表已建、marker 未写、version 未更新。
    let conn = Connection::open_in_memory().expect("open db");
    make_official_v17_like(&conn);
    Database::create_shared_key_tables_on_conn_for_test(&conn);
    conn.execute_batch(
        "INSERT INTO providers (id, app_type, name, settings_config, meta)
         VALUES ('p2', 'codex', 'P2', '{\"auth\":{}}', '{\"apiKeys\":[{\"id\":\"kx\",\"label\":\"\",\"key\":\"sk-x\"}],\"selectedKeyId\":\"kx\"}');",
    )
    .expect("seed interrupted provider");
    Database::set_user_version(&conn, 17).expect("set user_version=17");

    Database::apply_schema_migrations_on_conn(&conn).expect("recover interrupted migration");

    assert_eq!(Database::get_user_version(&conn).expect("version"), 18);
    assert!(
        Database::shared_key_pool_marker_present(&conn).expect("marker"),
        "中断态的二次迁移必须完成初始化并盖章"
    );
    let keys: i64 = conn
        .query_row("SELECT COUNT(*) FROM shared_api_keys", [], |r| r.get(0))
        .expect("pool count");
    assert_eq!(keys, 1, "残留的 provider Key 应被收进中央池");
    let links: i64 = conn
        .query_row("SELECT COUNT(*) FROM provider_shared_key_links", [], |r| {
            r.get(0)
        })
        .expect("link count");
    assert_eq!(links, 1);
}

#[test]
fn v18_repair_applies_cursor_columns_to_stamped_local_v18() {
    // 实机等价形态：本地语义 18 已盖章、池完整并带 marker、缺官方字节游标列。
    // 版本循环一步都不会进，必须靠版本无关的 ensure 兜底。
    let conn = Connection::open_in_memory().expect("open db");
    Database::create_tables_on_conn(&conn).expect("create current schema");
    conn.execute_batch(
        "DROP TABLE session_log_sync;
         CREATE TABLE session_log_sync (
            file_path TEXT PRIMARY KEY,
            last_modified INTEGER NOT NULL,
            last_line_offset INTEGER NOT NULL DEFAULT 0,
            last_synced_at INTEGER NOT NULL
         );
         INSERT INTO session_log_sync VALUES ('/tmp/a.jsonl', 11, 7, 3);
         INSERT INTO shared_key_groups (id, created_at) VALUES ('g-machine', 1);
         INSERT INTO shared_api_keys (id, group_id, label, key_value, sort_index)
         VALUES ('km', 'g-machine', 'machine', 'sk-machine', 0);
         INSERT INTO providers (id, app_type, name, settings_config, meta)
         VALUES ('pm', 'claude', 'PM', '{\"env\":{}}', '{\"selectedKeyId\":\"km\"}');
         INSERT INTO provider_shared_key_links (provider_id, app_type, group_id)
         VALUES ('pm', 'claude', 'g-machine');
         INSERT OR REPLACE INTO settings (key, value)
         VALUES ('shared_key_pool_migrated_v18', 'true');",
    )
    .expect("seed stamped local v18");
    // 关键：先删除两个字节游标列不存在——此处直接以旧 DDL 建表即已完成。
    Database::set_user_version(&conn, 18).expect("set user_version=18");

    Database::apply_schema_migrations_on_conn(&conn).expect("equal-version repair");

    assert_eq!(Database::get_user_version(&conn).expect("version"), 18);
    assert!(
        sync_table_has_byte_cursor_columns(&conn),
        "等版本修复必须补上官方字节游标列"
    );
    let existing_offset: Option<i64> = conn
        .query_row(
            "SELECT last_byte_offset FROM session_log_sync WHERE file_path = '/tmp/a.jsonl'",
            [],
            |r| r.get(0),
        )
        .expect("row preserved");
    assert!(
        existing_offset.is_none(),
        "存量行保持 NULL，由扫描层按旧行号游标转换"
    );
    let keys: i64 = conn
        .query_row("SELECT COUNT(*) FROM shared_api_keys", [], |r| r.get(0))
        .expect("pool count");
    assert_eq!(keys, 1, "已有池数据不得被重写或清空");
    let label: String = conn
        .query_row(
            "SELECT label FROM shared_api_keys WHERE id = 'km'",
            [],
            |r| r.get(0),
        )
        .expect("label preserved");
    assert_eq!(label, "machine");
}

#[test]
fn v18_repair_claims_complete_pool_without_resurrecting_deleted_keys() {
    // 完整已有池、无 marker，且 provider 的 settings_config 里已经删掉了旧 Key：
    // 只补 marker，不重跑 reconcile（旧 Key 不得复活，池行与 meta 必须原样）。
    let conn = Connection::open_in_memory().expect("open db");
    Database::create_tables_on_conn(&conn).expect("create current schema");
    conn.execute_batch(
        "INSERT INTO shared_key_groups (id, created_at) VALUES ('g2', 7);
         INSERT INTO shared_api_keys (id, group_id, label, key_value, sort_index)
         VALUES ('k20', 'g2', 'kept', 'sk-kept', 0);
         INSERT INTO providers (id, app_type, name, settings_config, meta)
         VALUES ('p20', 'claude', 'P20', '{\"env\":{\"ANTHROPIC_AUTH_TOKEN\":\"\"}}',
                 '{\"selectedKeyId\":\"k20\",\"customUserAgent\":\"ua-keep\"}');",
    )
    .expect("seed complete pool without marker");
    Database::set_user_version(&conn, 18).expect("set user_version=18");

    Database::apply_schema_migrations_on_conn(&conn).expect("claim existing pool");

    assert!(Database::shared_key_pool_marker_present(&conn).expect("marker"));
    let keys: Vec<(String, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, key_value FROM shared_api_keys ORDER BY id")
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect")
    };
    assert_eq!(keys, vec![("k20".to_string(), "sk-kept".to_string())]);
    let meta: String = conn
        .query_row("SELECT meta FROM providers WHERE id = 'p20'", [], |r| {
            r.get(0)
        })
        .expect("meta");
    assert!(
        meta.contains("ua-keep"),
        "provider 原始 meta 必须逐值保留: {meta}"
    );
    assert!(
        !meta.contains("sk-kept"),
        "只补 marker 的路径不得把池 Key 写回 provider meta: {meta}"
    );
}

#[test]
fn v18_repair_rejects_marker_without_pool_structure() {
    let conn = Connection::open_in_memory().expect("open db");
    Database::create_tables_on_conn(&conn).expect("create current schema");
    conn.execute_batch(
        "DROP TABLE IF EXISTS provider_shared_key_links;
         DROP TABLE IF EXISTS shared_api_keys;
         DROP TABLE IF EXISTS shared_key_groups;
         INSERT OR REPLACE INTO settings (key, value)
         VALUES ('shared_key_pool_migrated_v18', 'true');",
    )
    .expect("seed contradictory marker");
    Database::set_user_version(&conn, 18).expect("set user_version=18");

    let error = Database::apply_schema_migrations_on_conn(&conn)
        .expect_err("marker 与结构矛盾时必须拒绝自动修复");
    assert!(
        error.to_string().contains("共享 Key 池"),
        "错误信息应可定位到共享 Key 池: {error}"
    );
    assert!(
        !shared_key_pool_tables_exist(&conn),
        "拒绝后不得补出空池表掩盖损坏"
    );
}

#[test]
fn v18_repair_rejects_inconsistent_pool_and_rolls_back() {
    // 池有数据但关联指向不存在的分组：明确报错，且 savepoint 回滚到迁移前状态。
    let conn = Connection::open_in_memory().expect("open db");
    Database::create_tables_on_conn(&conn).expect("create current schema");
    conn.execute_batch(
        "DROP TABLE session_log_sync;
         CREATE TABLE session_log_sync (
            file_path TEXT PRIMARY KEY,
            last_modified INTEGER NOT NULL,
            last_line_offset INTEGER NOT NULL DEFAULT 0,
            last_synced_at INTEGER NOT NULL
         );
         PRAGMA foreign_keys = OFF;
         INSERT INTO shared_key_groups (id, created_at) VALUES ('g3', 1);
         INSERT INTO shared_api_keys (id, group_id, label, key_value, sort_index)
         VALUES ('k3', 'g3', '', 'sk-3', 0);
         INSERT INTO providers (id, app_type, name, settings_config, meta)
         VALUES ('p3', 'claude', 'P3', '{\"env\":{}}', '{}');
         INSERT INTO provider_shared_key_links (provider_id, app_type, group_id)
         VALUES ('p3', 'claude', 'g-missing');",
    )
    .expect("seed dangling link");
    Database::set_user_version(&conn, 18).expect("set user_version=18");

    let error = Database::apply_schema_migrations_on_conn(&conn)
        .expect_err("悬空关联必须报错而不是重建池");
    let message = error.to_string();
    assert!(
        message.contains("不存在的分组") || message.contains("meta 无法解析"),
        "悬空关联必须被识别为池状态异常: {message}"
    );
    assert!(
        !sync_table_has_byte_cursor_columns(&conn),
        "迁移 savepoint 内的补列必须随失败一起回滚"
    );
    let keys: i64 = conn
        .query_row("SELECT COUNT(*) FROM shared_api_keys", [], |r| r.get(0))
        .expect("pool preserved");
    assert_eq!(keys, 1, "用户池不得被清理或覆盖");
}

#[test]
fn v18_repair_skips_marker_for_partial_schema_fixture() {
    // 上游迁移单测会构造没有 providers 表的部分 schema：可以补结构，但不能写 marker，
    // 否则该库会永久跳过后续的池初始化。
    let conn = Connection::open_in_memory().expect("open db");
    conn.execute_batch(
        "CREATE TABLE session_log_sync (
            file_path TEXT PRIMARY KEY,
            last_modified INTEGER NOT NULL,
            last_line_offset INTEGER NOT NULL DEFAULT 0,
            last_synced_at INTEGER NOT NULL
         );
         CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);",
    )
    .expect("create partial schema");
    Database::set_user_version(&conn, 17).expect("set user_version=17");

    Database::apply_schema_migrations_on_conn(&conn).expect("migrate partial schema");

    assert_eq!(Database::get_user_version(&conn).expect("version"), 18);
    assert!(sync_table_has_byte_cursor_columns(&conn));
    assert!(
        !Database::shared_key_pool_marker_present(&conn).expect("marker"),
        "没有 providers 表时不得盖章"
    );

    // 之后补齐 providers 表，下一次启动仍应完成池初始化并盖章。
    Database::create_tables_on_conn(&conn).expect("complete schema");
    Database::apply_schema_migrations_on_conn(&conn).expect("second pass");
    assert!(
        Database::shared_key_pool_marker_present(&conn).expect("marker"),
        "补齐 providers 后必须继续执行数据迁移并盖章"
    );
}

#[test]
fn v18_repair_preserves_non_text_provider_configs() {
    let conn = Connection::open_in_memory().expect("open db");
    make_official_v17_like(&conn);
    conn.execute_batch(
        "INSERT INTO providers (id, app_type, name, settings_config, meta)
         VALUES ('blob', 'claude', 'B', x'00ff01', '{}');",
    )
    .expect("seed blob config");

    Database::apply_schema_migrations_on_conn(&conn).expect("migrate with blob row");

    let value_type: String = conn
        .query_row(
            "SELECT typeof(settings_config) FROM providers WHERE id = 'blob'",
            [],
            |r| r.get(0),
        )
        .expect("typeof");
    assert_eq!(value_type, "blob", "非 TEXT 配置必须原样保留");
    let blob: Vec<u8> = conn
        .query_row(
            "SELECT settings_config FROM providers WHERE id = 'blob'",
            [],
            |r| r.get(0),
        )
        .expect("blob value");
    assert_eq!(blob, vec![0x00, 0xff, 0x01]);
}

#[test]
fn v18_repair_detection_and_backup_gate() {
    // 结构探测 + 备份门禁的纯判定：有用户表的存量库在需要改写前必须先备份。
    assert_eq!(
        Database::safety_backup_reason(17, 18, false, true, false),
        None
    );
    assert_eq!(
        Database::safety_backup_reason(17, 18, true, true, false).as_deref(),
        Some("v17 → v18")
    );
    assert_eq!(
        Database::safety_backup_reason(18, 18, true, false, true).as_deref(),
        Some("v18 结构修复")
    );
    assert_eq!(
        Database::safety_backup_reason(18, 18, true, false, false),
        None
    );

    // 缺字节游标列的盖章 v18 库必须被判定为「需要修复」。
    let conn = Connection::open_in_memory().expect("open db");
    Database::create_tables_on_conn(&conn).expect("create schema");
    conn.execute_batch(
        "DROP TABLE session_log_sync;
         CREATE TABLE session_log_sync (
            file_path TEXT PRIMARY KEY,
            last_modified INTEGER NOT NULL,
            last_line_offset INTEGER NOT NULL DEFAULT 0,
            last_synced_at INTEGER NOT NULL
         );
         INSERT OR REPLACE INTO settings (key, value)
         VALUES ('shared_key_pool_migrated_v18', 'true');
         CREATE TABLE shared_key_groups (id TEXT PRIMARY KEY, created_at INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE shared_api_keys (
            id TEXT PRIMARY KEY, group_id TEXT NOT NULL, label TEXT NOT NULL DEFAULT '',
            key_value TEXT NOT NULL, sort_index INTEGER NOT NULL DEFAULT 0,
            UNIQUE(group_id, key_value)
         );
         CREATE TABLE provider_shared_key_links (
            provider_id TEXT NOT NULL, app_type TEXT NOT NULL, group_id TEXT NOT NULL,
            PRIMARY KEY (provider_id, app_type)
         );",
    )
    .expect("seed stamped v18 missing cursor columns");
    assert!(
        Database::schema_needs_v18_repair(&conn).expect("detect repair"),
        "缺字节游标列的 v18 库必须被识别为需要等版本修复"
    );

    // 结构完整且已盖章时不触发修复，也不需要备份。
    let done = Connection::open_in_memory().expect("open db");
    Database::create_tables_on_conn(&done).expect("create schema");
    Database::apply_schema_migrations_on_conn(&done).expect("migrate");
    assert!(
        !Database::schema_needs_v18_repair(&done).expect("detect repair"),
        "完整且带 marker 的库不应每次启动都触发备份与修复"
    );
}

#[test]
fn v18_repair_sql_round_trip_keeps_marker_and_pool() {
    let db = Database::memory().expect("memory db");
    {
        let conn = lock_conn!(db.conn);
        conn.execute_batch(
            "INSERT INTO shared_key_groups (id, created_at) VALUES ('gs', 3);
             INSERT INTO shared_api_keys (id, group_id, label, key_value, sort_index)
             VALUES ('ks', 'gs', 'roundtrip', 'sk-roundtrip', 0);
             INSERT INTO providers (id, app_type, name, settings_config, meta)
             VALUES ('ps', 'codex', 'PS', '{\"auth\":{}}', '{\"selectedKeyId\":\"ks\"}');
             INSERT INTO provider_shared_key_links (provider_id, app_type, group_id)
             VALUES ('ps', 'codex', 'gs');",
        )
        .expect("seed pool");
        Database::apply_schema_migrations_on_conn(&conn).expect("stamp marker");
        assert!(Database::shared_key_pool_marker_present(&conn).expect("marker"));
    }

    let exported = db.export_sql_string_for_sync().expect("export sql");
    assert!(exported.contains("shared_api_keys"));
    assert!(exported.contains("shared_key_pool_migrated_v18"));

    // 源库导出后保持不变，导入到一个独立主库后再校验 marker 与池一起往返。
    db.import_sql_string(&exported).expect("import sql");

    let conn = lock_conn!(db.conn);
    assert!(
        Database::shared_key_pool_marker_present(&conn).expect("marker after import"),
        "marker 随 settings 一起往返，导入后不得丢失"
    );
    let key: String = conn
        .query_row(
            "SELECT key_value FROM shared_api_keys WHERE id = 'ks'",
            [],
            |r| r.get(0),
        )
        .expect("pool key after import");
    assert_eq!(key, "sk-roundtrip");
    assert!(Database::table_exists(&conn, "session_log_sync").expect("table"));
}

#[test]
fn v18_repair_rejects_future_version_before_writing() {
    let conn = Connection::open_in_memory().expect("open db");
    Database::create_tables_on_conn(&conn).expect("create schema");
    conn.execute_batch(
        "DROP TABLE session_log_sync;
         CREATE TABLE session_log_sync (
            file_path TEXT PRIMARY KEY,
            last_modified INTEGER NOT NULL,
            last_line_offset INTEGER NOT NULL DEFAULT 0,
            last_synced_at INTEGER NOT NULL
         );",
    )
    .expect("downgrade structure");
    Database::set_user_version(&conn, 19).expect("set future version");

    let error = Database::apply_schema_migrations_on_conn(&conn)
        .expect_err("version > SCHEMA_VERSION 必须拒绝");
    assert!(error.to_string().contains("版本过新"), "错误信息: {error}");
    assert!(
        !sync_table_has_byte_cursor_columns(&conn),
        "版本过新时不得执行任何结构修复"
    );
}
