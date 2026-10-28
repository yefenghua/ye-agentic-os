use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: String,
    pub created_at: i64,
    pub state: String,
    pub summary: String,
    pub actor: String,
}

pub struct Ledger {
    conn: Connection
}

impl Ledger {
    /// 打开账本
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("创建目录 {}", dir.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("打开账本 {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS tasks(
                 id         TEXT PRIMARY KEY,
                 created_at INTEGER NOT NULL,
                 state      TEXT    NOT NULL,
                 summary    TEXT    NOT NULL,
                 actor      TEXT    NOT NULL
             );",
        )?;
        Ok(Self { conn })
    }

    /// 记一笔新任务
    pub fn insert(&self, t: &Task) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tasks(id, created_at, state, summary, actor) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![t.id, t.created_at, t.state, t.summary, t.actor],
        )?;
        Ok(())
    }

    /// 状态修改
    pub fn set_state(&self, id: &str, state: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE tasks SET state = ?2 WHERE id = ?1",
            params![id, state],
        )?;
        if n != 1 {
            bail!("账本里没有任务 {id}")
        }
        Ok(())
    }

    /// 获取账本内的内容
    pub fn get(&self, id: &str) -> Result<Task> {
        let row = self
            .conn
            .query_row(
                "SELECT id, created_at, state, summary, actor FROM tasks WHERE id = ?1",
                params![id],
                |r| {
                    Ok(Task {
                        id: r.get(0)?,
                        created_at: r.get(1)?,
                        state: r.get(2)?,
                        summary: r.get(3)?,
                        actor: r.get(4)?,
                    })
                },
            )
            .optional()?;
        row.ok_or_else(|| anyhow::anyhow!("账本里没有任务 {id}"))
    }

    /// 全部任务，时间新的在前面。
    pub fn list(&self) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, state, summary, actor FROM tasks ORDER BY created_at DESC, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Task {
                id: r.get(0)?,
                created_at: r.get(1)?,
                state: r.get(2)?,
                summary: r.get(3)?,
                actor: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_set_list() {
        let dir = std::env::temp_dir().join(format!("yetxn-test-{}", std::process::id()));
        let db = dir.join("ledger.db");
        let l = Ledger::open(&db).unwrap();
        let t = Task {
            id: "t1".into(),
            created_at: now(),
            state: "open".into(),
            summary: "测试".into(),
            actor: "test".into(),
        };
        l.insert(&t).unwrap();
        assert_eq!(l.get("t1").unwrap().state, "open");
        l.set_state("t1", "committed").unwrap();
        assert_eq!(l.get("t1").unwrap().state, "committed");
        assert!(l.set_state("nope", "open").is_err());
        assert_eq!(l.list().unwrap().len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }
}