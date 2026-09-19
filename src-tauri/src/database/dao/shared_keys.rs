use crate::database::Database;
use crate::error::AppError;
use crate::provider::{ApiKeyEntry, Provider, ProviderMeta};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use url::Url;

const SHARED_KEY_APPS: [&str; 2] = ["claude", "codex"];

#[derive(Debug, Clone)]
pub(crate) struct SharedKeySaveInput {
    pub keys: Vec<ApiKeyEntry>,
    pub selected_key_id: Option<String>,
    /// True only when the provider was loaded by a shared-key-aware client.
    /// This prevents older clients that omit `apiKeys` from clearing a pool.
    pub pool_loaded: bool,
}

#[derive(Debug, Clone)]
struct ProviderKeyRow {
    id: String,
    app_type: String,
    name: String,
    settings_config: Value,
    meta: ProviderMeta,
    group_id: Option<String>,
}

#[derive(Debug, Clone)]
struct StoredKey {
    id: String,
    group_id: String,
    label: String,
    key: String,
}

#[derive(Debug)]
struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u8>,
}

impl UnionFind {
    fn new(len: usize) -> Self {
        Self {
            parent: (0..len).collect(),
            rank: vec![0; len],
        }
    }

    fn find(&mut self, value: usize) -> usize {
        if self.parent[value] != value {
            self.parent[value] = self.find(self.parent[value]);
        }
        self.parent[value]
    }

    fn union(&mut self, left: usize, right: usize) {
        let left_root = self.find(left);
        let right_root = self.find(right);
        if left_root == right_root {
            return;
        }
        match self.rank[left_root].cmp(&self.rank[right_root]) {
            std::cmp::Ordering::Less => self.parent[left_root] = right_root,
            std::cmp::Ordering::Greater => self.parent[right_root] = left_root,
            std::cmp::Ordering::Equal => {
                self.parent[right_root] = left_root;
                self.rank[left_root] += 1;
            }
        }
    }
}

impl Database {
    pub(crate) fn app_supports_shared_keys(app_type: &str) -> bool {
        SHARED_KEY_APPS.contains(&app_type)
    }

    /// [`Self::hydrate_shared_keys_for_provider`] 的 Database 级入口。
    ///
    /// provider add/update 流程在保存后可能继续使用请求传入的原始 `Provider`（只有
    /// `selectedKeyId`，没有 `apiKeys`），因此构造 effective settings 前必须先按池
    /// hydrate。持锁期间只调用连接级 helper，不再调用会取同一 mutex 的 Database 方法；
    /// 数据库错误向上传递，不静默退回旧 Key。
    pub(crate) fn hydrate_shared_keys_for_provider_in_db(
        &self,
        app_type: &str,
        provider: &mut Provider,
    ) -> Result<(), AppError> {
        if !Self::app_supports_shared_keys(app_type) {
            return Ok(());
        }
        let conn = lock_conn!(self.conn);
        Self::hydrate_shared_keys_for_provider(&conn, app_type, provider)
    }

    /// Hydrate the central pool into the existing ProviderMeta wire shape so
    /// adapters and the frontend can keep using `apiKeys` without duplicating
    /// the list in every providers.meta row.
    pub(crate) fn hydrate_shared_keys_for_provider(
        conn: &Connection,
        app_type: &str,
        provider: &mut Provider,
    ) -> Result<(), AppError> {
        if !Self::app_supports_shared_keys(app_type) {
            return Ok(());
        }

        let group_id: Option<String> = conn
            .query_row(
                "SELECT group_id FROM provider_shared_key_links
                 WHERE provider_id = ?1 AND app_type = ?2",
                params![provider.id, app_type],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| AppError::Database(e.to_string()))?;
        let Some(group_id) = group_id else {
            return Ok(());
        };

        let meta = provider.meta.get_or_insert_with(ProviderMeta::default);
        let strategy = shared_key_strategy(app_type, meta, &provider.settings_config);
        let mut stmt = conn
            .prepare(
                "SELECT id, label, key_value FROM shared_api_keys
                 WHERE group_id = ?1 ORDER BY sort_index ASC, id ASC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let keys = stmt
            .query_map(params![group_id], |row| {
                Ok(ApiKeyEntry {
                    id: row.get(0)?,
                    label: row.get(1)?,
                    key: row.get(2)?,
                    strategy: Some(strategy.to_string()),
                })
            })
            .map_err(|e| AppError::Database(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut apps_stmt = conn
            .prepare(
                "SELECT DISTINCT app_type FROM provider_shared_key_links
                 WHERE group_id = ?1 ORDER BY app_type ASC",
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        let shared_apps = apps_stmt
            .query_map(params![group_id], |row| row.get::<_, String>(0))
            .map_err(|e| AppError::Database(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| AppError::Database(e.to_string()))?;

        meta.api_keys = keys;
        meta.shared_key_apps = shared_apps;
        meta.shared_key_pool_loaded = true;
        Ok(())
    }

    /// Reconcile the provider being saved with all Claude/Codex providers that
    /// share a normalized name OR root API domain. Existing links remain stable
    /// after a rename, while new matches merge their central pools by exact key.
    pub(crate) fn sync_shared_keys_after_provider_save(
        tx: &Transaction<'_>,
        provider_id: &str,
        app_type: &str,
        input: SharedKeySaveInput,
    ) -> Result<(), AppError> {
        if !Self::app_supports_shared_keys(app_type) {
            return Ok(());
        }

        let rows = load_provider_key_rows(tx)?;
        let Some(self_index) = rows
            .iter()
            .position(|row| row.id == provider_id && row.app_type == app_type)
        else {
            return Err(AppError::Database(format!(
                "Saved provider {app_type}/{provider_id} was not found"
            )));
        };
        let mut union_find = build_provider_components(&rows);
        let self_root = union_find.find(self_index);
        let component: Vec<usize> = (0..rows.len())
            .filter(|index| union_find.find(*index) == self_root)
            .collect();

        let existing_keys = load_all_stored_keys(tx)?;
        let self_group = rows[self_index].group_id.clone();
        let mut group_ids: Vec<String> = component
            .iter()
            .filter_map(|index| rows[*index].group_id.clone())
            .collect();
        group_ids.sort();
        group_ids.dedup();
        let target_group = self_group
            .clone()
            .or_else(|| group_ids.first().cloned())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let old_key_by_id = build_old_key_by_id(&rows, &existing_keys);
        let selection_values = build_selection_values(
            &rows,
            &component,
            &old_key_by_id,
            Some((self_index, &input)),
        );

        let authoritative = self_group.is_some() && input.pool_loaded;
        let mut final_keys = Vec::<ApiKeyEntry>::new();
        let mut key_indexes = HashMap::<String, usize>::new();
        let mut used_ids: HashSet<String> = existing_keys
            .iter()
            .filter(|key| !group_ids.contains(&key.group_id))
            .map(|key| key.id.clone())
            .collect();

        if authoritative {
            merge_api_keys(
                &mut final_keys,
                &mut key_indexes,
                &mut used_ids,
                input.keys.clone(),
            );
            merge_active_key(
                &mut final_keys,
                &mut key_indexes,
                &mut used_ids,
                &rows[self_index],
            );
        } else {
            merge_stored_keys(
                &mut final_keys,
                &mut key_indexes,
                &mut used_ids,
                existing_keys
                    .iter()
                    .filter(|key| group_ids.contains(&key.group_id)),
            );
            merge_api_keys(
                &mut final_keys,
                &mut key_indexes,
                &mut used_ids,
                input.keys.clone(),
            );
            merge_active_key(
                &mut final_keys,
                &mut key_indexes,
                &mut used_ids,
                &rows[self_index],
            );
        }

        // A newly connected group or an unlinked legacy provider contributes its
        // existing keys to the union. The already-loaded target pool is omitted
        // in authoritative mode so UI deletions remain effective.
        if authoritative {
            merge_stored_keys(
                &mut final_keys,
                &mut key_indexes,
                &mut used_ids,
                existing_keys.iter().filter(|key| {
                    group_ids.contains(&key.group_id)
                        && Some(key.group_id.as_str()) != self_group.as_deref()
                }),
            );
        }
        for index in &component {
            let row = &rows[*index];
            if !authoritative || row.group_id.as_deref() != self_group.as_deref() {
                merge_api_keys(
                    &mut final_keys,
                    &mut key_indexes,
                    &mut used_ids,
                    row.meta.api_keys.clone(),
                );
                merge_active_key(&mut final_keys, &mut key_indexes, &mut used_ids, row);
            }
        }

        if authoritative {
            let final_values: HashSet<&str> =
                final_keys.iter().map(|entry| entry.key.as_str()).collect();
            for index in &component {
                if *index == self_index {
                    continue;
                }
                if let Some(Some(selected_value)) = selection_values.get(index) {
                    if !final_values.contains(selected_value.as_str()) {
                        return Err(AppError::localized(
                            "shared_keys.delete_selected_by_other_provider",
                            format!(
                                "无法删除此 Key：它仍被 {} 中的供应商“{}”选中。请先在该供应商中切换 Key。",
                                rows[*index].app_type, rows[*index].name
                            ),
                            format!(
                                "Cannot delete this key because provider '{}' in {} still selects it. Switch that provider to another key first.",
                                rows[*index].name, rows[*index].app_type
                            ),
                        ));
                    }
                }
            }
        }

        apply_component_pool(
            tx,
            &rows,
            &component,
            &group_ids,
            &target_group,
            &final_keys,
            &selection_values,
        )?;
        Ok(())
    }

    /// v11 -> v12 data migration: centralize every existing Claude/Codex key
    /// list, merging connected providers by normalized name or root domain.
    pub(crate) fn migrate_provider_keys_to_shared_pools(conn: &Connection) -> Result<(), AppError> {
        let rows = load_provider_key_rows(conn)?;
        if rows.is_empty() {
            return Ok(());
        }
        let existing_keys = load_all_stored_keys(conn)?;
        let old_key_by_id = build_old_key_by_id(&rows, &existing_keys);
        let mut union_find = build_provider_components(&rows);
        let mut components = BTreeMap::<usize, Vec<usize>>::new();
        for index in 0..rows.len() {
            components
                .entry(union_find.find(index))
                .or_default()
                .push(index);
        }

        let mut assigned_ids = HashSet::<String>::new();
        for component in components.values() {
            let mut group_ids: Vec<String> = component
                .iter()
                .filter_map(|index| rows[*index].group_id.clone())
                .collect();
            group_ids.sort();
            group_ids.dedup();
            let target_group = group_ids
                .first()
                .cloned()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let mut final_keys = Vec::<ApiKeyEntry>::new();
            let mut key_indexes = HashMap::<String, usize>::new();
            let mut used_ids: HashSet<String> = existing_keys
                .iter()
                .filter(|key| !group_ids.contains(&key.group_id))
                .map(|key| key.id.clone())
                .chain(assigned_ids.iter().cloned())
                .collect();
            merge_stored_keys(
                &mut final_keys,
                &mut key_indexes,
                &mut used_ids,
                existing_keys
                    .iter()
                    .filter(|key| group_ids.contains(&key.group_id)),
            );
            for index in component {
                merge_api_keys(
                    &mut final_keys,
                    &mut key_indexes,
                    &mut used_ids,
                    rows[*index].meta.api_keys.clone(),
                );
                merge_active_key(
                    &mut final_keys,
                    &mut key_indexes,
                    &mut used_ids,
                    &rows[*index],
                );
            }
            let selection_values = build_selection_values(&rows, component, &old_key_by_id, None);
            apply_component_pool(
                conn,
                &rows,
                component,
                &group_ids,
                &target_group,
                &final_keys,
                &selection_values,
            )?;
            assigned_ids.extend(final_keys.iter().map(|entry| entry.id.clone()));
        }
        Ok(())
    }

    pub(crate) fn cleanup_orphaned_shared_key_groups(conn: &Connection) -> Result<(), AppError> {
        conn.execute(
            "DELETE FROM shared_key_groups
             WHERE NOT EXISTS (
                 SELECT 1 FROM provider_shared_key_links links
                 WHERE links.group_id = shared_key_groups.id
             )",
            [],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }
}

fn shared_key_strategy(app_type: &str, meta: &ProviderMeta, settings: &Value) -> &'static str {
    match app_type {
        "codex" => "bearer",
        "claude"
            if meta.api_key_field.as_deref() == Some("ANTHROPIC_AUTH_TOKEN")
                || (meta.api_key_field.is_none()
                    && settings
                        .pointer("/env/ANTHROPIC_AUTH_TOKEN")
                        .and_then(Value::as_str)
                        .is_some_and(|key| !key.trim().is_empty())) =>
        {
            "claude_auth"
        }
        "claude" => "anthropic",
        _ => "bearer",
    }
}

fn load_provider_key_rows(conn: &Connection) -> Result<Vec<ProviderKeyRow>, AppError> {
    // SQLite can preserve non-TEXT values even in TEXT-affinity columns (for example
    // a byte-for-byte SQL backup containing a BLOB). Such rows are not valid provider
    // JSON and must be preserved untouched rather than blocking the whole migration.
    let mut stmt = conn
        .prepare(
            "SELECT providers.id, providers.app_type, providers.name,
                    providers.settings_config, providers.meta, links.group_id
             FROM providers
             LEFT JOIN provider_shared_key_links links
               ON links.provider_id = providers.id
              AND links.app_type = providers.app_type
             WHERE providers.app_type IN ('claude', 'codex')
               AND typeof(providers.settings_config) = 'text'
               AND typeof(providers.meta) = 'text'
             ORDER BY providers.app_type ASC, providers.id ASC",
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mapped = stmt
        .query_map([], |row| {
            let settings_raw: String = row.get(3)?;
            let meta_raw: String = row.get(4)?;
            Ok(ProviderKeyRow {
                id: row.get(0)?,
                app_type: row.get(1)?,
                name: row.get(2)?,
                settings_config: serde_json::from_str(&settings_raw).unwrap_or(Value::Null),
                meta: serde_json::from_str(&meta_raw).unwrap_or_default(),
                group_id: row.get(5)?,
            })
        })
        .map_err(|e| AppError::Database(e.to_string()))?;
    mapped
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::Database(e.to_string()))
}

fn load_all_stored_keys(conn: &Connection) -> Result<Vec<StoredKey>, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, group_id, label, key_value FROM shared_api_keys
             ORDER BY group_id ASC, sort_index ASC, id ASC",
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    let mapped = stmt
        .query_map([], |row| {
            Ok(StoredKey {
                id: row.get(0)?,
                group_id: row.get(1)?,
                label: row.get(2)?,
                key: row.get(3)?,
            })
        })
        .map_err(|e| AppError::Database(e.to_string()))?;
    mapped
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AppError::Database(e.to_string()))
}

fn build_provider_components(rows: &[ProviderKeyRow]) -> UnionFind {
    let mut union_find = UnionFind::new(rows.len());
    let mut names = HashMap::<(String, String), Vec<usize>>::new();
    let mut domains = HashMap::<(String, String), Vec<usize>>::new();
    let mut groups = HashMap::<String, usize>::new();

    for (index, row) in rows.iter().enumerate() {
        let other_app = if row.app_type == "claude" {
            "codex"
        } else {
            "claude"
        };
        if let Some(name) = normalize_provider_name(&row.name) {
            if let Some(matches) = names.get(&(name.clone(), other_app.to_string())) {
                for previous in matches {
                    union_find.union(*previous, index);
                }
            }
            names
                .entry((name, row.app_type.clone()))
                .or_default()
                .push(index);
        }
        if let Some(domain) = provider_root_domain(&row.app_type, &row.settings_config) {
            if let Some(matches) = domains.get(&(domain.clone(), other_app.to_string())) {
                for previous in matches {
                    union_find.union(*previous, index);
                }
            }
            domains
                .entry((domain, row.app_type.clone()))
                .or_default()
                .push(index);
        }
        if let Some(group_id) = row.group_id.as_ref() {
            if let Some(previous) = groups.insert(group_id.clone(), index) {
                union_find.union(previous, index);
            }
        }
    }
    union_find
}

fn normalize_provider_name(name: &str) -> Option<String> {
    let normalized = name.trim().to_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

fn provider_root_domain(app_type: &str, settings: &Value) -> Option<String> {
    let base_url = match app_type {
        "claude" => settings
            .pointer("/env/ANTHROPIC_BASE_URL")
            .or_else(|| settings.get("apiBaseUrl"))
            .and_then(Value::as_str)
            .map(str::to_string),
        "codex" => settings
            .get("config")
            .and_then(Value::as_str)
            .and_then(crate::codex_config::extract_codex_base_url),
        _ => None,
    }?;
    let parsed = Url::parse(base_url.trim()).ok()?;
    let host = parsed.host_str()?.trim_end_matches('.').to_lowercase();
    root_domain_from_host(&host)
}

fn root_domain_from_host(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.').to_lowercase();
    if host.is_empty() {
        return None;
    }
    if host.parse::<std::net::IpAddr>().is_ok() || host == "localhost" {
        return Some(host);
    }
    let labels: Vec<&str> = host.split('.').filter(|label| !label.is_empty()).collect();
    if labels.len() <= 2 {
        return Some(host);
    }

    // Common multi-label public suffixes used by supported providers. Keeping
    // this local avoids adding a lockfile-changing dependency in the desktop app.
    const MULTI_LABEL_SUFFIXES: &[&str] = &[
        "ac.cn", "com.cn", "edu.cn", "gov.cn", "net.cn", "org.cn", "co.uk", "me.uk", "net.uk",
        "org.uk", "com.au", "net.au", "org.au", "co.jp", "ne.jp", "or.jp", "com.hk", "net.hk",
        "org.hk", "com.tw", "net.tw", "org.tw", "co.nz", "com.br", "com.sg", "com.my", "co.kr",
        "co.in",
    ];
    // Multi-tenant hosting suffixes must retain the tenant label to avoid
    // sharing keys between unrelated customers.
    const PRIVATE_SUFFIXES: &[&str] = &[
        "github.io",
        "pages.dev",
        "workers.dev",
        "vercel.app",
        "netlify.app",
        "onrender.com",
        "railway.app",
    ];

    let suffix2 = labels[labels.len() - 2..].join(".");
    let keep = if MULTI_LABEL_SUFFIXES.contains(&suffix2.as_str())
        || PRIVATE_SUFFIXES.contains(&suffix2.as_str())
    {
        3
    } else {
        2
    };
    Some(labels[labels.len().saturating_sub(keep)..].join("."))
}

fn build_old_key_by_id(rows: &[ProviderKeyRow], stored: &[StoredKey]) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for key in stored {
        values
            .entry(key.id.clone())
            .or_insert_with(|| key.key.clone());
    }
    for row in rows {
        for key in &row.meta.api_keys {
            values
                .entry(key.id.clone())
                .or_insert_with(|| key.key.clone());
        }
    }
    values
}

fn build_selection_values(
    rows: &[ProviderKeyRow],
    component: &[usize],
    old_key_by_id: &HashMap<String, String>,
    saved: Option<(usize, &SharedKeySaveInput)>,
) -> HashMap<usize, Option<String>> {
    let mut selections = HashMap::new();
    for index in component {
        let selected = if let Some((saved_index, input)) = saved {
            if *index == saved_index {
                input
                    .selected_key_id
                    .as_ref()
                    .and_then(|selected_id| {
                        input
                            .keys
                            .iter()
                            .find(|entry| &entry.id == selected_id)
                            .map(|entry| entry.key.trim().to_string())
                    })
                    .or_else(|| active_key_value(&rows[*index]))
            } else {
                selected_value_for_row(&rows[*index], old_key_by_id)
                    .or_else(|| active_key_value(&rows[*index]))
            }
        } else {
            selected_value_for_row(&rows[*index], old_key_by_id)
                .or_else(|| active_key_value(&rows[*index]))
        };
        selections.insert(*index, selected.filter(|value| !value.is_empty()));
    }
    selections
}

fn selected_value_for_row(
    row: &ProviderKeyRow,
    old_key_by_id: &HashMap<String, String>,
) -> Option<String> {
    let selected_id = row.meta.selected_key_id.as_ref()?;
    row.meta
        .api_keys
        .iter()
        .find(|entry| &entry.id == selected_id)
        .map(|entry| entry.key.clone())
        .or_else(|| old_key_by_id.get(selected_id).cloned())
}

fn active_key_value(row: &ProviderKeyRow) -> Option<String> {
    let raw = match row.app_type.as_str() {
        "claude" => {
            let env = row.settings_config.get("env")?;
            let preferred = row.meta.api_key_field.as_deref();
            preferred
                .and_then(|field| env.get(field))
                .and_then(Value::as_str)
                .or_else(|| env.get("ANTHROPIC_AUTH_TOKEN").and_then(Value::as_str))
                .or_else(|| env.get("ANTHROPIC_API_KEY").and_then(Value::as_str))
        }
        "codex" => row
            .settings_config
            .pointer("/auth/OPENAI_API_KEY")
            .and_then(Value::as_str),
        _ => None,
    }?;
    let value = raw.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn merge_active_key(
    final_keys: &mut Vec<ApiKeyEntry>,
    indexes: &mut HashMap<String, usize>,
    used_ids: &mut HashSet<String>,
    row: &ProviderKeyRow,
) {
    let Some(key_value) = active_key_value(row) else {
        return;
    };
    let label = row.name.trim();
    merge_api_keys(
        final_keys,
        indexes,
        used_ids,
        [ApiKeyEntry {
            id: uuid::Uuid::new_v4().to_string(),
            label: if label.is_empty() {
                "Active Key".to_string()
            } else {
                format!("{label} Key")
            },
            key: key_value,
            strategy: None,
        }],
    );
}

fn merge_stored_keys<'a>(
    final_keys: &mut Vec<ApiKeyEntry>,
    indexes: &mut HashMap<String, usize>,
    used_ids: &mut HashSet<String>,
    keys: impl Iterator<Item = &'a StoredKey>,
) {
    merge_api_keys(
        final_keys,
        indexes,
        used_ids,
        keys.map(|key| ApiKeyEntry {
            id: key.id.clone(),
            label: key.label.clone(),
            key: key.key.clone(),
            strategy: None,
        }),
    );
}

fn merge_api_keys(
    final_keys: &mut Vec<ApiKeyEntry>,
    indexes: &mut HashMap<String, usize>,
    used_ids: &mut HashSet<String>,
    keys: impl IntoIterator<Item = ApiKeyEntry>,
) {
    for mut entry in keys {
        entry.key = entry.key.trim().to_string();
        entry.label = entry.label.trim().to_string();
        if entry.key.is_empty() {
            continue;
        }
        if let Some(index) = indexes.get(&entry.key).copied() {
            if final_keys[index].label.is_empty() && !entry.label.is_empty() {
                final_keys[index].label = entry.label;
            }
            continue;
        }
        if entry.id.trim().is_empty() || used_ids.contains(&entry.id) {
            entry.id = uuid::Uuid::new_v4().to_string();
        }
        entry.strategy = None;
        used_ids.insert(entry.id.clone());
        indexes.insert(entry.key.clone(), final_keys.len());
        final_keys.push(entry);
    }
}

fn apply_component_pool(
    conn: &Connection,
    rows: &[ProviderKeyRow],
    component: &[usize],
    group_ids: &[String],
    target_group: &str,
    final_keys: &[ApiKeyEntry],
    selection_values: &HashMap<usize, Option<String>>,
) -> Result<(), AppError> {
    conn.execute(
        "INSERT OR IGNORE INTO shared_key_groups (id, created_at)
         VALUES (?1, strftime('%s','now'))",
        params![target_group],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;

    for index in component {
        conn.execute(
            "INSERT INTO provider_shared_key_links (provider_id, app_type, group_id)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(provider_id, app_type)
             DO UPDATE SET group_id = excluded.group_id",
            params![rows[*index].id, rows[*index].app_type, target_group],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    }

    for group_id in group_ids {
        if group_id != target_group {
            conn.execute(
                "DELETE FROM shared_key_groups WHERE id = ?1",
                params![group_id],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        }
    }
    conn.execute(
        "DELETE FROM shared_api_keys WHERE group_id = ?1",
        params![target_group],
    )
    .map_err(|e| AppError::Database(e.to_string()))?;

    let key_id_by_value: HashMap<&str, &str> = final_keys
        .iter()
        .map(|entry| (entry.key.as_str(), entry.id.as_str()))
        .collect();
    for (sort_index, entry) in final_keys.iter().enumerate() {
        conn.execute(
            "INSERT INTO shared_api_keys
             (id, group_id, label, key_value, sort_index)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![entry.id, target_group, entry.label, entry.key, sort_index],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    }

    for index in component {
        let mut meta = rows[*index].meta.clone();
        meta.api_keys.clear();
        meta.shared_key_apps.clear();
        meta.shared_key_pool_loaded = false;
        meta.selected_key_id = selection_values
            .get(index)
            .and_then(|value| value.as_deref())
            .and_then(|value| key_id_by_value.get(value).copied())
            .map(str::to_string);
        let meta_json = serde_json::to_string(&meta)
            .map_err(|e| AppError::Database(format!("Failed to serialize meta: {e}")))?;
        conn.execute(
            "UPDATE providers SET meta = ?1 WHERE id = ?2 AND app_type = ?3",
            params![meta_json, rows[*index].id, rows[*index].app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn root_domain_handles_subdomains_country_suffixes_and_tenants() {
        assert_eq!(
            root_domain_from_host("api.example.com").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            root_domain_from_host("claude.example.com.cn").as_deref(),
            Some("example.com.cn")
        );
        assert_eq!(
            root_domain_from_host("tenant.github.io").as_deref(),
            Some("tenant.github.io")
        );
        assert_eq!(
            root_domain_from_host("127.0.0.1").as_deref(),
            Some("127.0.0.1")
        );
    }

    #[test]
    fn normalized_names_ignore_outer_space_and_case() {
        assert_eq!(
            normalize_provider_name("  RunAway  ").as_deref(),
            Some("runaway")
        );
        assert_eq!(
            normalize_provider_name("  随时跑路  ").as_deref(),
            Some("随时跑路")
        );
    }

    #[test]
    fn automatic_identity_matching_is_cross_app_only() {
        let rows = vec![
            ProviderKeyRow {
                id: "claude-a".to_string(),
                app_type: "claude".to_string(),
                name: "Same".to_string(),
                settings_config: json!({"env": {"ANTHROPIC_BASE_URL": "https://api.same.example"}}),
                meta: ProviderMeta::default(),
                group_id: None,
            },
            ProviderKeyRow {
                id: "claude-b".to_string(),
                app_type: "claude".to_string(),
                name: "same".to_string(),
                settings_config: json!({
                    "env": {"ANTHROPIC_BASE_URL": "https://other.same.example"}
                }),
                meta: ProviderMeta::default(),
                group_id: None,
            },
        ];
        let mut components = build_provider_components(&rows);
        assert_ne!(components.find(0), components.find(1));
    }

    #[test]
    fn merges_exact_key_values_and_keeps_non_empty_label() {
        let mut result = Vec::new();
        let mut indexes = HashMap::new();
        let mut used = HashSet::new();
        merge_api_keys(
            &mut result,
            &mut indexes,
            &mut used,
            vec![
                ApiKeyEntry {
                    id: "a".to_string(),
                    label: String::new(),
                    key: "sk-same".to_string(),
                    strategy: Some("anthropic".to_string()),
                },
                ApiKeyEntry {
                    id: "b".to_string(),
                    label: "Codex Key".to_string(),
                    key: "sk-same".to_string(),
                    strategy: Some("bearer".to_string()),
                },
                ApiKeyEntry {
                    id: "c".to_string(),
                    label: "Other".to_string(),
                    key: "sk-other".to_string(),
                    strategy: None,
                },
            ],
        );
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].id, "a");
        assert_eq!(result[0].label, "Codex Key");
        assert!(result.iter().all(|entry| entry.strategy.is_none()));
    }

    #[test]
    fn provider_domain_uses_app_specific_config() {
        let claude = json!({
            "env": {"ANTHROPIC_BASE_URL": "https://claude.example.com/v1"}
        });
        let codex = json!({
            "config": "model_provider = \"test\"\n\n[model_providers.test]\nbase_url = \"https://api.example.com/v1\"\n"
        });
        assert_eq!(
            provider_root_domain("claude", &claude).as_deref(),
            Some("example.com")
        );
        assert_eq!(
            provider_root_domain("codex", &codex).as_deref(),
            Some("example.com")
        );
    }

    fn key(id: &str, label: &str, value: &str, strategy: &str) -> ApiKeyEntry {
        ApiKeyEntry {
            id: id.to_string(),
            label: label.to_string(),
            key: value.to_string(),
            strategy: Some(strategy.to_string()),
        }
    }

    fn provider_with_keys(
        id: &str,
        name: &str,
        settings_config: Value,
        keys: Vec<ApiKeyEntry>,
        selected_key_id: &str,
    ) -> Provider {
        let mut provider =
            Provider::with_id(id.to_string(), name.to_string(), settings_config, None);
        provider.meta = Some(ProviderMeta {
            api_keys: keys,
            selected_key_id: Some(selected_key_id.to_string()),
            ..ProviderMeta::default()
        });
        provider
    }

    #[test]
    fn save_merges_claude_and_codex_pools_with_independent_selection() -> Result<(), AppError> {
        let db = Database::memory()?;
        let claude = provider_with_keys(
            "claude-provider",
            "  Same Vendor ",
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://claude.vendor.example/v1",
                    "ANTHROPIC_AUTH_TOKEN": "sk-a"
                }
            }),
            vec![
                key("key-a", "Claude A", "sk-a", "claude_auth"),
                key("key-b", "", "sk-shared", "claude_auth"),
            ],
            "key-a",
        );
        db.save_provider("claude", &claude)?;

        let codex = provider_with_keys(
            "codex-provider",
            "same vendor",
            json!({
                "auth": {"OPENAI_API_KEY": "sk-c"},
                "config": "[model_providers.vendor]\nbase_url = \"https://codex.vendor.example/v1\"\n"
            }),
            vec![
                key("key-shared-other-id", "Shared Label", "sk-shared", "bearer"),
                key("key-c", "Codex C", "sk-c", "bearer"),
            ],
            "key-c",
        );
        db.save_provider("codex", &codex)?;

        let claude = db
            .get_provider_by_id("claude-provider", "claude")?
            .expect("Claude provider");
        let codex = db
            .get_provider_by_id("codex-provider", "codex")?
            .expect("Codex provider");
        let claude_meta = claude.meta.expect("Claude meta");
        let codex_meta = codex.meta.expect("Codex meta");

        assert_eq!(claude_meta.api_keys.len(), 3);
        assert_eq!(codex_meta.api_keys.len(), 3);
        assert_eq!(
            claude_meta
                .resolve_selected_key()
                .map(|selected| selected.0),
            Some("sk-a")
        );
        assert_eq!(
            codex_meta.resolve_selected_key().map(|selected| selected.0),
            Some("sk-c")
        );
        assert!(claude_meta
            .api_keys
            .iter()
            .all(|entry| entry.strategy.as_deref() == Some("claude_auth")));
        assert!(codex_meta
            .api_keys
            .iter()
            .all(|entry| entry.strategy.as_deref() == Some("bearer")));
        assert_eq!(claude_meta.shared_key_apps, vec!["claude", "codex"]);
        assert_eq!(codex_meta.shared_key_apps, vec!["claude", "codex"]);

        let conn = crate::database::lock_conn!(db.conn);
        let central_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM shared_api_keys", [], |row| row.get(0))?;
        let raw_meta: String = conn.query_row(
            "SELECT meta FROM providers WHERE id = 'claude-provider' AND app_type = 'claude'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(central_count, 3);
        let raw_meta_json: Value = serde_json::from_str(&raw_meta).expect("valid provider meta");
        assert!(raw_meta_json.get("apiKeys").is_none());
        drop(conn);

        let sync_sql = db.export_sql_string_for_sync()?;
        assert!(sync_sql.contains("shared_api_keys"));
        assert!(sync_sql.contains("provider_shared_key_links"));
        assert_eq!(sync_sql.matches("sk-shared").count(), 1);
        Ok(())
    }

    #[test]
    fn legacy_active_credentials_join_pool() -> Result<(), AppError> {
        let db = Database::memory()?;
        let claude = provider_with_keys(
            "claude-legacy",
            "Legacy Shared",
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://claude.legacy.example",
                    "ANTHROPIC_API_KEY": "sk-legacy-a"
                }
            }),
            vec![],
            "",
        );
        db.save_provider("claude", &claude)?;
        let codex = provider_with_keys(
            "codex-legacy",
            "Legacy Shared",
            json!({
                "auth": {"OPENAI_API_KEY": "sk-legacy-b"},
                "config": "base_url = \"https://codex.legacy.example\""
            }),
            vec![],
            "",
        );
        db.save_provider("codex", &codex)?;

        let claude = db
            .get_provider_by_id("claude-legacy", "claude")?
            .expect("Claude provider");
        let codex = db
            .get_provider_by_id("codex-legacy", "codex")?
            .expect("Codex provider");
        let claude_meta = claude.meta.expect("Claude meta");
        let codex_meta = codex.meta.expect("Codex meta");
        assert_eq!(claude_meta.api_keys.len(), 2);
        assert_eq!(codex_meta.api_keys.len(), 2);
        assert_eq!(
            claude_meta
                .resolve_selected_key()
                .map(|selected| selected.0),
            Some("sk-legacy-a")
        );
        assert_eq!(
            codex_meta.resolve_selected_key().map(|selected| selected.0),
            Some("sk-legacy-b")
        );
        Ok(())
    }

    #[test]
    fn cannot_delete_key_selected_by_linked_provider() -> Result<(), AppError> {
        let db = Database::memory()?;
        let claude = provider_with_keys(
            "claude-provider",
            "Same",
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://api.same.example",
                    "ANTHROPIC_API_KEY": "sk-a"
                }
            }),
            vec![key("key-a", "A", "sk-a", "anthropic")],
            "key-a",
        );
        db.save_provider("claude", &claude)?;
        let codex = provider_with_keys(
            "codex-provider",
            "Same",
            json!({
                "auth": {"OPENAI_API_KEY": "sk-b"},
                "config": "base_url = \"https://api.same.example\""
            }),
            vec![key("key-b", "B", "sk-b", "bearer")],
            "key-b",
        );
        db.save_provider("codex", &codex)?;

        let mut claude = db
            .get_provider_by_id("claude-provider", "claude")?
            .expect("Claude provider");
        let meta = claude.meta.as_mut().expect("Claude meta");
        meta.api_keys.retain(|entry| entry.key != "sk-b");
        let err = db
            .save_provider("claude", &claude)
            .expect_err("linked selected key deletion must fail");
        assert!(err.to_string().contains("Codex") || err.to_string().contains("codex"));
        Ok(())
    }
}
