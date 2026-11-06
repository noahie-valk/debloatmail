//! D-Bus engine adapter calling `org.noahie.debloatmail.Daemon`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rusqlite::Connection;
use tokio::sync::Mutex;
use zbus::proxy;
use zbus::zvariant::{OwnedValue, Value};

use debloatmail_core::SortKey;

use crate::error::FfiError;
use crate::traits::{EngineMutations, EngineReads};
use crate::types::*;

#[proxy(
    default_service = "org.noahie.debloatmail.Daemon",
    default_path = "/org/noahie/debloatmail",
    interface = "org.noahie.debloatmail.Items"
)]
trait Items {
    fn mark_read(&self, ids: &[&str], read: bool) -> zbus::Result<u32>;
    fn set_starred(&self, ids: &[&str], starred: bool) -> zbus::Result<u32>;
    fn r#move(&self, ids: &[&str], target_folder_id: &str) -> zbus::Result<u32>;
    fn copy(&self, ids: &[&str], target_folder_id: &str) -> zbus::Result<u32>;
    fn delete(&self, ids: &[&str], expunge: bool) -> zbus::Result<u32>;
    fn hide(&self, ids: &[&str]) -> zbus::Result<u32>;
}

#[proxy(
    default_service = "org.noahie.debloatmail.Daemon",
    default_path = "/org/noahie/debloatmail",
    interface = "org.noahie.debloatmail.Sync"
)]
trait Sync {
    fn sync(&self, scope: HashMap<String, Value<'_>>, full: bool) -> zbus::Result<u32>;
    fn cancel(&self, job_id: u32) -> zbus::Result<bool>;
    fn fetch_body(&self, item_id: &str) -> zbus::Result<String>;
    fn fetch_attachment(&self, item_id: &str, section_id: &str) -> zbus::Result<String>;
    fn refresh_feed(&self, feed_id: &str) -> zbus::Result<u32>;
}

#[proxy(
    default_service = "org.noahie.debloatmail.Daemon",
    default_path = "/org/noahie/debloatmail",
    interface = "org.noahie.debloatmail.Accounts"
)]
trait Accounts {
    fn add(&self, config: HashMap<String, Value<'_>>) -> zbus::Result<String>;
    fn update(&self, account_id: &str, config: HashMap<String, Value<'_>>) -> zbus::Result<bool>;
    fn remove(&self, account_id: &str, purge: bool) -> zbus::Result<bool>;
    fn check(&self, account_id: &str) -> zbus::Result<HashMap<String, OwnedValue>>;
    fn reload(&self) -> zbus::Result<bool>;
}

#[proxy(
    default_service = "org.noahie.debloatmail.Daemon",
    default_path = "/org/noahie/debloatmail",
    interface = "org.noahie.debloatmail.Feeds"
)]
trait Feeds {
    fn add(
        &self,
        account_id: &str,
        url: &str,
        name: &str,
        interval_secs: u32,
    ) -> zbus::Result<HashMap<String, OwnedValue>>;
    fn update(&self, feed_id: &str, config: HashMap<String, Value<'_>>) -> zbus::Result<bool>;
    fn remove(&self, feed_id: &str, purge: bool) -> zbus::Result<bool>;
}

#[proxy(
    default_service = "org.noahie.debloatmail.Daemon",
    default_path = "/org/noahie/debloatmail",
    interface = "org.noahie.debloatmail.Compose"
)]
trait Compose {
    fn send(
        &self,
        draft: HashMap<String, Value<'_>>,
        draft_id: &str,
        append_to_sent: bool,
    ) -> zbus::Result<String>;
    fn save_draft(&self, draft: HashMap<String, Value<'_>>) -> zbus::Result<String>;
    fn delete_draft(&self, draft_id: &str) -> zbus::Result<bool>;
}

#[proxy(
    default_service = "org.noahie.debloatmail.Daemon",
    default_path = "/org/noahie/debloatmail",
    interface = "org.noahie.debloatmail.Queue"
)]
trait Queue {
    #[zbus(property)]
    fn count(&self) -> zbus::Result<u32>;
    fn pending(&self) -> zbus::Result<Vec<String>>;
    fn get(&self, op_id: &str) -> zbus::Result<HashMap<String, OwnedValue>>;
    fn cancel(&self, op_id: &str) -> zbus::Result<bool>;
}

#[proxy(
    default_service = "org.noahie.debloatmail.Daemon",
    default_path = "/org/noahie/debloatmail",
    interface = "org.noahie.debloatmail.Status"
)]
trait Status {
    #[zbus(property)]
    fn api_version(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn features(&self) -> zbus::Result<Vec<String>>;
    #[zbus(property)]
    fn daemon_version(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn state(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn revision(&self) -> zbus::Result<u64>;
    #[zbus(property)]
    fn pending_ops(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn last_sync_at(&self) -> zbus::Result<u64>;
}

fn map_dbus_error(err: zbus::Error) -> FfiError {
    match err {
        zbus::Error::MethodError(ref name, ref detail, _) => {
            let detail_str = detail.clone().unwrap_or_default();
            let name_str = name.as_str();
            if name_str.ends_with(".NotFound") {
                FfiError::NotFound(detail_str)
            } else if name_str.ends_with(".InvalidArgument") {
                FfiError::InvalidArgument(detail_str)
            } else if name_str.ends_with(".Unsupported") {
                FfiError::Unsupported(detail_str)
            } else if name_str.ends_with(".ServiceUnknown")
                || name_str.ends_with(".NameHasNoOwner")
                || name_str.ends_with(".NoReply")
            {
                FfiError::DaemonUnavailable(format!("{name_str}: {detail_str}"))
            } else {
                FfiError::Dbus(err)
            }
        }
        zbus::Error::InputOutput(_) => FfiError::DaemonUnavailable(err.to_string()),
        other => FfiError::Dbus(other),
    }
}

/// D-Bus client engine adapter connecting to `org.noahie.debloatmail.Daemon`.
///
/// Implements `EngineReads` via direct read-only SQLite queries (WAL concurrent reading)
/// and `EngineMutations` via asynchronous D-Bus proxy calls to `debloatmaild`.
pub struct DbusEngine {
    dbus: zbus::Connection,
    conn: Arc<Mutex<Connection>>,
    blobs_dir: PathBuf,
}

impl DbusEngine {
    /// Connects to the user's D-Bus session bus and opens a read-only SQLite connection.
    pub async fn connect(db_path: &Path, blobs_dir: PathBuf) -> Result<Self, FfiError> {
        let dbus = zbus::Connection::session().await.map_err(|e| {
            FfiError::DaemonUnavailable(format!(
                "session bus connect failed for org.noahie.debloatmail.Daemon: {e}"
            ))
        })?;

        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_URI;

        let conn = rusqlite::Connection::open_with_flags(db_path, flags)?;
        conn.pragma_update(None, "busy_timeout", 5000)?;

        Ok(Self {
            dbus,
            conn: Arc::new(Mutex::new(conn)),
            blobs_dir,
        })
    }

    /// Creates a `DbusEngine` instance from existing components (useful for tests).
    pub fn from_parts(dbus: zbus::Connection, conn: Connection, blobs_dir: PathBuf) -> Self {
        Self {
            dbus,
            conn: Arc::new(Mutex::new(conn)),
            blobs_dir,
        }
    }

    fn lock_conn(&self) -> Result<tokio::sync::MutexGuard<'_, Connection>, FfiError> {
        if let Ok(guard) = self.conn.try_lock() {
            return Ok(guard);
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            Ok(tokio::task::block_in_place(|| self.conn.blocking_lock()))
        } else {
            Ok(self.conn.blocking_lock())
        }
    }
}

impl EngineReads for DbusEngine {
    fn query_stream(
        &self,
        account_id: Option<&str>,
        folder_id: Option<&str>,
        feed_id: Option<&str>,
        kind_filter: Option<&str>,
        unread_only: bool,
        starred_only: bool,
        since: Option<&str>,
        before: Option<&str>,
        sort: SortKey,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<FfiPage<FfiItemSummary>, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::query_stream_fn(
            &conn,
            account_id,
            folder_id,
            feed_id,
            kind_filter,
            unread_only,
            starred_only,
            since,
            before,
            sort,
            limit,
            cursor,
        )
    }

    fn query_threads(
        &self,
        account_id: Option<&str>,
        sort: SortKey,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<FfiPage<FfiThreadSummary>, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::query_threads_fn(&conn, account_id, sort, limit, cursor)
    }

    fn get_item(&self, public_id: &str) -> Result<FfiItemDetail, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::get_item_fn(&conn, &self.blobs_dir, public_id)
    }

    fn search_items(
        &self,
        query: &str,
        account_id: Option<&str>,
        sort: SortKey,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<FfiPage<FfiItemSummary>, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::search_items_fn(&conn, query, account_id, sort, limit, cursor)
    }

    fn list_accounts(&self) -> Result<Vec<FfiAccount>, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::list_accounts_fn(&conn)
    }

    fn get_account(&self, public_id: &str) -> Result<FfiAccount, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::get_account_fn(&conn, public_id)
    }

    fn list_folders(&self, account_id: Option<&str>) -> Result<Vec<FfiFolder>, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::list_folders_fn(&conn, account_id)
    }

    fn list_feeds(&self, account_id: Option<&str>) -> Result<Vec<FfiFeed>, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::list_feeds_fn(&conn, account_id)
    }

    fn get_feed(&self, public_id: &str) -> Result<FfiFeed, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::get_feed_fn(&conn, public_id)
    }

    fn get_pending_ops(&self) -> Result<Vec<FfiOpStatus>, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::get_pending_ops_fn(&conn)
    }

    fn get_op_status(&self, op_id: &str) -> Result<FfiOpStatus, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::get_op_status_fn(&conn, op_id)
    }
}

#[async_trait]
impl EngineMutations for DbusEngine {
    async fn mark_read(&self, ids: &[String], read: bool) -> Result<u32, FfiError> {
        let proxy = ItemsProxy::new(&self.dbus).await.map_err(map_dbus_error)?;
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        proxy
            .mark_read(&id_refs, read)
            .await
            .map_err(map_dbus_error)
    }

    async fn set_starred(&self, ids: &[String], starred: bool) -> Result<u32, FfiError> {
        let proxy = ItemsProxy::new(&self.dbus).await.map_err(map_dbus_error)?;
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        proxy
            .set_starred(&id_refs, starred)
            .await
            .map_err(map_dbus_error)
    }

    async fn move_items(&self, ids: &[String], target_folder_id: &str) -> Result<u32, FfiError> {
        let proxy = ItemsProxy::new(&self.dbus).await.map_err(map_dbus_error)?;
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        proxy
            .r#move(&id_refs, target_folder_id)
            .await
            .map_err(map_dbus_error)
    }

    async fn copy_items(&self, ids: &[String], target_folder_id: &str) -> Result<u32, FfiError> {
        let proxy = ItemsProxy::new(&self.dbus).await.map_err(map_dbus_error)?;
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        proxy
            .copy(&id_refs, target_folder_id)
            .await
            .map_err(map_dbus_error)
    }

    async fn delete_items(&self, ids: &[String], expunge: bool) -> Result<u32, FfiError> {
        let proxy = ItemsProxy::new(&self.dbus).await.map_err(map_dbus_error)?;
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        proxy
            .delete(&id_refs, expunge)
            .await
            .map_err(map_dbus_error)
    }

    async fn hide_items(&self, ids: &[String]) -> Result<u32, FfiError> {
        let proxy = ItemsProxy::new(&self.dbus).await.map_err(map_dbus_error)?;
        let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        proxy.hide(&id_refs).await.map_err(map_dbus_error)
    }

    async fn send_message(
        &self,
        draft: FfiDraft,
        draft_id: Option<String>,
        append_to_sent: bool,
    ) -> Result<String, FfiError> {
        let proxy = ComposeProxy::new(&self.dbus)
            .await
            .map_err(map_dbus_error)?;
        let mut draft_map: HashMap<String, Value<'_>> = HashMap::new();
        let draft_id_str = draft_id.unwrap_or_default();

        let to_vals: Vec<Value<'_>> = draft.to.iter().map(|s| Value::from(s.as_str())).collect();
        let cc_vals: Vec<Value<'_>> = draft.cc.iter().map(|s| Value::from(s.as_str())).collect();
        let bcc_vals: Vec<Value<'_>> = draft.bcc.iter().map(|s| Value::from(s.as_str())).collect();
        let att_vals: Vec<Value<'_>> = draft
            .attachment_paths
            .iter()
            .map(|p| Value::from(p.to_string_lossy().into_owned()))
            .collect();

        draft_map.insert("to".to_string(), Value::from(to_vals));
        draft_map.insert("cc".to_string(), Value::from(cc_vals));
        draft_map.insert("bcc".to_string(), Value::from(bcc_vals));
        draft_map.insert("subject".to_string(), Value::from(draft.subject.as_str()));
        draft_map.insert("body".to_string(), Value::from(draft.body.as_str()));
        if let Some(in_reply) = &draft.in_reply_to {
            draft_map.insert("in_reply_to".to_string(), Value::from(in_reply.as_str()));
        }
        draft_map.insert("attachments".to_string(), Value::from(att_vals));

        proxy
            .send(draft_map, &draft_id_str, append_to_sent)
            .await
            .map_err(map_dbus_error)
    }

    async fn save_draft(&self, draft: FfiDraft) -> Result<String, FfiError> {
        let proxy = ComposeProxy::new(&self.dbus)
            .await
            .map_err(map_dbus_error)?;
        let mut draft_map: HashMap<String, Value<'_>> = HashMap::new();

        let to_vals: Vec<Value<'_>> = draft.to.iter().map(|s| Value::from(s.as_str())).collect();
        let cc_vals: Vec<Value<'_>> = draft.cc.iter().map(|s| Value::from(s.as_str())).collect();
        let bcc_vals: Vec<Value<'_>> = draft.bcc.iter().map(|s| Value::from(s.as_str())).collect();
        let att_vals: Vec<Value<'_>> = draft
            .attachment_paths
            .iter()
            .map(|p| Value::from(p.to_string_lossy().into_owned()))
            .collect();

        draft_map.insert("to".to_string(), Value::from(to_vals));
        draft_map.insert("cc".to_string(), Value::from(cc_vals));
        draft_map.insert("bcc".to_string(), Value::from(bcc_vals));
        draft_map.insert("subject".to_string(), Value::from(draft.subject.as_str()));
        draft_map.insert("body".to_string(), Value::from(draft.body.as_str()));
        if let Some(in_reply) = &draft.in_reply_to {
            draft_map.insert("in_reply_to".to_string(), Value::from(in_reply.as_str()));
        }
        draft_map.insert("attachments".to_string(), Value::from(att_vals));

        proxy.save_draft(draft_map).await.map_err(map_dbus_error)
    }

    async fn fetch_body(&self, item_id: &str) -> Result<FfiBodyState, FfiError> {
        let proxy = SyncProxy::new(&self.dbus).await.map_err(map_dbus_error)?;
        let res = proxy.fetch_body(item_id).await.map_err(map_dbus_error)?;
        let state = match res.as_str() {
            "cached" => FfiBodyState::Cached,
            "failed" => FfiBodyState::Failed,
            "pending" => FfiBodyState::Pending,
            _ => FfiBodyState::None,
        };
        Ok(state)
    }

    async fn fetch_attachment(&self, item_id: &str, section_id: &str) -> Result<PathBuf, FfiError> {
        let proxy = SyncProxy::new(&self.dbus).await.map_err(map_dbus_error)?;
        let path_str = proxy
            .fetch_attachment(item_id, section_id)
            .await
            .map_err(map_dbus_error)?;
        Ok(PathBuf::from(path_str))
    }

    async fn trigger_sync(
        &self,
        account_id: Option<String>,
        folder_id: Option<String>,
        feed_id: Option<String>,
        full: bool,
    ) -> Result<String, FfiError> {
        let proxy = SyncProxy::new(&self.dbus).await.map_err(map_dbus_error)?;
        let mut map: HashMap<String, Value<'_>> = HashMap::new();
        if let Some(acc) = &account_id {
            map.insert("account".to_string(), Value::from(acc.as_str()));
        }
        if let Some(fol) = &folder_id {
            map.insert("folder".to_string(), Value::from(fol.as_str()));
        }
        if let Some(fee) = &feed_id {
            map.insert("feed".to_string(), Value::from(fee.as_str()));
        }
        let job_id = proxy.sync(map, full).await.map_err(map_dbus_error)?;
        Ok(job_id.to_string())
    }

    async fn refresh_feed(&self, feed_id: &str) -> Result<String, FfiError> {
        let proxy = SyncProxy::new(&self.dbus).await.map_err(map_dbus_error)?;
        let job_id = proxy.refresh_feed(feed_id).await.map_err(map_dbus_error)?;
        Ok(job_id.to_string())
    }

    async fn add_feed(
        &self,
        account_id: &str,
        url: &str,
        name: &str,
        interval_secs: u32,
    ) -> Result<(String, bool), FfiError> {
        let proxy = FeedsProxy::new(&self.dbus).await.map_err(map_dbus_error)?;
        let res_map = proxy
            .add(account_id, url, name, interval_secs)
            .await
            .map_err(map_dbus_error)?;

        let feed_id = res_map
            .get("feed_id")
            .and_then(|v| {
                if let Ok(Value::Str(s)) = Value::try_from(v) {
                    Some(s.to_string())
                } else {
                    None
                }
            })
            .unwrap_or_default();

        let validated = res_map
            .get("validated")
            .and_then(|v| {
                if let Ok(Value::Bool(b)) = Value::try_from(v) {
                    Some(b)
                } else {
                    None
                }
            })
            .unwrap_or(false);

        Ok((feed_id, validated))
    }

    async fn update_feed(
        &self,
        feed_id: &str,
        name: Option<&str>,
        interval_secs: Option<u32>,
    ) -> Result<(), FfiError> {
        let proxy = FeedsProxy::new(&self.dbus).await.map_err(map_dbus_error)?;
        let mut map: HashMap<String, Value<'_>> = HashMap::new();
        if let Some(n) = name {
            map.insert("name".to_string(), Value::from(n));
        }
        if let Some(i) = interval_secs {
            map.insert("interval_secs".to_string(), Value::from(i));
        }
        proxy.update(feed_id, map).await.map_err(map_dbus_error)?;
        Ok(())
    }

    async fn remove_feed(&self, feed_id: &str, purge: bool) -> Result<(), FfiError> {
        let proxy = FeedsProxy::new(&self.dbus).await.map_err(map_dbus_error)?;
        proxy.remove(feed_id, purge).await.map_err(map_dbus_error)?;
        Ok(())
    }

    async fn add_account(&self, params: serde_json::Value) -> Result<String, FfiError> {
        let proxy = AccountsProxy::new(&self.dbus)
            .await
            .map_err(map_dbus_error)?;
        let mut map: HashMap<String, Value<'_>> = HashMap::new();

        if let Some(obj) = params.as_object() {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    map.insert(k.clone(), Value::from(s));
                } else if let Some(b) = v.as_bool() {
                    map.insert(k.clone(), Value::from(b));
                } else if let Some(u) = v.as_u64() {
                    map.insert(k.clone(), Value::from(u as u32));
                }
            }
        }

        proxy.add(map).await.map_err(map_dbus_error)
    }

    async fn update_account(
        &self,
        account_id: &str,
        params: serde_json::Value,
    ) -> Result<(), FfiError> {
        let proxy = AccountsProxy::new(&self.dbus)
            .await
            .map_err(map_dbus_error)?;
        let mut map: HashMap<String, Value<'_>> = HashMap::new();

        if let Some(obj) = params.as_object() {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    map.insert(k.clone(), Value::from(s));
                } else if let Some(b) = v.as_bool() {
                    map.insert(k.clone(), Value::from(b));
                } else if let Some(u) = v.as_u64() {
                    map.insert(k.clone(), Value::from(u as u32));
                }
            }
        }

        proxy
            .update(account_id, map)
            .await
            .map_err(map_dbus_error)?;
        Ok(())
    }

    async fn remove_account(&self, account_id: &str, purge: bool) -> Result<(), FfiError> {
        let proxy = AccountsProxy::new(&self.dbus)
            .await
            .map_err(map_dbus_error)?;
        proxy
            .remove(account_id, purge)
            .await
            .map_err(map_dbus_error)?;
        Ok(())
    }

    async fn check_account(&self, account_id: &str) -> Result<FfiAccountCheck, FfiError> {
        let proxy = AccountsProxy::new(&self.dbus)
            .await
            .map_err(map_dbus_error)?;
        let report_map = proxy.check(account_id).await.map_err(map_dbus_error)?;

        let mut steps = Vec::new();
        let step_keys = [
            "dns",
            "tcp",
            "tls",
            "auth",
            "capabilities",
            "smtp",
            "folders",
        ];

        for step_name in step_keys {
            if let Some(val) = report_map.get(step_name) {
                // val is an a{sv} dict represented as an OwnedValue dictionary
                let (passed, detail) = if let Ok(Value::Dict(d)) = Value::try_from(val) {
                    let mut ok = false;
                    let mut det = String::new();
                    for (k, v) in d.iter() {
                        if let Ok(Value::Str(ks)) = Value::try_from(k) {
                            match ks.as_str() {
                                "ok" => {
                                    if let Ok(Value::Bool(b)) = Value::try_from(v) {
                                        ok = b;
                                    }
                                }
                                "detail" => {
                                    if let Ok(Value::Str(ds)) = Value::try_from(v) {
                                        det = ds.to_string();
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    (ok, det)
                } else {
                    (false, String::new())
                };

                steps.push(FfiCheckStep {
                    step: step_name.to_string(),
                    passed,
                    detail,
                });
            }
        }

        let overall_success = !steps.is_empty() && steps.iter().all(|s| s.passed);

        Ok(FfiAccountCheck {
            account_id: account_id.to_string(),
            steps,
            overall_success,
        })
    }

    async fn reload_accounts(&self) -> Result<(), FfiError> {
        let proxy = AccountsProxy::new(&self.dbus)
            .await
            .map_err(map_dbus_error)?;
        proxy.reload().await.map_err(map_dbus_error)?;
        Ok(())
    }

    async fn get_daemon_status(&self) -> Result<FfiDaemonStatus, FfiError> {
        let proxy = StatusProxy::new(&self.dbus).await.map_err(map_dbus_error)?;

        let state = proxy.state().await.map_err(map_dbus_error)?;
        let api_version = proxy
            .api_version()
            .await
            .map(|v| v.to_string())
            .unwrap_or_else(|_| "1".to_string());
        let daemon_version = proxy
            .daemon_version()
            .await
            .unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
        let pending_ops_count = proxy.pending_ops().await.unwrap_or(0);
        let last_sync_at = proxy
            .last_sync_at()
            .await
            .ok()
            .and_then(|t| if t > 0 { Some(t.to_string()) } else { None });

        let unread_cnt = {
            let conn = self.lock_conn()?;
            conn.query_row(
                "SELECT COUNT(*) FROM visible_items WHERE unread = 1;",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
        };

        Ok(FfiDaemonStatus {
            running: true,
            state,
            unread_count: unread_cnt,
            last_sync_at,
            pending_ops_count,
            daemon_version,
            api_version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use debloatmail_store::schema::open_in_memory;
    use tempfile::tempdir;

    #[test]
    fn test_map_dbus_error_not_found() {
        let err_name =
            zbus::names::OwnedErrorName::try_from("org.noahie.debloatmail.Error.NotFound")
                .expect("valid error name");
        let msg = zbus::message::Message::method_call("/org/test", "Test")
            .unwrap()
            .destination("org.test")
            .unwrap()
            .interface("org.test")
            .unwrap()
            .build(&())
            .unwrap();
        let zerr = zbus::Error::MethodError(err_name, Some("item 123 not found".to_string()), msg);
        let ffi_err = map_dbus_error(zerr);
        match ffi_err {
            FfiError::NotFound(msg) => assert_eq!(msg, "item 123 not found"),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn test_map_dbus_error_daemon_unavailable() {
        let err_name =
            zbus::names::OwnedErrorName::try_from("org.freedesktop.DBus.Error.ServiceUnknown")
                .expect("valid error name");
        let msg = zbus::message::Message::method_call("/org/test", "Test")
            .unwrap()
            .destination("org.test")
            .unwrap()
            .interface("org.test")
            .unwrap()
            .build(&())
            .unwrap();
        let zerr = zbus::Error::MethodError(err_name, Some("service unknown".to_string()), msg);
        let ffi_err = map_dbus_error(zerr);
        match ffi_err {
            FfiError::DaemonUnavailable(msg) => {
                assert!(msg.contains("org.freedesktop.DBus.Error.ServiceUnknown"))
            }
            other => panic!("expected DaemonUnavailable, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_dbus_engine_reads_without_daemon() {
        let tmp = tempdir().unwrap();
        let conn = open_in_memory().expect("open memory db");

        // Seed an account
        conn.execute(
            "INSERT INTO accounts (public_id, name, kind, email, enabled, provider, secret_ref, created_at, updated_at)
             VALUES ('018f3a9c-0000-7000-8000-000000000001', 'Test Acc', 'imap', 'test@example.com', 1, 'fastmail', 'secret_key', 1700000000, 1700000000);",
            [],
        ).unwrap();

        // Connect to session bus or fall back to an unstarted/mocked connection
        // Note: zbus::Connection::session() works if session bus is present; if not, we can test reads via from_parts
        // In any Linux session, session bus or a peer connection is available.
        if let Ok(dbus) = zbus::Connection::session().await {
            let engine = DbusEngine::from_parts(dbus, conn, tmp.path().to_path_buf());
            let accounts = engine
                .list_accounts()
                .expect("list_accounts should succeed");
            assert_eq!(accounts.len(), 1);
            assert_eq!(accounts[0].name, "Test Acc");

            let folders = engine
                .list_folders(None)
                .expect("list_folders should succeed");
            assert_eq!(folders.len(), 0);
        }
    }
}
