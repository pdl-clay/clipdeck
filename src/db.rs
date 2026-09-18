//! Camada de persistência: SQLite (WAL) com busca FTS5, pins e descarte.
//!
//! O banco é o único estado compartilhado entre o daemon (escrita na captura) e a
//! CLI (leitura/escrita pontual). WAL + `busy_timeout` tornam isso seguro.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Html,
    Image,
    Files,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Html => "html",
            Kind::Image => "image",
            Kind::Files => "files",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "text" => Some(Kind::Text),
            "html" => Some(Kind::Html),
            "image" => Some(Kind::Image),
            "files" => Some(Kind::Files),
            _ => None,
        }
    }
}

/// Conteúdo pronto para entrar no histórico (saída do watcher).
#[derive(Debug, Clone)]
pub struct Captured {
    pub kind: Kind,
    pub mime: String,
    /// Texto puro: conteúdo, companion do HTML ou lista de caminhos de arquivos.
    /// Vazio para imagens.
    pub text: String,
    pub html: Option<String>,
    pub bytes: Vec<u8>,
    pub thumb: Option<Vec<u8>>,
    /// Prévia maior (1024px) para o painel de prévia do popup.
    pub preview: Option<Vec<u8>>,
    pub sha256: String,
}

/// Item do histórico. `blob` só é preenchido por [`Db::get`]; as listagens trazem
/// apenas a miniatura para não arrastar megabytes por linha.
#[derive(Debug, Clone)]
pub struct Item {
    pub id: i64,
    pub kind: Kind,
    pub mime: String,
    pub text: String,
    pub html: Option<String>,
    pub blob: Option<Vec<u8>>,
    pub thumb: Option<Vec<u8>>,
    pub bytes: i64,
    pub pinned: bool,
    pub created_at: i64,
}

impl Item {
    /// Primeira linha do conteúdo, para exibição na lista.
    pub fn preview(&self, max_chars: usize) -> String {
        let source = if self.kind == Kind::Files {
            self.text
                .lines()
                .filter_map(path_from_uri)
                .map(short_path)
                .collect::<Vec<_>>()
                .join(", ")
        } else if self.text.is_empty() {
            String::new()
        } else {
            self.text.clone()
        };
        let one_line: String = source
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let trimmed = one_line.split_whitespace().collect::<Vec<_>>().join(" ");
        if trimmed.chars().count() > max_chars {
            let mut out: String = trimmed.chars().take(max_chars.saturating_sub(1)).collect();
            out.push('…');
            out
        } else {
            trimmed
        }
    }
}

fn path_from_uri(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    Some(line.strip_prefix("file://").unwrap_or(line).to_string())
}

fn short_path(path: String) -> String {
    path.rsplit('/').next().unwrap_or(&path).to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Inserted(i64),
    Bumped(i64),
}

impl Upsert {
    pub fn id(self) -> i64 {
        match self {
            Upsert::Inserted(id) | Upsert::Bumped(id) => id,
        }
    }

    pub fn was_inserted(self) -> bool {
        matches!(self, Upsert::Inserted(_))
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub items: i64,
    pub pinned: i64,
    pub bytes: i64,
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS items (
  id         INTEGER PRIMARY KEY,
  kind       TEXT    NOT NULL CHECK (kind IN ('text','html','image','files')),
  mime       TEXT    NOT NULL,
  text       TEXT    NOT NULL DEFAULT '',
  html       TEXT,
  blob       BLOB,
  thumb      BLOB,
  preview    BLOB,
  sha256     TEXT    NOT NULL UNIQUE,
  bytes      INTEGER NOT NULL,
  pinned     INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_items_created ON items(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_items_pinned ON items(pinned, created_at DESC);
CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
  text, content='items', content_rowid='id', tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER IF NOT EXISTS items_fts_ai AFTER INSERT ON items BEGIN
  INSERT INTO items_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER IF NOT EXISTS items_fts_ad AFTER DELETE ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER IF NOT EXISTS items_fts_au AFTER UPDATE ON items BEGIN
  INSERT INTO items_fts(items_fts, rowid, text) VALUES ('delete', old.id, old.text);
  INSERT INTO items_fts(rowid, text) VALUES (new.id, new.text);
END;
"#;

const LIST_COLUMNS: &str = "items.id, items.kind, items.mime, items.text, items.html, \
                            items.thumb, items.bytes, items.pinned, items.created_at";

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("criando diretório {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("abrindo banco {}", path.display()))?;
        Self::prepare(conn)
    }

    #[cfg(test)]
    fn open_temp(dir: &Path) -> Result<Self> {
        Self::open(&dir.join("history.db"))
    }

    fn prepare(conn: Connection) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(2))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let db = Self { conn };
        db.conn.execute_batch(SCHEMA).context("criando schema")?;
        // Bancos criados antes do painel de prévia não têm a coluna.
        db.ensure_column("preview", "ALTER TABLE items ADD COLUMN preview BLOB")?;
        Ok(db)
    }

    fn ensure_column(&self, name: &str, alter: &str) -> Result<()> {
        let exists: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('items') WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        if exists == 0 {
            tracing::info!("migrando banco: adicionando coluna {name}");
            self.conn.execute(alter, [])?;
        }
        Ok(())
    }

    /// Insere um item novo ou, se o mesmo conteúdo já existe, sobe ele para o topo.
    pub fn upsert(&mut self, item: &Captured, cfg: &Config) -> Result<Upsert> {
        let tx = self.conn.transaction()?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM items WHERE sha256 = ?1",
                params![item.sha256],
                |row| row.get(0),
            )
            .optional()?;

        let result = match existing {
            Some(id) => {
                let created_at = next_timestamp(&tx)?;
                tx.execute(
                    "UPDATE items SET created_at = ?1 WHERE id = ?2",
                    params![created_at, id],
                )?;
                Upsert::Bumped(id)
            }
            None => {
                let created_at = next_timestamp(&tx)?;
                tx.execute(
                    "INSERT INTO items (kind, mime, text, html, blob, thumb, preview, sha256, bytes, pinned, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10)",
                    params![
                        item.kind.as_str(),
                        item.mime,
                        item.text,
                        item.html,
                        item.bytes,
                        item.thumb,
                        item.preview,
                        item.sha256,
                        item.bytes.len() as i64,
                        created_at,
                    ],
                )?;
                Upsert::Inserted(tx.last_insert_rowid())
            }
        };
        tx.commit()?;
        self.evict(cfg)?;
        Ok(result)
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<Item>> {
        let sql = format!(
            "SELECT {LIST_COLUMNS} FROM items ORDER BY pinned DESC, created_at DESC LIMIT ?1"
        );
        self.query_items(&sql, params![limit as i64])
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Item>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return self.recent(limit);
        }
        let sql = format!(
            "SELECT {LIST_COLUMNS} FROM items
             JOIN items_fts ON items_fts.rowid = items.id
             WHERE items_fts MATCH ?1
             ORDER BY items.pinned DESC, items.created_at DESC LIMIT ?2"
        );
        match self.query_items(&sql, params![fts_query(trimmed), limit as i64]) {
            Ok(items) => Ok(items),
            Err(err) => {
                tracing::debug!("busca FTS falhou ({err}); usando LIKE");
                let like = format!("%{}%", trimmed.replace('%', "\\%").replace('_', "\\_"));
                let sql = format!(
                    "SELECT {LIST_COLUMNS} FROM items WHERE text LIKE ?1 ESCAPE '\\'
                     ORDER BY pinned DESC, created_at DESC LIMIT ?2"
                );
                self.query_items(&sql, params![like, limit as i64])
            }
        }
    }

    pub fn get(&self, id: i64) -> Result<Option<Item>> {
        let sql = format!("SELECT {LIST_COLUMNS}, items.blob FROM items WHERE items.id = ?1");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(params![id], row_to_item)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Só os bytes da prévia (com a miniatura como reserva): leitura barata para
    /// atualizar o painel de prévia sem arrastar o blob original.
    pub fn preview_of(&self, id: i64) -> Result<Option<Vec<u8>>> {
        let data: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT COALESCE(preview, thumb) FROM items WHERE id = ?1",
                params![id],
                |row| row.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?
            .flatten();
        Ok(data)
    }

    /// Imagens sem prévia (capturadas antes do painel de prévia existir).
    pub fn images_without_preview(&self, limit: usize) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT id FROM items WHERE kind = 'image' AND preview IS NULL
             ORDER BY created_at DESC LIMIT ?1",
        )?;
        let ids = stmt
            .query_map(params![limit as i64], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<i64>>>()?;
        Ok(ids)
    }

    pub fn blob_of(&self, id: i64) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT blob FROM items WHERE id = ?1",
                params![id],
                |row| row.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?
            .flatten())
    }

    pub fn set_preview(&mut self, id: i64, preview: &[u8]) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE items SET preview = ?1 WHERE id = ?2",
            params![preview, id],
        )?;
        Ok(changed > 0)
    }

    pub fn set_pinned(&mut self, id: i64, pinned: bool) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE items SET pinned = ?1 WHERE id = ?2",
            params![i64::from(pinned), id],
        )?;
        Ok(changed > 0)
    }

    pub fn toggle_pinned(&mut self, id: i64) -> Result<Option<bool>> {
        let current: Option<bool> = self
            .conn
            .query_row("SELECT pinned FROM items WHERE id = ?1", params![id], |row| {
                row.get::<_, i64>(0).map(|v| v != 0)
            })
            .optional()?;
        match current {
            Some(pinned) => {
                self.set_pinned(id, !pinned)?;
                Ok(Some(!pinned))
            }
            None => Ok(None),
        }
    }

    pub fn remove(&mut self, id: i64) -> Result<bool> {
        let changed = self
            .conn
            .execute("DELETE FROM items WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    pub fn clear(&mut self, keep_pinned: bool) -> Result<usize> {
        let sql = if keep_pinned {
            "DELETE FROM items WHERE pinned = 0"
        } else {
            "DELETE FROM items"
        };
        Ok(self.conn.execute(sql, [])?)
    }

    pub fn stats(&self) -> Result<Stats> {
        let (items, pinned, bytes, oldest, newest) = self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(pinned), 0), COALESCE(SUM(bytes), 0),
                    MIN(created_at), MAX(created_at) FROM items",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                ))
            },
        )?;
        Ok(Stats {
            items,
            pinned,
            bytes,
            oldest,
            newest,
        })
    }

    pub fn integrity_check(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?)
    }

    fn query_items(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<Item>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params, row_to_item)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// Aplica `max_items` e `max_total_bytes`, sempre preservando itens fixados.
    fn evict(&mut self, cfg: &Config) -> Result<usize> {
        let mut removed = self.conn.execute(
            "DELETE FROM items WHERE pinned = 0 AND id NOT IN (
                 SELECT id FROM items ORDER BY pinned DESC, created_at DESC LIMIT ?1
             )",
            params![cfg.max_items as i64],
        )?;

        let total: i64 =
            self.conn
                .query_row("SELECT COALESCE(SUM(bytes), 0) FROM items", [], |row| {
                    row.get(0)
                })?;
        let cap = cfg.max_total_bytes as i64;
        if total > cap {
            let excess = total - cap;
            let mut stmt = self
                .conn
                .prepare("SELECT id, bytes FROM items WHERE pinned = 0 ORDER BY created_at ASC")?;
            let candidates = stmt
                .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut freed = 0i64;
            let mut ids = Vec::new();
            for (id, bytes) in candidates {
                if freed >= excess {
                    break;
                }
                freed += bytes;
                ids.push(id);
            }
            if !ids.is_empty() {
                let placeholders = ids
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                removed += self.conn.execute(
                    &format!("DELETE FROM items WHERE id IN ({placeholders})"),
                    [],
                )?;
            }
        }
        Ok(removed)
    }
}

fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
    let kind: String = row.get("kind")?;
    Ok(Item {
        id: row.get("id")?,
        kind: Kind::parse(&kind).unwrap_or(Kind::Text),
        mime: row.get("mime")?,
        text: row.get("text")?,
        html: row.get("html")?,
        thumb: row.get("thumb")?,
        bytes: row.get("bytes")?,
        pinned: row.get::<_, i64>("pinned")? != 0,
        created_at: row.get("created_at")?,
        blob: row.get("blob").ok(),
    })
}

/// Timestamp estritamente maior que o último do banco: garante ordenação estável
/// mesmo com várias cópias no mesmo milissegundo.
fn next_timestamp(conn: &Connection) -> Result<i64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let last: Option<i64> = conn.query_row("SELECT MAX(created_at) FROM items", [], |row| {
        row.get::<_, Option<i64>>(0)
    })?;
    Ok(match last {
        Some(last) if last >= now => last + 1,
        _ => now,
    })
}

/// Converte texto livre em consulta FTS5 segura: cada termo entre aspas, o último
/// como prefixo.
pub fn fts_query(input: &str) -> String {
    let terms: Vec<String> = input
        .split_whitespace()
        .map(|term| {
            let escaped = term.replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect();
    match terms.split_last() {
        Some((last, rest)) => {
            let mut out = rest.join(" ");
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&format!("{last}*"));
            out
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(dir: &tempfile::TempDir) -> Db {
        Db::open_temp(dir.path()).expect("abrir banco")
    }

    fn captured(text: &str) -> Captured {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        Captured {
            kind: Kind::Text,
            mime: "text/plain".into(),
            text: text.into(),
            html: None,
            bytes: text.as_bytes().to_vec(),
            thumb: None,
            preview: None,
            sha256: format!("{:x}", hasher.finalize()),
        }
    }

    fn image(sha: &str, bytes: usize) -> Captured {
        Captured {
            kind: Kind::Image,
            mime: "image/png".into(),
            text: String::new(),
            html: None,
            bytes: vec![0u8; bytes],
            thumb: Some(vec![1, 2, 3]),
            preview: Some(vec![4, 5, 6]),
            sha256: sha.into(),
        }
    }

    #[test]
    fn preview_of_prefere_a_previa_e_cai_na_miniatura() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = db(&dir);
        let cfg = Config::default();

        let id = db.upsert(&image("img-preview", 32), &cfg).unwrap().id();
        assert_eq!(db.preview_of(id).unwrap(), Some(vec![4, 5, 6]));

        let mut sem_previa = image("img-sem-previa", 32);
        sem_previa.preview = None;
        let outro = db.upsert(&sem_previa, &cfg).unwrap().id();
        assert_eq!(
            db.preview_of(outro).unwrap(),
            Some(vec![1, 2, 3]),
            "sem prévia, usa a miniatura"
        );

        assert_eq!(db.preview_of(9999).unwrap(), None);
    }

    #[test]
    fn encontra_imagens_sem_previa_e_grava() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = db(&dir);
        let cfg = Config::default();

        let mut sem_previa = image("sem-previa", 16);
        sem_previa.preview = None;
        let id = db.upsert(&sem_previa, &cfg).unwrap().id();
        db.upsert(&image("com-previa", 16), &cfg).unwrap();

        assert_eq!(db.images_without_preview(10).unwrap(), vec![id]);
        assert_eq!(db.blob_of(id).unwrap().unwrap().len(), 16);

        assert!(db.set_preview(id, &[9, 9]).unwrap());
        assert!(db.images_without_preview(10).unwrap().is_empty());
        assert_eq!(db.preview_of(id).unwrap(), Some(vec![9, 9]));
    }

    #[test]
    fn banco_antigo_recebe_a_coluna_de_previa() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.db");
        {
            // schema como era antes do painel de prévia
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE items (
                     id INTEGER PRIMARY KEY,
                     kind TEXT NOT NULL,
                     mime TEXT NOT NULL,
                     text TEXT NOT NULL DEFAULT '',
                     html TEXT,
                     blob BLOB,
                     thumb BLOB,
                     sha256 TEXT NOT NULL UNIQUE,
                     bytes INTEGER NOT NULL,
                     pinned INTEGER NOT NULL DEFAULT 0,
                     created_at INTEGER NOT NULL
                 );
                 INSERT INTO items (kind, mime, text, sha256, bytes, pinned, created_at)
                 VALUES ('text', 'text/plain', 'antigo', 'sha-antigo', 6, 0, 1);",
            )
            .unwrap();
        }

        let mut db = Db::open(&path).expect("abre banco antigo");
        let items = db.recent(10).unwrap();
        assert_eq!(items.len(), 1, "item antigo preservado");
        assert_eq!(items[0].text, "antigo");
        assert_eq!(db.preview_of(items[0].id).unwrap(), None);

        let id = db.upsert(&image("novo", 16), &Config::default()).unwrap().id();
        assert_eq!(
            db.preview_of(id).unwrap(),
            Some(vec![4, 5, 6]),
            "coluna nova funciona depois da migração"
        );
    }

    #[test]
    fn upsert_insere_e_bump_nao_duplica() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = db(&dir);
        let cfg = Config::default();

        let first = db.upsert(&captured("olá mundo"), &cfg).unwrap();
        assert!(first.was_inserted());
        let items = db.recent(10).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].text, "olá mundo");
        assert!(!items[0].pinned);

        let again = db.upsert(&captured("olá mundo"), &cfg).unwrap();
        assert!(!again.was_inserted(), "mesmo sha256 deve subir, não duplicar");
        assert_eq!(again.id(), first.id());
        let items = db.recent(10).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].created_at > 0);
    }

    #[test]
    fn bump_preserva_pin_e_sobe_para_o_topo() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = db(&dir);
        let cfg = Config::default();

        let a = db.upsert(&captured("primeiro"), &cfg).unwrap();
        db.upsert(&captured("segundo"), &cfg).unwrap();
        assert!(db.set_pinned(a.id(), true).unwrap());

        db.upsert(&captured("primeiro"), &cfg).unwrap();
        let items = db.recent(10).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].text, "primeiro");
        assert!(items[0].pinned, "pin sobrevive ao bump");
    }

    #[test]
    fn busca_fts_por_prefixo_e_sem_acento() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = db(&dir);
        let cfg = Config::default();
        db.upsert(&captured("hello world"), &cfg).unwrap();
        db.upsert(&captured("café com leite"), &cfg).unwrap();
        db.upsert(&captured("outra coisa"), &cfg).unwrap();

        let by_prefix = db.search("hel", 10).unwrap();
        assert_eq!(by_prefix.len(), 1);
        assert_eq!(by_prefix[0].text, "hello world");

        let sem_acento = db.search("cafe", 10).unwrap();
        assert_eq!(sem_acento.len(), 1);
        assert_eq!(sem_acento[0].text, "café com leite");

        assert!(db.search("inexistente", 10).unwrap().is_empty());
    }

    #[test]
    fn evict_respeita_max_items_e_preserva_fixados() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = db(&dir);
        let cfg = Config {
            max_items: 3,
            ..Config::default()
        };

        let pinned = db.upsert(&captured("fixado"), &cfg).unwrap().id();
        db.set_pinned(pinned, true).unwrap();
        for i in 0..5 {
            db.upsert(&captured(&format!("item {i}")), &cfg).unwrap();
        }

        let items = db.recent(10).unwrap();
        assert_eq!(items.len(), 3, "limite de itens aplicado");
        assert!(items.iter().any(|item| item.id == pinned && item.pinned));
        assert_eq!(db.stats().unwrap().items, 3);
    }

    #[test]
    fn evict_respeita_teto_de_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = db(&dir);
        let cfg = Config {
            max_items: 100,
            max_total_bytes: 250,
            max_item_bytes: u64::MAX,
            ..Config::default()
        };

        db.upsert(&image("a", 100), &cfg).unwrap();
        db.upsert(&image("b", 100), &cfg).unwrap();
        db.upsert(&image("c", 100), &cfg).unwrap();
        db.upsert(&image("d", 100), &cfg).unwrap();

        let stats = db.stats().unwrap();
        assert!(stats.bytes <= 250, "bytes: {}", stats.bytes);
        assert_eq!(stats.items, 2, "descarta os mais antigos até caber");
    }

    #[test]
    fn clear_preserva_fixados_quando_pedido() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = db(&dir);
        let cfg = Config::default();
        let pinned = db.upsert(&captured("fixado"), &cfg).unwrap().id();
        db.upsert(&captured("solto"), &cfg).unwrap();
        db.set_pinned(pinned, true).unwrap();

        assert_eq!(db.clear(true).unwrap(), 1);
        let items = db.recent(10).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, pinned);

        assert_eq!(db.clear(false).unwrap(), 1);
        assert!(db.recent(10).unwrap().is_empty());
    }

    #[test]
    fn get_traz_o_blob_e_listagem_nao() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = db(&dir);
        let cfg = Config::default();
        let id = db.upsert(&image("img", 64), &cfg).unwrap().id();

        let listed = db.recent(10).unwrap();
        assert!(
            listed[0].blob.is_none(),
            "listagem não carrega o blob da imagem"
        );
        assert!(listed[0].thumb.is_some(), "miniatura vem na listagem");

        let full = db.get(id).unwrap().expect("item existe");
        assert_eq!(full.blob.unwrap().len(), 64);
    }

    #[test]
    fn toggle_pinned_e_remove_reportam_ausencia() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = db(&dir);
        let cfg = Config::default();
        let id = db.upsert(&captured("x"), &cfg).unwrap().id();

        assert_eq!(db.toggle_pinned(id).unwrap(), Some(true));
        assert_eq!(db.toggle_pinned(id).unwrap(), Some(false));
        assert_eq!(db.toggle_pinned(999).unwrap(), None);
        assert!(db.remove(id).unwrap());
        assert!(!db.remove(id).unwrap());
    }

    #[test]
    fn preview_resume_linhas_e_caminhos() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = db(&dir);
        let cfg = Config::default();
        let text = Captured {
            kind: Kind::Text,
            text: "linha um\nlinha dois".into(),
            bytes: b"linha um\nlinha dois".to_vec(),
            sha256: "hash-text".into(),
            ..captured("x")
        };
        db.upsert(&text, &cfg).unwrap();
        let files = Captured {
            kind: Kind::Files,
            mime: "text/uri-list".into(),
            text: "file:///home/pdl/nota.txt\nfile:///home/pdl/outra.md".into(),
            bytes: b"file:///home/pdl/nota.txt".to_vec(),
            sha256: "hash-files".into(),
            ..captured("y")
        };
        db.upsert(&files, &cfg).unwrap();

        let items = db.recent(10).unwrap();
        let files_item = items.iter().find(|i| i.kind == Kind::Files).unwrap();
        assert_eq!(files_item.preview(80), "nota.txt, outra.md");
        let text_item = items.iter().find(|i| i.kind == Kind::Text).unwrap();
        assert_eq!(text_item.preview(80), "linha um linha dois");
        assert_eq!(text_item.preview(5), "linh…");
    }

    #[test]
    fn fts_query_escapa_aspas() {
        assert_eq!(fts_query("ola mundo"), "\"ola\" \"mundo\"*");
        assert_eq!(fts_query("a\"b"), "\"a\"\"b\"*");
        assert_eq!(fts_query("   "), "");
    }
}
