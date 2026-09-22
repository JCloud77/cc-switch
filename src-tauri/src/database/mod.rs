//! 数据库模块 - SQLite 数据持久化
//!
//! 此模块提供应用的核心数据存储功能，包括：
//! - 供应商配置管理
//! - MCP 服务器配置
//! - 提示词管理
//! - Skills 管理
//! - 通用设置存储
//!
//! ## 架构设计
//!
//! ```text
//! database/
//! ├── mod.rs        - Database 结构体 + 初始化
//! ├── schema.rs     - 表结构定义 + Schema 迁移
//! ├── backup.rs     - SQL 导入导出 + 快照备份
//! ├── migration.rs  - JSON → SQLite 数据迁移
//! └── dao/          - 数据访问对象
//!     ├── providers.rs
//!     ├── mcp.rs
//!     ├── prompts.rs
//!     ├── skills.rs
//!     └── settings.rs
//! ```

pub(crate) mod backup;
mod dao;
mod migration;
mod schema;

#[cfg(test)]
mod tests;

// DAO 类型导出供外部使用
pub(crate) use dao::providers_seed::{
    is_official_seed_id, CLAUDE_DESKTOP_OFFICIAL_PROVIDER_ID, CODEX_OFFICIAL_PROVIDER_ID,
    GROKBUILD_OFFICIAL_PROVIDER_ID,
};
pub(crate) use dao::proxy::{
    validate_cost_multiplier, validate_pricing_source, PRICING_SOURCE_REQUEST,
    PRICING_SOURCE_RESPONSE,
};
pub use dao::FailoverQueueItem;
pub use dao::Profile;

use crate::config::get_app_config_dir;
use crate::error::AppError;
use rusqlite::{hooks::Action, Connection};
use schema::SharedKeyPoolState;
use serde::Serialize;
use std::sync::Mutex;

// DAO 方法通过 impl Database 提供，无需额外导出

/// 当前 Schema 版本号
/// 每次修改表结构时递增，并在 schema.rs 中添加相应的迁移逻辑
pub(crate) const SCHEMA_VERSION: i32 = 18;

/// 安全地序列化 JSON，避免 unwrap panic
pub(crate) fn to_json_string<T: Serialize>(value: &T) -> Result<String, AppError> {
    serde_json::to_string(value)
        .map_err(|e| AppError::Config(format!("JSON serialization failed: {e}")))
}

/// 安全地获取 Mutex 锁，避免 unwrap panic
macro_rules! lock_conn {
    ($mutex:expr) => {
        $mutex
            .lock()
            .map_err(|e| AppError::Database(format!("Mutex lock failed: {}", e)))?
    };
}

// 导出宏供子模块使用
pub(crate) use lock_conn;

/// 数据库连接封装
///
/// 使用 Mutex 包装 Connection 以支持在多线程环境（如 Tauri State）中共享。
/// rusqlite::Connection 本身不是 Sync 的，因此需要这层包装。
pub struct Database {
    pub(crate) conn: Mutex<Connection>,
}

fn register_db_change_hook(conn: &Connection) {
    conn.update_hook(Some(
        |action: Action, _database: &str, table: &str, _row_id: i64| match action {
            Action::SQLITE_INSERT | Action::SQLITE_UPDATE | Action::SQLITE_DELETE => {
                crate::services::webdav_auto_sync::notify_db_changed(table);
                crate::services::s3_auto_sync::notify_db_changed(table);
            }
            _ => {}
        },
    ));
}

impl Database {
    /// 初始化数据库连接并创建表
    ///
    /// 数据库文件位于 `~/.cc-switch/cc-switch.db`
    ///
    /// 顺序要求（数据保护）：先只读探测版本与结构，再按门禁生成一次安全备份，
    /// **最后**才执行 `create_tables` 与迁移。若先建表/迁移再备份，备份里已经包含
    /// 部分修复，失去回滚价值；而 v18 的「等版本结构修复」（实机库已被本地魔改版
    /// 盖章为 `user_version = 18` 但缺官方字节游标列）同样会改写存量库，必须一起
    /// 纳入前置备份。
    pub fn init() -> Result<Self, AppError> {
        let db_path = get_app_config_dir().join("cc-switch.db");
        let db_exists = db_path.exists();

        // 确保父目录存在
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
        }

        let conn = Connection::open(&db_path).map_err(|e| AppError::Database(e.to_string()))?;

        // 启用外键约束
        conn.execute("PRAGMA foreign_keys = ON;", [])
            .map_err(|e| AppError::Database(e.to_string()))?;
        if !db_exists {
            // For a brand-new database, configure incremental auto-vacuum
            // before creating any tables so no rebuild is needed later.
            conn.execute("PRAGMA auto_vacuum = INCREMENTAL;", [])
                .map_err(|e| AppError::Database(e.to_string()))?;
        }
        register_db_change_hook(&conn);

        let db = Self {
            conn: Mutex::new(conn),
        };

        // 备份门禁所需的只读探测：必须在 create_tables / 迁移之前完成。
        let backup_reason = {
            let conn = lock_conn!(db.conn);
            let version = Self::get_user_version(&conn)?;
            if version > SCHEMA_VERSION {
                return Err(AppError::Database(format!(
                    "数据库版本过新（{version}），当前应用仅支持 {SCHEMA_VERSION}，请升级应用后再尝试。"
                )));
            }
            let has_user_tables = Self::has_user_tables(&conn)?;
            // 存量残缺库门禁 + 池迁移标记与原始池结构的矛盾判定，都必须在建表之前完成。
            Self::ensure_core_tables_before_write(&conn)?;
            Self::ensure_v18_pool_marker_consistent_with_raw_state(&conn)?;
            let needs_migration =
                Self::needs_migration_backup(version, SCHEMA_VERSION, has_user_tables);
            let needs_v18_repair = Self::schema_needs_v18_repair(&conn)?;
            Self::safety_backup_reason(
                version,
                SCHEMA_VERSION,
                has_user_tables,
                needs_migration,
                needs_v18_repair,
            )
        };

        if let Some(reason) = backup_reason {
            log::info!("Creating pre-write database safety backup ({reason})");
            match db.backup_database_file() {
                Ok(Some(path)) => {
                    log::info!("Safety backup created at {}", path.display());
                }
                Ok(None) => {
                    return Err(AppError::Database(
                        "数据库已存在但没有可备份的主库文件，已停止后续建表与迁移以保护用户数据"
                            .to_string(),
                    ));
                }
                Err(e) => {
                    return Err(AppError::Database(format!(
                        "安全备份失败（{e}），已停止后续建表与迁移以保护用户数据"
                    )));
                }
            }
        }

        db.create_tables()?;
        db.apply_schema_migrations()?;
        if let Err(e) = db.ensure_incremental_auto_vacuum() {
            log::warn!("Failed to ensure incremental auto-vacuum: {e}");
        }
        db.ensure_model_pricing_seeded()?;
        if let Err(e) = crate::services::model_pricing::sync_local_model_pricing(&db) {
            log::warn!("Failed to sync local model pricing file: {e}");
        }

        // Startup cleanup: prune old logs and reclaim space
        if let Err(e) = db.cleanup_old_stream_check_logs(7) {
            log::warn!("Startup stream_check_logs cleanup failed: {e}");
        }
        if let Err(e) = db.rollup_and_prune(30) {
            log::warn!("Startup rollup_and_prune failed: {e}");
        }
        // Reclaim disk space after cleanup
        {
            let conn = lock_conn!(db.conn);
            if let Err(e) = conn.execute_batch("PRAGMA incremental_vacuum;") {
                log::warn!("Startup incremental vacuum failed: {e}");
            }
        }

        Ok(db)
    }

    /// 判断本次启动是否必须先做安全备份，并给出原因。
    ///
    /// 纯函数，便于单测覆盖门禁：只有「库里已有用户表」且「即将改写存量数据」
    /// （低版本迁移或 v18 等版本结构修复）才需要备份；全新空库不做无意义备份。
    pub(crate) fn safety_backup_reason(
        version: i32,
        schema_version: i32,
        has_user_tables: bool,
        needs_migration: bool,
        needs_v18_repair: bool,
    ) -> Option<String> {
        if !has_user_tables {
            return None;
        }
        if needs_migration {
            return Some(format!("v{version} → v{schema_version}"));
        }
        if needs_v18_repair {
            return Some(format!("v{schema_version} 结构修复"));
        }
        None
    }

    /// 建表前的核心表门禁。
    ///
    /// 「有用户表但没有 core 表 providers」= 存量残缺库（表被外部工具删掉、或从半截备份恢复）。
    /// 这种库必须在 `create_tables` 之前拒绝：建表会把 providers 补成空表，之后启动路径再也
    /// 看不到数据缺失，用户会在「一切正常」的假象里丢掉全部供应商配置。
    ///
    /// 全新空库（一张表都没有）不在此列，它本来就是待初始化的。
    pub(crate) fn ensure_core_tables_before_write(conn: &Connection) -> Result<(), AppError> {
        if !Self::has_user_tables(conn)? {
            return Ok(());
        }
        if Self::table_exists(conn, "providers")? {
            return Ok(());
        }
        Err(AppError::Database(
            "数据库存在用户表但缺少核心表 providers；已停止建表与迁移以免用空表掩盖数据缺失，请从备份恢复该数据库"
                .to_string(),
        ))
    }

    /// 是否需要「低版本迁移」备份。
    ///
    /// `version == 0` 但库里已有用户表同样是即将被改写的存量数据——迁移链会从 v0 逐级跑到
    /// 当前版本，不能因为版本号是 0 就当成新库跳过备份。
    pub(crate) fn needs_migration_backup(
        version: i32,
        schema_version: i32,
        has_user_tables: bool,
    ) -> bool {
        version < schema_version && (version > 0 || has_user_tables)
    }

    /// 只读探测：v18 结构是否缺失，需要「等版本修复」。
    ///
    /// 覆盖缺 `session_log_sync` 表、缺字节游标列、缺去重账本、缺共享 Key 池
    /// 表/必需列、缺有效 marker，以及池状态异常（异常同样按「需要处理」返回 true，
    /// 由迁移阶段给出明确错误——但备份先落地，避免用户失去回滚点）。
    pub(crate) fn schema_needs_v18_repair(conn: &Connection) -> Result<bool, AppError> {
        if !Self::has_user_tables(conn)? {
            return Ok(false);
        }
        // 核心表缺失（例如只有 settings 的残缺库）：`init` 的只读门禁会直接拒绝启动，
        // 这里保持「不视为 v18 修复项」，避免在任何坏库上写入 marker。
        if !Self::table_exists(conn, "providers")? {
            return Ok(false);
        }

        if !Self::table_exists(conn, "session_log_sync")? {
            return Ok(true);
        }
        if !Self::has_column(conn, "session_log_sync", "last_byte_offset")?
            || !Self::has_column(conn, "session_log_sync", "last_tail_fingerprint")?
        {
            return Ok(true);
        }
        if !Self::table_exists(conn, "session_usage_dedup")? {
            return Ok(true);
        }
        if !Self::shared_key_pool_columns_present(conn)? {
            return Ok(true);
        }
        if !Self::shared_key_pool_marker_present(conn)? {
            return Ok(true);
        }
        Ok(matches!(
            Self::classify_shared_key_pool_state(conn)?,
            SharedKeyPoolState::Inconsistent(_)
        ))
    }

    /// 读取磁盘上数据库的 `user_version`；仅当它比应用支持的 [`SCHEMA_VERSION`]
    /// 更新时返回 `Some(version)`。
    ///
    /// 用于初始化失败后判断是否为「数据库版本过新（应用过旧，需升级应用）」的可恢复
    /// 场景——此时不应反复弹出无效的重试对话框，而应引导用户在应用内升级。
    pub fn stored_user_version_exceeds_supported(
        db_path: &std::path::Path,
    ) -> Result<Option<i32>, AppError> {
        if !db_path.exists() {
            return Ok(None);
        }
        let conn = Connection::open(db_path).map_err(|e| AppError::Database(e.to_string()))?;
        let version = Self::get_user_version(&conn)?;
        Ok((version > SCHEMA_VERSION).then_some(version))
    }

    /// 创建内存数据库（用于测试）
    pub fn memory() -> Result<Self, AppError> {
        let conn = Connection::open_in_memory().map_err(|e| AppError::Database(e.to_string()))?;

        // 启用外键约束
        conn.execute("PRAGMA foreign_keys = ON;", [])
            .map_err(|e| AppError::Database(e.to_string()))?;
        conn.execute("PRAGMA auto_vacuum = INCREMENTAL;", [])
            .map_err(|e| AppError::Database(e.to_string()))?;
        register_db_change_hook(&conn);

        let db = Self {
            conn: Mutex::new(conn),
        };
        db.create_tables()?;
        db.ensure_model_pricing_seeded()?;

        Ok(db)
    }

    pub(crate) fn get_auto_vacuum_mode(conn: &Connection) -> Result<i32, AppError> {
        conn.query_row("PRAGMA auto_vacuum;", [], |row| row.get(0))
            .map_err(|e| AppError::Database(format!("读取 auto_vacuum 失败: {e}")))
    }

    fn has_user_tables(conn: &Connection) -> Result<bool, AppError> {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(format!("读取表数量失败: {e}")))?;
        Ok(count > 0)
    }

    pub(crate) fn ensure_incremental_auto_vacuum_on_conn(
        conn: &Connection,
    ) -> Result<bool, AppError> {
        let mode = Self::get_auto_vacuum_mode(conn)?;
        if mode == 2 {
            return Ok(false);
        }

        let has_tables = Self::has_user_tables(conn)?;
        conn.execute("PRAGMA auto_vacuum = INCREMENTAL;", [])
            .map_err(|e| AppError::Database(format!("设置 auto_vacuum 失败: {e}")))?;

        if !has_tables {
            return Ok(false);
        }

        conn.execute("VACUUM;", [])
            .map_err(|e| AppError::Database(format!("执行 VACUUM 失败: {e}")))?;
        conn.execute("PRAGMA foreign_keys = ON;", [])
            .map_err(|e| AppError::Database(format!("恢复 foreign_keys 失败: {e}")))?;
        Ok(true)
    }

    pub(crate) fn ensure_incremental_auto_vacuum(&self) -> Result<bool, AppError> {
        let mode = {
            let conn = lock_conn!(self.conn);
            Self::get_auto_vacuum_mode(&conn)?
        };
        if mode == 2 {
            return Ok(false);
        }

        let has_tables = {
            let conn = lock_conn!(self.conn);
            Self::has_user_tables(&conn)?
        };
        if has_tables {
            log::info!(
                "Detected auto_vacuum={mode}, rebuilding database to enable incremental vacuum"
            );
            self.backup_database_file()?;
        }

        let rebuilt = {
            let conn = lock_conn!(self.conn);
            Self::ensure_incremental_auto_vacuum_on_conn(&conn)?
        };

        if rebuilt {
            log::info!("Incremental auto-vacuum enabled after database rebuild");
        } else {
            log::info!("Incremental auto-vacuum configured for new database");
        }

        Ok(rebuilt)
    }

    /// 检查 MCP 服务器表是否为空
    pub fn is_mcp_table_empty(&self) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM mcp_servers", [], |row| row.get(0))
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(count == 0)
    }

    /// 检查提示词表是否为空
    pub fn is_prompts_table_empty(&self) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM prompts", [], |row| row.get(0))
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(count == 0)
    }
}
