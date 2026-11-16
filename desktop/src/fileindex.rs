//! 持久化文件索引（Phase 1）：全量文件清单 + 摘要 + 每卷 USN 游标。
//!
//! 与 `hashcache.rs` 的 `file_hashes` 分开建库（`file-index-v1.db`），理由见设计文档
//! `~/workspace/goals/c-drive-cleaner/files/persistent-index-design.md`：
//! 索引是"全量文件清单"，摘要缓存是"摘要存储"，生命周期不同；索引损坏不影响实时扫描路径。
//! 身份是 `(volume_serial, frn)`——改名不换 FRN，盘符会变；path 只是属性。
//! 覆盖范围是 ≥ 1MB 的文件（`INDEX_MIN_SIZE`，与 MFT 快照 floor 一致）。

use crate::hashcache::Digest;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};

/// 索引覆盖的最小文件大小（1MB）。与 `mft::SNAPSHOT_FLOOR_MIN_SIZE` 及重复扫描默认 min 一致。
pub const INDEX_MIN_SIZE: u64 = 1 << 20;

pub const SCHEMA_VERSION: &str = "1";

/// 一条文件元数据记录（摘要另由 `DigestUpdate` 写）。
pub struct IndexRow {
    pub volume_serial: u64,
    pub frn: u64,
    pub path: String,
    pub size: u64,
    pub mtime: u64,
    pub last_seen: u64,
}

/// 一批摘要写回。`size` / `mtime` 是写前校验：行内容变了就不写，避免把旧摘要钉到新文件上。
pub struct DigestUpdate {
    pub volume_serial: u64,
    pub frn: u64,
    pub size: u64,
    pub mtime: u64,
    pub head: Option<Digest>,
    pub sample: Option<Digest>,
    pub strong: Option<Digest>,
    pub full: Option<Digest>,
}

pub struct FileIndex {
    conn: Connection,
}

/// USN 游标是否仍可用（与 `mft::try_mft_scan` 的快照校验同一组谓词）。
/// `stored_*` 是索引库里记的值，`current_*` / `first_usn` 是当前卷的实测值。
/// 任一不满足即断档，调用方应全量重建。Phase 2 增量维护用。
pub fn usn_state_usable(
    stored_journal_id: u64,
    stored_next_usn: i64,
    current_journal_id: u64,
    first_usn: i64,
    current_next_usn: i64,
) -> bool {
    stored_journal_id == current_journal_id
        && stored_next_usn >= first_usn
        && stored_next_usn <= current_next_usn
}

/// 哈希补算 / 索引查询的输入行：元数据 + 已有摘要（可能为 None）。
/// （volume_serial 不存——调用方已知是哪一卷。）
pub struct IndexHashRow {
    pub frn: u64,
    pub path: String,
    pub size: u64,
    pub mtime: u64,
    pub head: Option<Digest>,
    pub sample: Option<Digest>,
    pub strong: Option<Digest>,
    pub full: Option<Digest>,
}

/// 重复查询的输入行：元数据 + 四种摘要（可能为 None）。
/// （volume_serial 不存——调用方已知是哪一卷；但查询跨卷时需要它做硬链接去重，
/// 所以这里保留 volume_serial，与 IndexHashRow 不同。）
pub struct IndexDupRow {
    pub volume_serial: u64,
    pub frn: u64,
    pub path: String,
    pub size: u64,
    pub mtime: u64,
    pub head: Option<Digest>,
    pub sample: Option<Digest>,
    pub strong: Option<Digest>,
    pub full: Option<Digest>,
}

/// 大文件查询的输入行：元数据即可（不需要摘要）。
pub struct IndexBigRow {
    pub path: String,
    pub size: u64,
    pub mtime: u64,
}

fn digest_col(row: &rusqlite::Row, idx: usize) -> rusqlite::Result<Option<Digest>> {
    let v: Option<Vec<u8>> = row.get(idx)?;
    Ok(v.and_then(|b| b.try_into().ok()))
}

impl FileIndex {
    pub fn default_db_path() -> PathBuf {
        crate::cache_dir().join("file-index-v1.db")
    }

    pub fn open_default() -> Option<Self> {
        Self::open(&Self::default_db_path())
    }

    pub fn open(path: &Path) -> Option<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path).ok()?;
        let _ = conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             CREATE TABLE IF NOT EXISTS index_meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS files (
                 volume_serial INTEGER NOT NULL,
                 frn INTEGER NOT NULL,
                 path TEXT NOT NULL,
                 size INTEGER NOT NULL,
                 mtime INTEGER NOT NULL,
                 head BLOB,
                 sample BLOB,
                 strong BLOB,
                 full BLOB,
                 last_seen INTEGER NOT NULL,
                 PRIMARY KEY (volume_serial, frn)
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS idx_files_path ON files(path);
             CREATE INDEX IF NOT EXISTS idx_files_size_full ON files(size, full);
             CREATE TABLE IF NOT EXISTS usn_state (
                 volume_serial INTEGER PRIMARY KEY,
                 journal_id INTEGER NOT NULL,
                 next_usn INTEGER NOT NULL,
                 updated_at INTEGER NOT NULL
             ) WITHOUT ROWID;",
        );
        let _ = conn.execute(
            "INSERT OR IGNORE INTO index_meta (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION],
        );
        Some(FileIndex { conn })
    }

    /// 批量写入元数据。同一 `(volume_serial, frn)` 已存在时更新 path/size/mtime；
    /// size 或 mtime 变了则摘要全部清 NULL（内容可能变了），否则保留已有摘要。
    pub fn upsert_files(&mut self, rows: &[IndexRow]) -> usize {
        let Ok(tx) = self.conn.transaction() else {
            return 0;
        };
        let mut written = 0;
        {
            let Ok(mut stmt) = tx.prepare(
                "INSERT INTO files
                 (volume_serial, frn, path, size, mtime, head, sample, strong, full, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, NULL, NULL, ?6)
                 ON CONFLICT(volume_serial, frn) DO UPDATE SET
                     path = excluded.path,
                     size = excluded.size,
                     mtime = excluded.mtime,
                     head = CASE WHEN files.size <> excluded.size OR files.mtime <> excluded.mtime THEN NULL ELSE files.head END,
                     sample = CASE WHEN files.size <> excluded.size OR files.mtime <> excluded.mtime THEN NULL ELSE files.sample END,
                     strong = CASE WHEN files.size <> excluded.size OR files.mtime <> excluded.mtime THEN NULL ELSE files.strong END,
                     full = CASE WHEN files.size <> excluded.size OR files.mtime <> excluded.mtime THEN NULL ELSE files.full END,
                     last_seen = excluded.last_seen",
            ) else {
                return 0;
            };
            for row in rows {
                if stmt
                    .execute(params![
                        row.volume_serial as i64,
                        row.frn as i64,
                        row.path,
                        row.size as i64,
                        row.mtime as i64,
                        row.last_seen as i64,
                    ])
                    .is_ok()
                {
                    written += 1;
                }
            }
        }
        if tx.commit().is_ok() {
            written
        } else {
            0
        }
    }

    /// 批量写回摘要。`COALESCE` 只补空缺（不覆盖已有摘要）；
    /// `size` / `mtime` 对不上的行不写（行内容在哈希算出后又变了）。
    /// 返回实际更新的行数。
    pub fn set_digests_batch(&mut self, updates: &[DigestUpdate]) -> usize {
        let Ok(tx) = self.conn.transaction() else {
            return 0;
        };
        let mut updated = 0;
        {
            let Ok(mut stmt) = tx.prepare(
                "UPDATE files SET
                     head = COALESCE(?5, head),
                     sample = COALESCE(?6, sample),
                     strong = COALESCE(?7, strong),
                     full = COALESCE(?8, full),
                     last_seen = ?9
                 WHERE volume_serial = ?1 AND frn = ?2 AND size = ?3 AND mtime = ?4",
            ) else {
                return 0;
            };
            for u in updates {
                let Ok(n) = stmt.execute(params![
                    u.volume_serial as i64,
                    u.frn as i64,
                    u.size as i64,
                    u.mtime as i64,
                    u.head.as_ref().map(|d| d.as_slice()),
                    u.sample.as_ref().map(|d| d.as_slice()),
                    u.strong.as_ref().map(|d| d.as_slice()),
                    u.full.as_ref().map(|d| d.as_slice()),
                    crate::now_seconds() as i64,
                ]) else {
                    continue;
                };
                updated += n;
            }
        }
        if tx.commit().is_ok() {
            updated
        } else {
            0
        }
    }

    pub fn set_usn_state(&mut self, volume_serial: u64, journal_id: u64, next_usn: i64) {
        let _ = self.conn.execute(
            "INSERT INTO usn_state (volume_serial, journal_id, next_usn, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(volume_serial) DO UPDATE SET
                 journal_id = excluded.journal_id,
                 next_usn = excluded.next_usn,
                 updated_at = excluded.updated_at",
            params![
                volume_serial as i64,
                journal_id as i64,
                next_usn,
                crate::now_seconds() as i64,
            ],
        );
    }

    /// Phase 2 增量维护时读游标用。
    pub fn get_usn_state(&self, volume_serial: u64) -> Option<(u64, i64)> {
        self.conn
            .query_row(
                "SELECT journal_id, next_usn FROM usn_state WHERE volume_serial = ?1",
                params![volume_serial as i64],
                |r| {
                    let journal_id: i64 = r.get(0)?;
                    let next_usn: i64 = r.get(1)?;
                    Ok((journal_id.max(0) as u64, next_usn))
                },
            )
            .ok()
    }

    /// 删掉一批 FRN 对应的行（增量维护：文件已删 / 不再符合索引条件）。
    /// 一个事务内逐条删；返回实际删除的行数。
    pub fn delete_rows(&mut self, volume_serial: u64, frns: &[u64]) -> usize {
        if frns.is_empty() {
            return 0;
        }
        let Ok(tx) = self.conn.transaction() else {
            return 0;
        };
        let mut deleted = 0;
        {
            let Ok(mut stmt) =
                tx.prepare("DELETE FROM files WHERE volume_serial = ?1 AND frn = ?2")
            else {
                return 0;
            };
            for frn in frns {
                let Ok(n) = stmt.execute(params![volume_serial as i64, *frn as i64]) else {
                    continue;
                };
                deleted += n;
            }
        }
        if tx.commit().is_ok() {
            deleted
        } else {
            0
        }
    }

    /// 目录改名/移动后，把该目录下所有索引行的 path 前缀批量改写。
    /// `old_prefix` / `new_prefix` 是不带尾斜杠的目录路径；函数内部补 `\`，
    /// 只匹配"目录下"（`C:\旧名\...`），不误伤 `C:\旧名2\...`。
    /// 长度用 SQLite 的 `LENGTH()` 算（字符数），中文目录名安全。
    /// 返回改写的行数。
    pub fn update_path_prefix(
        &mut self,
        volume_serial: u64,
        old_prefix: &str,
        new_prefix: &str,
    ) -> usize {
        let old = format!("{}\\", old_prefix.trim_end_matches('\\'));
        let new = format!("{}\\", new_prefix.trim_end_matches('\\'));
        self.conn
            .execute(
                "UPDATE files
                 SET path = ?2 || SUBSTR(path, LENGTH(?3) + 1)
                 WHERE volume_serial = ?1 AND SUBSTR(path, 1, LENGTH(?3)) = ?3",
                params![volume_serial as i64, new, old],
            )
            .unwrap_or(0)
    }

    /// 哈希补算的输入：同 size ≥ 2 且组内至少一个缺 full 的组的所有行
    /// （含已有 full 的成员——哈希流水线需要整组做候选）。
    /// 按 size 排序，调用方按 size 分组。
    pub fn rows_needing_full(&self, volume_serial: u64) -> Vec<IndexHashRow> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT frn, path, size, mtime, head, sample, strong, full
             FROM files
             WHERE volume_serial = ?1 AND size IN (
                 SELECT size FROM files WHERE volume_serial = ?1
                 GROUP BY size HAVING COUNT(*) >= 2 AND COUNT(full) < COUNT(*)
             )
             ORDER BY size, frn",
        ) else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map(params![volume_serial as i64], |r| {
            Ok(IndexHashRow {
                frn: r.get::<_, i64>(0)?.max(0) as u64,
                path: r.get(1)?,
                size: r.get::<_, i64>(2)?.max(0) as u64,
                mtime: r.get::<_, i64>(3)?.max(0) as u64,
                head: digest_col(r, 4)?,
                sample: digest_col(r, 5)?,
                strong: digest_col(r, 6)?,
                full: digest_col(r, 7)?,
            })
        }) else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 清掉一卷的全部索引行和 USN 游标（`--index --rebuild` 用；下次回填从零开始）。
    pub fn delete_volume(&mut self, volume_serial: u64) {
        let _ = self.conn.execute(
            "DELETE FROM files WHERE volume_serial = ?1",
            params![volume_serial as i64],
        );
        let _ = self.conn.execute(
            "DELETE FROM usn_state WHERE volume_serial = ?1",
            params![volume_serial as i64],
        );
    }

    /// 返回（总行数，含 full 摘要的行数）。
    pub fn counts(&self) -> (u64, u64) {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))
            .unwrap_or(0);
        let with_full: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE full IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        (total.max(0) as u64, with_full.max(0) as u64)
    }

    /// 返回一卷的（总行数，含 full 摘要的行数）。
    pub fn volume_counts(&self, volume_serial: u64) -> (u64, u64) {
        let total: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE volume_serial = ?1",
                params![volume_serial as i64],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let with_full: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM files WHERE volume_serial = ?1 AND full IS NOT NULL",
                params![volume_serial as i64],
                |r| r.get(0),
            )
            .unwrap_or(0);
        (total.max(0) as u64, with_full.max(0) as u64)
    }

    /// 返回一卷 USN 游标的更新时间（unix 秒），没有游标时 None。
    pub fn usn_state_updated_at(&self, volume_serial: u64) -> Option<u64> {
        self.conn
            .query_row(
                "SELECT updated_at FROM usn_state WHERE volume_serial = ?1",
                params![volume_serial as i64],
                |r| {
                    let v: i64 = r.get(0)?;
                    Ok(v.max(0) as u64)
                },
            )
            .ok()
    }

    /// 重复查询：给定卷上"同 size ≥ 2"的组的所有行（含缺摘要的成员——调用方据此判断完整性）。
    /// `min_size` 是调用方的下限（≥ INDEX_MIN_SIZE 才有意义）。
    /// 子查询按 size 跨卷分组——跨盘的重复对也能查到。排序不保证组内连续，
    /// 调用方用 HashMap 按 (size, 摘要) 分组。
    pub fn query_dup_rows(&self, serials: &[u64], min_size: u64) -> Vec<IndexDupRow> {
        if serials.is_empty() {
            return Vec::new();
        }
        let placeholders = std::iter::repeat("?")
            .take(serials.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT volume_serial, frn, path, size, mtime, head, sample, strong, full
             FROM files
             WHERE volume_serial IN ({}) AND size >= ? AND size IN (
                 SELECT size FROM files WHERE volume_serial IN ({})
                 GROUP BY size HAVING COUNT(*) >= 2
             )
             ORDER BY size DESC",
            placeholders, placeholders
        );
        let Ok(mut stmt) = self.conn.prepare(&sql) else {
            return Vec::new();
        };
        let mut params: Vec<i64> = serials.iter().map(|s| *s as i64).collect();
        params.push(min_size as i64);
        params.extend(serials.iter().map(|s| *s as i64));
        let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok(IndexDupRow {
                volume_serial: r.get::<_, i64>(0)?.max(0) as u64,
                frn: r.get::<_, i64>(1)?.max(0) as u64,
                path: r.get(2)?,
                size: r.get::<_, i64>(3)?.max(0) as u64,
                mtime: r.get::<_, i64>(4)?.max(0) as u64,
                head: digest_col(r, 5)?,
                sample: digest_col(r, 6)?,
                strong: digest_col(r, 7)?,
                full: digest_col(r, 8)?,
            })
        }) else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 大文件查询：给定卷上 ≥ 2MB（最低档阈值）的所有行，按 size 倒序。
    /// 调用方再按扩展名分档阈值过滤（与 `scan_big` 的 Rust 侧过滤同一规则）。
    pub fn query_big_rows(&self, serials: &[u64]) -> Vec<IndexBigRow> {
        if serials.is_empty() {
            return Vec::new();
        }
        let placeholders = std::iter::repeat("?")
            .take(serials.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT path, size, mtime FROM files
             WHERE volume_serial IN ({}) AND size >= ?
             ORDER BY size DESC",
            placeholders
        );
        let Ok(mut stmt) = self.conn.prepare(&sql) else {
            return Vec::new();
        };
        let mut params: Vec<i64> = serials.iter().map(|s| *s as i64).collect();
        params.push(2 * 1024 * 1024);
        let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok(IndexBigRow {
                path: r.get(0)?,
                size: r.get::<_, i64>(1)?.max(0) as u64,
                mtime: r.get::<_, i64>(2)?.max(0) as u64,
            })
        }) else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    #[cfg(test)]
    fn row_for(&self, volume_serial: u64, frn: u64) -> Option<(String, u64, u64, bool)> {
        self.conn
            .query_row(
                "SELECT path, size, mtime, full FROM files WHERE volume_serial = ?1 AND frn = ?2",
                params![volume_serial as i64, frn as i64],
                |r| {
                    let path: String = r.get(0)?;
                    let size: i64 = r.get(1)?;
                    let mtime: i64 = r.get(2)?;
                    let full: Option<Vec<u8>> = r.get(3)?;
                    Ok((
                        path,
                        size.max(0) as u64,
                        mtime.max(0) as u64,
                        full.is_some(),
                    ))
                },
            )
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("test.db")
    }

    fn row(serial: u64, frn: u64, path: &str, size: u64, mtime: u64) -> IndexRow {
        IndexRow {
            volume_serial: serial,
            frn,
            path: path.to_string(),
            size,
            mtime,
            last_seen: 1000,
        }
    }

    #[test]
    fn usn_state_usable_cases() {
        // 日志一致、无变化：可用
        assert!(usn_state_usable(7, 500, 7, 100, 500));
        // 日志一致、有变化可追赶：可用
        assert!(usn_state_usable(7, 500, 7, 100, 900));
        // 起点就是日志起点：可用
        assert!(usn_state_usable(7, 100, 7, 100, 900));
        // journal_id 变了（日志重建）：不可用
        assert!(!usn_state_usable(7, 500, 8, 100, 900));
        // 存的游标早于日志起点（断档）：不可用
        assert!(!usn_state_usable(7, 50, 7, 100, 900));
        // 存的游标超前于当前（日志被截断/异常）：不可用
        assert!(!usn_state_usable(7, 950, 7, 100, 900));
    }

    #[test]
    fn index_roundtrip_and_digest_rules() {
        let db = tmp_db("cdc-fileindex-test");
        let mut idx = FileIndex::open(&db).unwrap();

        idx.upsert_files(&[
            row(11, 100, "C:\\a.bin", 4096, 111),
            row(11, 101, "C:\\b.bin", 4096, 222),
        ]);
        assert_eq!(idx.counts(), (2, 0));

        // 写摘要
        let n = idx.set_digests_batch(&[DigestUpdate {
            volume_serial: 11,
            frn: 100,
            size: 4096,
            mtime: 111,
            head: Some([1u8; 32]),
            sample: None,
            strong: None,
            full: Some([2u8; 32]),
        }]);
        assert_eq!(n, 1);
        assert_eq!(idx.counts(), (2, 1));

        // size/mtime 对不上的写回被拒绝
        let n = idx.set_digests_batch(&[DigestUpdate {
            volume_serial: 11,
            frn: 101,
            size: 4097,
            mtime: 222,
            head: None,
            sample: None,
            strong: None,
            full: Some([3u8; 32]),
        }]);
        assert_eq!(n, 0);
        assert_eq!(idx.counts(), (2, 1));

        // 改名（同 frn、同 size/mtime、换 path）：摘要保留
        idx.upsert_files(&[row(11, 100, "C:\\a-renamed.bin", 4096, 111)]);
        assert_eq!(
            idx.row_for(11, 100),
            Some(("C:\\a-renamed.bin".to_string(), 4096, 111, true))
        );

        // 内容变了（size 变化）：摘要清掉
        idx.upsert_files(&[row(11, 100, "C:\\a-renamed.bin", 8192, 333)]);
        assert_eq!(
            idx.row_for(11, 100),
            Some(("C:\\a-renamed.bin".to_string(), 8192, 333, false))
        );
        assert_eq!(idx.counts(), (2, 0));

        // USN 状态读写
        assert_eq!(idx.get_usn_state(11), None);
        idx.set_usn_state(11, 77, 12345);
        assert_eq!(idx.get_usn_state(11), Some((77, 12345)));
        idx.set_usn_state(11, 77, 12399);
        assert_eq!(idx.get_usn_state(11), Some((77, 12399)));

        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn delete_rows_and_prefix_rewrite() {
        let db = tmp_db("cdc-fileindex-test2");
        let mut idx = FileIndex::open(&db).unwrap();
        // 中文目录名：LENGTH() 按字符算，前缀改写不能按字节切。
        idx.upsert_files(&[
            row(11, 100, "C:\\旧目录\\a.bin", 4096, 111),
            row(11, 101, "C:\\旧目录\\子目录\\b.bin", 4096, 222),
            row(11, 102, "C:\\旧目录2\\c.bin", 4096, 333),
            row(11, 103, "C:\\其他\\d.bin", 4096, 444),
        ]);
        // 批量删行（不存在的 frn 不计）
        assert_eq!(idx.delete_rows(11, &[101, 999]), 1);
        assert_eq!(idx.counts(), (3, 0));
        assert_eq!(idx.row_for(11, 101), None);
        // 空列表不删
        assert_eq!(idx.delete_rows(11, &[]), 0);
        // 目录改名：只改写该目录下的路径，不误伤同名前缀目录
        assert_eq!(idx.update_path_prefix(11, "C:\\旧目录", "C:\\新目录"), 1);
        assert_eq!(
            idx.row_for(11, 100),
            Some(("C:\\新目录\\a.bin".to_string(), 4096, 111, false))
        );
        assert_eq!(
            idx.row_for(11, 102),
            Some(("C:\\旧目录2\\c.bin".to_string(), 4096, 333, false))
        );
        assert_eq!(
            idx.row_for(11, 103),
            Some(("C:\\其他\\d.bin".to_string(), 4096, 444, false))
        );
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn rows_needing_full_groups() {
        let db = tmp_db("cdc-fileindex-test3");
        let mut idx = FileIndex::open(&db).unwrap();
        // size=4096 的组：一个有 full，一个缺 → 整组返回
        // size=8192 的组：两个都有 full → 不返回
        // size=2048：单件 → 不返回
        idx.upsert_files(&[
            row(11, 100, "C:\\a.bin", 4096, 111),
            row(11, 101, "C:\\b.bin", 4096, 222),
            row(11, 102, "C:\\c.bin", 8192, 333),
            row(11, 103, "C:\\d.bin", 8192, 444),
            row(11, 104, "C:\\e.bin", 2048, 555),
        ]);
        for frn in [100, 102, 103] {
            idx.set_digests_batch(&[DigestUpdate {
                volume_serial: 11,
                frn,
                size: if frn == 100 { 4096 } else { 8192 },
                mtime: if frn == 100 {
                    111
                } else if frn == 102 {
                    333
                } else {
                    444
                },
                head: None,
                sample: None,
                strong: None,
                full: Some([9u8; 32]),
            }]);
        }
        let rows = idx.rows_needing_full(11);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.size == 4096));
        assert!(rows.iter().any(|r| r.frn == 100 && r.full.is_some()));
        assert!(rows.iter().any(|r| r.frn == 101 && r.full.is_none()));
        // 全量补完后不再返回
        idx.set_digests_batch(&[DigestUpdate {
            volume_serial: 11,
            frn: 101,
            size: 4096,
            mtime: 222,
            head: None,
            sample: None,
            strong: None,
            full: Some([8u8; 32]),
        }]);
        assert!(idx.rows_needing_full(11).is_empty());
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn delete_volume_clears() {
        let db = tmp_db("cdc-fileindex-test4");
        let mut idx = FileIndex::open(&db).unwrap();
        idx.upsert_files(&[
            row(11, 100, "C:\\a.bin", 4096, 111),
            row(22, 200, "D:\\a.bin", 4096, 111),
        ]);
        idx.set_usn_state(11, 77, 100);
        idx.set_usn_state(22, 88, 200);
        idx.delete_volume(11);
        assert_eq!(idx.counts(), (1, 0));
        assert_eq!(idx.get_usn_state(11), None);
        assert_eq!(idx.get_usn_state(22), Some((88, 200)));
        assert!(idx.row_for(22, 200).is_some());
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn query_dup_rows_groups() {
        let db = tmp_db("cdc-fileindex-test5");
        let mut idx = FileIndex::open(&db).unwrap();
        // size=4096：C 盘两个 + D 盘一个（跨卷组，共 3 个）
        // size=8192：C 盘一个（单件，不返回）
        // size=2048：C 盘两个但低于 min_size=4096，不返回
        idx.upsert_files(&[
            row(11, 100, "C:\\a.bin", 4096, 111),
            row(11, 101, "C:\\b.bin", 4096, 222),
            row(22, 200, "D:\\c.bin", 4096, 333),
            row(11, 102, "C:\\d.bin", 8192, 444),
            row(11, 103, "C:\\e.bin", 2048, 555),
            row(11, 104, "C:\\f.bin", 2048, 666),
        ]);
        idx.set_digests_batch(&[DigestUpdate {
            volume_serial: 11,
            frn: 100,
            size: 4096,
            mtime: 111,
            head: None,
            sample: None,
            strong: Some([5u8; 32]),
            full: Some([9u8; 32]),
        }]);
        let rows = idx.query_dup_rows(&[11, 22], 4096);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.size == 4096));
        let mut frns: Vec<u64> = rows.iter().map(|r| r.frn).collect();
        frns.sort_unstable();
        assert_eq!(frns, vec![100, 101, 200]);
        // 100 的摘要读回正确，101 缺摘要（调用方据此判断完整性）
        let r100 = rows.iter().find(|r| r.frn == 100).unwrap();
        assert_eq!(r100.full, Some([9u8; 32]));
        assert_eq!(r100.strong, Some([5u8; 32]));
        assert!(rows.iter().find(|r| r.frn == 101).unwrap().full.is_none());
        // 只查 C 盘：跨卷组只剩 2 个
        assert_eq!(idx.query_dup_rows(&[11], 4096).len(), 2);
        // min_size 抬到 8192：没有 size ≥ 8192 且 ≥2 的组
        assert!(idx.query_dup_rows(&[11, 22], 8192).is_empty());
        // 空卷列表
        assert!(idx.query_dup_rows(&[], 4096).is_empty());
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn query_big_rows_order_and_floor() {
        let db = tmp_db("cdc-fileindex-test6");
        let mut idx = FileIndex::open(&db).unwrap();
        idx.upsert_files(&[
            row(11, 100, "C:\\small.bin", 1024 * 1024, 111),
            row(11, 101, "C:\\mid.bin", 3 * 1024 * 1024, 222),
            row(11, 102, "C:\\big.bin", 100 * 1024 * 1024, 333),
            row(22, 200, "D:\\other.bin", 50 * 1024 * 1024, 444),
        ]);
        let rows = idx.query_big_rows(&[11, 22]);
        // 1MB 的行被 2MB 下限滤掉；其余按 size 倒序
        assert_eq!(
            rows.iter().map(|r| r.size).collect::<Vec<_>>(),
            vec![100 * 1024 * 1024, 50 * 1024 * 1024, 3 * 1024 * 1024]
        );
        assert_eq!(rows[0].path, "C:\\big.bin");
        assert_eq!(idx.query_big_rows(&[22]).len(), 1);
        assert!(idx.query_big_rows(&[]).is_empty());
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    #[test]
    fn volume_counts_and_updated_at() {
        let db = tmp_db("cdc-fileindex-test7");
        let mut idx = FileIndex::open(&db).unwrap();
        assert_eq!(idx.volume_counts(11), (0, 0));
        assert_eq!(idx.usn_state_updated_at(11), None);
        idx.upsert_files(&[
            row(11, 100, "C:\\a.bin", 4096, 111),
            row(11, 101, "C:\\b.bin", 4096, 222),
        ]);
        idx.set_digests_batch(&[DigestUpdate {
            volume_serial: 11,
            frn: 100,
            size: 4096,
            mtime: 111,
            head: None,
            sample: None,
            strong: None,
            full: Some([9u8; 32]),
        }]);
        assert_eq!(idx.volume_counts(11), (2, 1));
        assert_eq!(idx.volume_counts(22), (0, 0));
        idx.set_usn_state(11, 77, 12345);
        assert!(idx.usn_state_updated_at(11).is_some());
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }
}
