//! 重复文件哈希缓存：基于 SQLite 按「路径 + 大小 + 修改时间」增量持久化
//! 4KB 头部哈希、采样哈希、强采样哈希与全文件哈希（32 字节原始 BLAKE3 摘要）。
//!
//! 相比全量读写几十 MB 的 JSON：
//! - 每次扫描只按候选路径点查，耗时 O(候选数) 而非 O(全库总条数)；
//! - 仅把本次新计算或变更的条目写回磁盘，缓存命中时零写入；
//! - 不受 10 万条上限截断，全盘扫描也能稳定复用全部历史缓存。

use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub type Digest = [u8; 32];

#[derive(Clone, Default)]
pub struct Entry {
    pub size: u64,
    pub mtime: u64,
    pub head: Option<Digest>,
    pub sample: Option<Digest>,
    pub strong: Option<Digest>,
    pub full: Option<Digest>,
    pub last_seen: u64,
}

#[derive(Default)]
pub struct HashCache {
    entries: HashMap<String, Entry>,
    dirty: HashSet<String>,
    db_path: Option<PathBuf>,
}

#[derive(Deserialize)]
struct LegacyEntry {
    size: u64,
    mtime: u64,
    #[serde(default)]
    sample: String,
    #[serde(default)]
    full: String,
    #[serde(default)]
    strong: String,
    #[serde(default)]
    last_seen: u64,
}

#[derive(Deserialize)]
struct LegacyCache {
    #[serde(default)]
    entries: HashMap<String, LegacyEntry>,
}

pub fn digest_to_hex(digest: &Digest) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for &b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

pub fn hex_to_digest(hex: &str) -> Option<Digest> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let bytes = hex.as_bytes();
    for i in 0..32 {
        let hi = (bytes[i * 2] as char).to_digit(16)? as u8;
        let lo = (bytes[i * 2 + 1] as char).to_digit(16)? as u8;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn blob_to_digest(blob: Option<Vec<u8>>) -> Option<Digest> {
    let bytes = blob?;
    if bytes.len() == 32 {
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Some(out)
    } else {
        None
    }
}

impl HashCache {
    fn default_db_path() -> PathBuf {
        crate::cache_dir().join("hash-cache-v3.db")
    }

    fn open_conn(path: &Path) -> Option<Connection> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path).ok()?;
        let _ = conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             CREATE TABLE IF NOT EXISTS file_hashes (
                 path TEXT PRIMARY KEY,
                 size INTEGER NOT NULL,
                 mtime INTEGER NOT NULL,
                 head BLOB,
                 sample BLOB,
                 strong BLOB,
                 full BLOB,
                 last_seen INTEGER NOT NULL
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS idx_hashes_last_seen ON file_hashes(last_seen);",
        );
        Some(conn)
    }

    fn maybe_migrate_legacy(conn: &mut Connection) {
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM file_hashes", [], |r| r.get(0))
            .unwrap_or(1);
        if count > 0 {
            return;
        }
        let legacy_path = crate::cache_dir().join("hash-cache-v2.json");
        let Ok(text) = std::fs::read_to_string(&legacy_path) else {
            return;
        };
        let Ok(legacy) = serde_json::from_str::<LegacyCache>(&text) else {
            return;
        };
        let Ok(tx) = conn.transaction() else {
            return;
        };
        {
            let Ok(mut stmt) = tx.prepare(
                "INSERT OR IGNORE INTO file_hashes
                 (path, size, mtime, head, sample, strong, full, last_seen)
                 VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7)",
            ) else {
                return;
            };
            for (path, item) in legacy.entries {
                let sample = hex_to_digest(&item.sample);
                let strong = hex_to_digest(&item.strong);
                let full = hex_to_digest(&item.full);
                if sample.is_none() && strong.is_none() && full.is_none() {
                    continue;
                }
                let _ = stmt.execute(params![
                    path,
                    item.size as i64,
                    item.mtime as i64,
                    sample.as_ref().map(|d| d.as_slice()),
                    strong.as_ref().map(|d| d.as_slice()),
                    full.as_ref().map(|d| d.as_slice()),
                    item.last_seen as i64,
                ]);
            }
        }
        let _ = tx.commit();
    }

    /// 打开默认 SQLite 缓存库，并只预加载当前候选集涉及的路径记录。
    pub fn load_for_paths<'a, I>(paths: I) -> Self
    where
        I: IntoIterator<Item = &'a str>,
    {
        Self::load_for_paths_at(Self::default_db_path(), paths, true)
    }

    pub fn load_for_paths_at<'a, I>(db_path: PathBuf, paths: I, migrate_legacy: bool) -> Self
    where
        I: IntoIterator<Item = &'a str>,
    {
        let mut cache = HashCache {
            entries: HashMap::new(),
            dirty: HashSet::new(),
            db_path: Some(db_path.clone()),
        };
        let Some(mut conn) = Self::open_conn(&db_path) else {
            return cache;
        };
        if migrate_legacy {
            Self::maybe_migrate_legacy(&mut conn);
        }
        let Ok(tx) = conn.transaction() else {
            return cache;
        };
        {
            let Ok(mut stmt) = tx.prepare(
                "SELECT size, mtime, head, sample, strong, full, last_seen
                 FROM file_hashes WHERE path = ?1",
            ) else {
                return cache;
            };
            for path in paths {
                if cache.entries.contains_key(path) {
                    continue;
                }
                let row = stmt
                    .query_row([path], |r| {
                        let size: i64 = r.get(0)?;
                        let mtime: i64 = r.get(1)?;
                        let head: Option<Vec<u8>> = r.get(2)?;
                        let sample: Option<Vec<u8>> = r.get(3)?;
                        let strong: Option<Vec<u8>> = r.get(4)?;
                        let full: Option<Vec<u8>> = r.get(5)?;
                        let last_seen: i64 = r.get(6)?;
                        Ok(Entry {
                            size: size.max(0) as u64,
                            mtime: mtime.max(0) as u64,
                            head: blob_to_digest(head),
                            sample: blob_to_digest(sample),
                            strong: blob_to_digest(strong),
                            full: blob_to_digest(full),
                            last_seen: last_seen.max(0) as u64,
                        })
                    })
                    .optional();
                if let Ok(Some(entry)) = row {
                    cache.entries.insert(path.to_string(), entry);
                }
            }
        }
        let _ = tx.commit();
        cache
    }

    pub fn entry_for(&self, path: &str, size: u64, mtime: u64) -> Option<&Entry> {
        let entry = self.entries.get(path)?;
        if entry.size == size && entry.mtime == mtime {
            Some(entry)
        } else {
            None
        }
    }

    #[cfg(test)]
    pub fn head_digest_for(&self, path: &str, size: u64, mtime: u64) -> Option<Digest> {
        self.entry_for(path, size, mtime)?.head
    }

    #[cfg(test)]
    pub fn sample_digest_for(&self, path: &str, size: u64, mtime: u64) -> Option<Digest> {
        self.entry_for(path, size, mtime)?.sample
    }

    #[cfg(test)]
    pub fn strong_digest_for(&self, path: &str, size: u64, mtime: u64) -> Option<Digest> {
        self.entry_for(path, size, mtime)?.strong
    }

    #[cfg(test)]
    pub fn full_digest_for(&self, path: &str, size: u64, mtime: u64) -> Option<Digest> {
        self.entry_for(path, size, mtime)?.full
    }

    pub fn remember_digests(
        &mut self,
        path: String,
        size: u64,
        mtime: u64,
        head: Option<Digest>,
        sample: Option<Digest>,
        strong: Option<Digest>,
        full: Option<Digest>,
        now: u64,
    ) {
        let mut changed = false;
        let entry = self.entries.entry(path.clone()).or_insert_with(|| {
            changed = true;
            Entry {
                size,
                mtime,
                head: None,
                sample: None,
                strong: None,
                full: None,
                last_seen: now,
            }
        });
        if entry.size != size || entry.mtime != mtime {
            entry.size = size;
            entry.mtime = mtime;
            entry.head = None;
            entry.sample = None;
            entry.strong = None;
            entry.full = None;
            changed = true;
        }
        if let Some(v) = head {
            if entry.head != Some(v) {
                entry.head = Some(v);
                changed = true;
            }
        }
        if let Some(v) = sample {
            if entry.sample != Some(v) {
                entry.sample = Some(v);
                changed = true;
            }
        }
        if let Some(v) = strong {
            if entry.strong != Some(v) {
                entry.strong = Some(v);
                changed = true;
            }
        }
        if let Some(v) = full {
            if entry.full != Some(v) {
                entry.full = Some(v);
                changed = true;
            }
        }
        if changed {
            entry.last_seen = now;
            self.dirty.insert(path);
        }
    }

    /// 仅把本次新增或变化过的记录写入 SQLite；全命中缓存时直接跳过写盘。
    pub fn save_dirty(&mut self) {
        if self.dirty.is_empty() {
            return;
        }
        let Some(db_path) = self.db_path.clone() else {
            self.dirty.clear();
            return;
        };
        let Some(mut conn) = Self::open_conn(&db_path) else {
            return;
        };
        let cutoff = crate::now_seconds().saturating_sub(30 * 86400);
        let Ok(tx) = conn.transaction() else {
            return;
        };
        {
            let Ok(mut stmt) = tx.prepare(
                "INSERT INTO file_hashes
                 (path, size, mtime, head, sample, strong, full, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(path) DO UPDATE SET
                     size = excluded.size,
                     mtime = excluded.mtime,
                     head = COALESCE(excluded.head, file_hashes.head),
                     sample = COALESCE(excluded.sample, file_hashes.sample),
                     strong = COALESCE(excluded.strong, file_hashes.strong),
                     full = COALESCE(excluded.full, file_hashes.full),
                     last_seen = excluded.last_seen",
            ) else {
                return;
            };
            for path in &self.dirty {
                let Some(entry) = self.entries.get(path) else {
                    continue;
                };
                let _ = stmt.execute(params![
                    path,
                    entry.size as i64,
                    entry.mtime as i64,
                    entry.head.as_ref().map(|d| d.as_slice()),
                    entry.sample.as_ref().map(|d| d.as_slice()),
                    entry.strong.as_ref().map(|d| d.as_slice()),
                    entry.full.as_ref().map(|d| d.as_slice()),
                    entry.last_seen as i64,
                ]);
            }
        }
        let _ = tx.execute("DELETE FROM file_hashes WHERE last_seen < ?1", params![cutoff as i64]);
        if tx.commit().is_ok() {
            self.dirty.clear();
        }
    }

    #[cfg(test)]
    pub fn sample_for(&self, path: &str, size: u64, mtime: u64) -> Option<String> {
        self.sample_digest_for(path, size, mtime)
            .map(|d| digest_to_hex(&d))
    }

    #[cfg(test)]
    pub fn full_for(&self, path: &str, size: u64, mtime: u64) -> Option<String> {
        self.full_digest_for(path, size, mtime)
            .map(|d| digest_to_hex(&d))
    }

    #[cfg(test)]
    pub fn strong_for(&self, path: &str, size: u64, mtime: u64) -> Option<String> {
        self.strong_digest_for(path, size, mtime)
            .map(|d| digest_to_hex(&d))
    }

    #[cfg(test)]
    pub fn remember(
        &mut self,
        path: String,
        size: u64,
        mtime: u64,
        sample: Option<String>,
        full: Option<String>,
        strong: Option<String>,
        now: u64,
    ) {
        self.remember_digests(
            path,
            size,
            mtime,
            None,
            sample.and_then(|s| hex_to_digest(&s)),
            strong.and_then(|s| hex_to_digest(&s)),
            full.and_then(|s| hex_to_digest(&s)),
            now,
        );
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_hex(tag: u8) -> String {
        digest_to_hex(&[tag; 32])
    }

    #[test]
    fn reuses_hash_while_file_is_unchanged() {
        let mut cache = HashCache::default();
        let s = fake_hex(1);
        let f = fake_hex(2);
        cache.remember("C:\\a.bin".into(), 10, 100, Some(s.clone()), Some(f.clone()), None, 1);
        assert_eq!(cache.sample_for("C:\\a.bin", 10, 100), Some(s));
        assert_eq!(cache.full_for("C:\\a.bin", 10, 100), Some(f));
        assert_eq!(cache.sample_for("C:\\a.bin", 11, 100), None);
        assert_eq!(cache.full_for("C:\\a.bin", 10, 101), None);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn changing_file_resets_old_hash() {
        let mut cache = HashCache::default();
        let s1 = fake_hex(1);
        let f1 = fake_hex(2);
        let s2 = fake_hex(3);
        cache.remember("C:\\a.bin".into(), 10, 100, Some(s1), Some(f1), None, 1);
        cache.remember("C:\\a.bin".into(), 12, 200, Some(s2.clone()), None, None, 2);
        assert_eq!(cache.sample_for("C:\\a.bin", 12, 200), Some(s2));
        assert_eq!(cache.full_for("C:\\a.bin", 12, 200), None);
    }

    #[test]
    fn keeps_strong_and_full_hash_separately() {
        let mut cache = HashCache::default();
        let f = fake_hex(4);
        let st1 = fake_hex(5);
        let st2 = fake_hex(6);
        cache.remember("C:\\a.bin".into(), 10, 100, None, Some(f.clone()), Some(st1.clone()), 1);
        assert_eq!(cache.full_for("C:\\a.bin", 10, 100), Some(f.clone()));
        assert_eq!(cache.strong_for("C:\\a.bin", 10, 100), Some(st1));
        cache.remember("C:\\a.bin".into(), 10, 100, None, None, Some(st2.clone()), 2);
        assert_eq!(cache.full_for("C:\\a.bin", 10, 100), Some(f));
        assert_eq!(cache.strong_for("C:\\a.bin", 10, 100), Some(st2));
    }

    #[test]
    fn sqlite_persists_and_loads_only_requested_paths() {
        let dir = std::env::temp_dir().join("cdc-hashcache-sqlite-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("test.db");

        let mut c1 = HashCache::load_for_paths_at(db.clone(), std::iter::empty(), false);
        c1.remember_digests(
            "D:\\a.bin".into(),
            100,
            200,
            Some([1u8; 32]),
            Some([2u8; 32]),
            None,
            Some([3u8; 32]),
            crate::now_seconds(),
        );
        c1.remember_digests(
            "E:\\b.bin".into(),
            300,
            400,
            Some([4u8; 32]),
            None,
            None,
            None,
            crate::now_seconds(),
        );
        c1.save_dirty();

        let c2 = HashCache::load_for_paths_at(db.clone(), ["D:\\a.bin"], false);
        assert_eq!(c2.head_digest_for("D:\\a.bin", 100, 200), Some([1u8; 32]));
        assert_eq!(c2.full_digest_for("D:\\a.bin", 100, 200), Some([3u8; 32]));
        // E:\b.bin wasn't in the requested paths, so not loaded into memory
        assert_eq!(c2.head_digest_for("E:\\b.bin", 300, 400), None);

        let _ = std::fs::remove_dir_all(&dir);
    }
}