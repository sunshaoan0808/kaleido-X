//! P6 Hybrid search: FTS5 full-text + metadata + exact-body channels fused with RRF.
//!
//! Index store: `{data}/search.sqlite` (independent of business dbs).
//! Indexed sources: story-packs (pack / chapter / character / node / lore rows).
//! Channel weights (Scriverse-inspired): fulltext 1.0, metadata 0.8, exact 0.6;
//! score += weight / (60 + rank); matchKind priority metadata > exact > fulltext.

use crate::{CoreError, CoreResult, DataRoot};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

const RRF_K: f64 = 60.0;
const CHANNEL_WEIGHTS: [f64; 3] = [1.0, 0.8, 0.6];

fn sql_err(e: rusqlite::Error) -> CoreError {
    CoreError::Io(std::io::Error::other(e))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub kind: String,
    pub id: String,
    pub title: String,
    pub snippet: String,
    pub work_id: String,
    pub work_title: String,
    pub score: f64,
    pub match_kind: String,
}

#[derive(Debug, Clone)]
struct RawHit {
    work_id: String,
    kind: String,
    doc_id: String,
    title: String,
    body: String,
    match_kind: &'static str,
}

#[derive(Clone)]
pub struct SearchIndex {
    conn: Arc<Mutex<Connection>>,
    data: DataRoot,
}

impl SearchIndex {
    pub fn new(data: DataRoot) -> CoreResult<Self> {
        let db_path = data.root().join("search.sqlite");
        let conn = Arc::new(Mutex::new(
            Connection::open(&db_path).map_err(sql_err)?,
        ));
        {
            let g = conn.lock();
            g.pragma_update(None, "journal_mode", "WAL").map_err(sql_err)?;
            g.execute_batch(
                "CREATE TABLE IF NOT EXISTS idx_meta(
                     pack_id TEXT PRIMARY KEY,
                     updated_at TEXT NOT NULL
                 );
                 CREATE VIRTUAL TABLE IF NOT EXISTS pack_fts USING fts5(
                     work_id UNINDEXED,
                     kind UNINDEXED,
                     doc_id UNINDEXED,
                     title,
                     body,
                     tokenize='trigram'
                 );
                 -- plain mirror for LIKE (FTS5 tables translate LIKE to MATCH,
                 -- which is limited by trigram to >=3 chars; 2-char CJK queries
                 -- like 雨巷 never match otherwise)
                 CREATE TABLE IF NOT EXISTS pack_meta(
                     work_id TEXT NOT NULL,
                     kind TEXT NOT NULL,
                     doc_id TEXT NOT NULL,
                     title TEXT NOT NULL,
                     body TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_pack_meta_work ON pack_meta(work_id);",
            )
            .map_err(sql_err)?;
        }
        Ok(Self { conn, data })
    }

    /// Main entry: sync index (if stale) then run 3-channel RRF search.
    pub fn search(&self, work_id: Option<&str>, q: &str, limit: usize) -> CoreResult<Vec<SearchHit>> {
        let q = q.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 50);
        self.ensure_indexed(work_id)?;

        let per_channel = limit * 4;
        let conn = self.conn.lock();

        // ── Channel A: FTS5 fulltext (trigram requires >= 3 chars) ──
        let mut a: Vec<RawHit> = Vec::new();
        if q.chars().count() >= 3 {
            let phrase = format!("\"{}\"", sanitize_phrase(q));
            let res = if let Some(wid) = work_id {
                conn.prepare(
                    "SELECT work_id, kind, doc_id, title, body FROM pack_fts
                     WHERE pack_fts MATCH ?1 AND work_id = ?2 ORDER BY rank LIMIT ?3",
                )
                .and_then(|mut st| {
                    let rows = st.query_map(params![phrase, wid, per_channel], row_to_raw)?;
                    rows.collect::<Result<Vec<_>, _>>()
                })
            } else {
                conn.prepare(
                    "SELECT work_id, kind, doc_id, title, body FROM pack_fts
                     WHERE pack_fts MATCH ?1 ORDER BY rank LIMIT ?2",
                )
                .and_then(|mut st| {
                    let rows = st.query_map(params![phrase, per_channel], row_to_raw)?;
                    rows.collect::<Result<Vec<_>, _>>()
                })
            };
            match res {
                Ok(rows) => {
                    a = rows.into_iter().map(|mut r| { r.match_kind = "fulltext"; r }).collect();
                }
                Err(e) => {
                    tracing::warn!(err=%e, q, "fts channel failed; falling back to LIKE");
                }
            }
        }

        // ── Channel B: metadata (title/name) LIKE ──
        let like = format!("%{}%", escape_like(q));
        let mut b: Vec<RawHit> = Vec::new();
        let res = if let Some(wid) = work_id {
            conn.prepare(
                "SELECT work_id, kind, doc_id, title, body FROM pack_meta
                 WHERE title LIKE ?1 AND work_id = ?2 ORDER BY title LIMIT ?3",
            )
            .and_then(|mut st| {
                let rows = st.query_map(params![like, wid, per_channel], row_to_raw)?;
                rows.collect::<Result<Vec<_>, _>>()
            })
        } else {
            conn.prepare(
                "SELECT work_id, kind, doc_id, title, body FROM pack_meta
                 WHERE title LIKE ?1 ORDER BY title LIMIT ?2",
            )
            .and_then(|mut st| {
                let rows = st.query_map(params![like, per_channel], row_to_raw)?;
                rows.collect::<Result<Vec<_>, _>>()
            })
        };
        if let Ok(rows) = res {
            b = rows.into_iter().map(|mut r| { r.match_kind = "metadata"; r }).collect();
        }

        // ── Channel C: exact body LIKE (2-char CJK fallback + substring boost) ──
        let mut c: Vec<RawHit> = Vec::new();
        let res = if let Some(wid) = work_id {
            conn.prepare(
                "SELECT work_id, kind, doc_id, title, body FROM pack_meta
                 WHERE body LIKE ?1 AND work_id = ?2 ORDER BY title LIMIT ?3",
            )
            .and_then(|mut st| {
                let rows = st.query_map(params![like, wid, per_channel], row_to_raw)?;
                rows.collect::<Result<Vec<_>, _>>()
            })
        } else {
            conn.prepare(
                "SELECT work_id, kind, doc_id, title, body FROM pack_meta
                 WHERE body LIKE ?1 ORDER BY title LIMIT ?2",
            )
            .and_then(|mut st| {
                let rows = st.query_map(params![like, per_channel], row_to_raw)?;
                rows.collect::<Result<Vec<_>, _>>()
            })
        };
        if let Ok(rows) = res {
            c = rows.into_iter().map(|mut r| { r.match_kind = "exact"; r }).collect();
        }

        drop(conn);
        Ok(fuse(vec![a, b, c], q, limit))
    }

    /// Incrementally rebuild FTS rows for packs whose updated_at changed.
    fn ensure_indexed(&self, only_work: Option<&str>) -> CoreResult<()> {
        let packs_root = self.data.story_packs_dir();
        if !packs_root.exists() {
            return Ok(());
        }
        let conn = self.conn.lock();
        let entries: Vec<PathBuf> = fs::read_dir(&packs_root)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .filter(|p| {
                only_work.map(|w| p.file_name().map(|n| n == w).unwrap_or(false)).unwrap_or(true)
            })
            .collect();

        for dir in entries {
            let Some(pack_id) = dir.file_name().and_then(|n| n.to_str()).map(String::from) else {
                continue;
            };
            let json_path = dir.join("pack.json");
            let Ok(raw) = fs::read_to_string(&json_path) else { continue };
            let Ok(pack) = serde_json::from_str::<crate::story_tavern::StoryPack>(&raw) else {
                continue;
            };
            let updated = if pack.updated_at.is_empty() { "0".to_string() } else { pack.updated_at.clone() };

            let indexed: Option<String> = conn
                .query_row(
                    "SELECT updated_at FROM idx_meta WHERE pack_id = ?1",
                    params![pack_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_err)?;
            if indexed.as_deref() == Some(updated.as_str()) {
                continue;
            }

            tracing::info!(pack = %pack_id, "reindex pack for search");
            reindex_pack(&conn, &dir, &pack, &pack_id, &updated).map_err(sql_err)?;
        }
        Ok(())
    }
}

fn row_to_raw(r: &rusqlite::Row) -> rusqlite::Result<RawHit> {
    Ok(RawHit {
        work_id: r.get(0)?,
        kind: r.get(1)?,
        doc_id: r.get(2)?,
        title: r.get(3)?,
        body: r.get(4)?,
        match_kind: "fulltext",
    })
}

fn reindex_pack(
    conn: &Connection,
    pack_dir: &PathBuf,
    pack: &crate::story_tavern::StoryPack,
    pack_id: &str,
    updated: &str,
) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM pack_fts WHERE work_id = ?1", params![pack_id])?;
    conn.execute("DELETE FROM pack_meta WHERE work_id = ?1", params![pack_id])?;
    let mut insert = conn.prepare(
        "INSERT INTO pack_fts(work_id, kind, doc_id, title, body) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut insert_meta = conn.prepare(
        "INSERT INTO pack_meta(work_id, kind, doc_id, title, body) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    // pack row
    insert.execute(params![pack_id, "pack", pack_id, pack.title, pack.title])?;
    insert_meta.execute(params![pack_id, "pack", pack_id, pack.title, pack.title])?;
    // chapters (body from md file)
    for ch in &pack.chapters {
        let body = read_chapter_body(pack_dir, &ch.body_path);
        insert.execute(params![pack_id, "chapter", ch.id, ch.title, body])?;
        insert_meta.execute(params![pack_id, "chapter", ch.id, ch.title, body.clone()])?;
    }
    // characters
    for c in &pack.characters {
        let body = format!(
            "{}。{}。{}",
            c.role, c.personality, c.speech_style
        );
        insert.execute(params![pack_id, "character", c.id, c.name, body])?;
        insert_meta.execute(params![pack_id, "character", c.id, c.name, body.clone()])?;
    }
    // nodes
    for n in &pack.nodes {
        let body = format!("{} {}", n.entry, n.summary);
        insert.execute(params![pack_id, "node", n.id, n.title, body])?;
        insert_meta.execute(params![pack_id, "node", n.id, n.title, body.clone()])?;
    }
    // lore entries (loose JSON: id/title/description/content)
    for l in &pack.lore_entries {
        let id = l.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let title = l.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let desc = l
            .get("description")
            .or_else(|| l.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if title.is_empty() && desc.is_empty() {
            continue;
        }
        insert.execute(params![pack_id, "lore", id, title, desc])?;
        insert_meta.execute(params![pack_id, "lore", id, title, desc.clone()])?;
    }
    drop(insert);
    drop(insert_meta);
    conn.execute(
        "INSERT INTO idx_meta(pack_id, updated_at) VALUES (?1, ?2)
         ON CONFLICT(pack_id) DO UPDATE SET updated_at = excluded.updated_at",
        params![pack_id, updated],
    )?;
    Ok(())
}

fn read_chapter_body(pack_dir: &PathBuf, rel: &str) -> String {
    let rel = rel.trim_start_matches('/');
    // guard against path traversal
    if rel.contains("..") {
        return String::new();
    }
    let path = pack_dir.join(rel);
    fs::read_to_string(&path).unwrap_or_default()
}

/// RRF fusion over channels.
fn fuse(channels: Vec<Vec<RawHit>>, q: &str, limit: usize) -> Vec<SearchHit> {
    // key -> (score, match_kind_rank, RawHit)
    let mut agg: HashMap<(String, String, String), (f64, u8, RawHit)> = HashMap::new();
    for (ci, hits) in channels.into_iter().enumerate() {
        let weight = CHANNEL_WEIGHTS.get(ci).copied().unwrap_or(0.5);
        for (rank, hit) in hits.into_iter().enumerate() {
            let key = (hit.work_id.clone(), hit.kind.clone(), hit.doc_id.clone());
            let mk_rank = match hit.match_kind {
                "metadata" => 1u8,
                "exact" => 2,
                _ => 3,
            };
            let entry = agg.entry(key).or_insert((0.0, mk_rank, hit));
            entry.0 += weight / (RRF_K + rank as f64);
            if mk_rank < entry.1 {
                entry.1 = mk_rank;
            }
        }
    }
    let mut out: Vec<SearchHit> = agg
        .into_iter()
        .map(|(_, (score, mk_rank, h))| {
            let match_kind = match mk_rank {
                1 => "metadata",
                2 => "exact",
                _ => "fulltext",
            }
            .to_string();
            SearchHit {
                kind: h.kind.clone(),
                id: h.doc_id.clone(),
                title: h.title.clone(),
                snippet: build_snippet(&h, q),
                work_id: h.work_id.clone(),
                work_title: h.title.clone(),
                score,
                match_kind,
            }
        })
        .collect();
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(limit);
    out
}

/// Build a short snippet around first match of q in body/title.
fn build_snippet(h: &RawHit, q: &str) -> String {
    let mut hay = h.body.as_str();
    if hay.is_empty() {
        hay = h.title.as_str();
    }
    if let Some(pos) = hay.to_lowercase().find(&q.to_lowercase()) {
        let pos = pos.min(hay.len());
        let start = char_floor(hay, pos.saturating_sub(30));
        let end = char_floor(hay, (pos + q.len() + 60).min(hay.len()));
        let mut s = if start > 0 { "…".to_string() } else { String::new() };
        s.push_str(&hay[start..end]);
        if end < hay.len() {
            s.push('…');
        }
        s
    } else if hay.len() > 80 {
        let end = char_floor(hay, 80);
        format!("{}…", &hay[..end])
    } else {
        hay.to_string()
    }
}

/// Snap `idx` down to the nearest UTF-8 char boundary of `s`.
fn char_floor(s: &str, idx: usize) -> usize {
    let mut i = idx.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Escape LIKE wildcards.
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Strip FTS5 metacharacters from a phrase body (keeps CJK + alnum).
fn sanitize_phrase(s: &str) -> String {
    s.chars()
        .filter(|c| {
            c.is_alphanumeric()
                || (*c as u32) >= 0x4e00 && (*c as u32) <= 0x9fff
                || (*c as u32) >= 0x3400 && (*c as u32) <= 0x4dbf
                || c.is_whitespace()
        })
        .collect::<String>()
        .replace('"', "")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DataRoot;
    use std::io::Write;

    fn make_pack(root: &DataRoot, id: &str, title: &str, chapters: &[(&str, &str, &str)]) {
        let dir = root.story_packs_dir().join(id);
        fs::create_dir_all(dir.join("chapters")).unwrap();
        let chs: Vec<serde_json::Value> = chapters
            .iter()
            .map(|(cid, ctitle, fname)| {
                serde_json::json!({
                    "id": cid, "title": ctitle, "order": 0, "goals": [],
                    "nodeIds": [], "bodyPath": format!("chapters/{}", fname)
                })
            })
            .collect();
        let pack = serde_json::json!({
            "id": id, "title": title, "source": {"type": "demo", "refs": []},
            "characters": [{"id": "c1", "name": "林淡妆", "role": "青衣门杀手",
                            "contentTier": "standard", "exampleDialogs": [], "boundaries": [],
                            "personality": "冷峻", "speechStyle": "寡言"}],
            "worldBookIds": [], "chapters": chs, "nodes": [],
            "loreEntries": [{"id": "l1", "title": "青衣门", "description": "杀手组织"}],
            "defaultMode": "mainline", "maxTier": "standard", "language": "zh",
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z"
        });
        fs::write(dir.join("pack.json"), serde_json::to_string_pretty(&pack).unwrap()).unwrap();
        for (_, _, fname) in chapters {
            let mut f = fs::File::create(dir.join("chapters").join(fname)).unwrap();
            let _ = f.write_all(format!("# 第{}章 正文示例\n\n夜雨敲窗，{}站在窗前。\n", fname, title).as_bytes());
        }
    }

    fn temp_root(tag: &str) -> DataRoot {
        let dir = std::env::temp_dir().join(format!("p6_search_{}_{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        DataRoot::new(&dir).unwrap()
    }

    #[test]
    fn chinese_short_word_fallback() {
        let root = temp_root("chinese");
        make_pack(&root, "pack-1", "剑雨江湖", &[("ch01", "第一章", "ch01.md")]);
        let idx = SearchIndex::new(root).unwrap();
        // 2-char CJK query: FTS trigram skips, LIKE exact channel hits
        let hits = idx.search(None, "剑雨", 20).unwrap();
        assert!(!hits.is_empty(), "expected LIKE fallback hit");
        assert_eq!(hits[0].work_title, "剑雨江湖");
    }

    #[test]
    fn metadata_outranks_body() {
        let root = temp_root("metadata");
        make_pack(&root, "pack-1", "林淡妆传", &[("ch01", "第一章", "ch01.md")]);
        make_pack(&root, "pack-2", "另一本书", &[("ch01", "第一章", "ch01.md")]);
        let idx = SearchIndex::new(root).unwrap();
        let hits = idx.search(None, "林淡妆", 20).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].work_id, "pack-1", "title/character metadata should rank first");
    }

    #[test]
    fn work_id_filter() {
        let root = temp_root("workid");
        make_pack(&root, "pack-1", "剑雨江湖", &[("ch01", "第一章", "ch01.md")]);
        make_pack(&root, "pack-2", "另一本书", &[("ch01", "第一章", "ch01.md")]);
        let idx = SearchIndex::new(root).unwrap();
        let hits = idx.search(Some("pack-2"), "剑雨", 20).unwrap();
        assert!(hits.is_empty(), "work filter should exclude pack-1");
    }

    #[test]
    fn reindex_only_on_change() {
        let root = temp_root("reindex");
        make_pack(&root, "pack-1", "剑雨江湖", &[("ch01", "第一章", "ch01.md")]);
        let idx = SearchIndex::new(root).unwrap();
        let h1 = idx.search(None, "夜雨", 20).unwrap();
        assert!(!h1.is_empty());
        // no change: still findable
        let h2 = idx.search(None, "夜雨", 20).unwrap();
        assert!(!h2.is_empty());
    }
}
