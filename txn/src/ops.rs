use crate::btrfs::{btrfs, run, subvol_generation, subvol_id};
use crate::ledger::{now, Ledger, Task};
use anyhow::{bail, ensure, Context, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::format;
use std::fs;
use std::path::{Path, PathBuf};
use ye_common::paths::{LEDGER_DB, SNAPSHOTS_DIR, VAR_SUBVOL, YETXN_RUN_DIR};

/// 需要root权限
fn ensure_root() -> Result<()> {
    if !nix::unistd::geteuid().is_root() {
        bail!("yetxn 需要 root 权限，请用 sudo，或通过第 1 章的代理调用");
    }
    Ok(())
}

/// `/.snapshots/<task_id>`
fn snapshot_dir(task_id: &str) -> PathBuf {
    PathBuf::from(SNAPSHOTS_DIR).join(task_id)
}

/// 打开账本
fn open_ledger() -> Result<Ledger> {
    Ledger::open(Path::new(LEDGER_DB))
}

pub fn txn_begin(summary: &str, actor: &str) -> Result<String> {
    ensure_root()?;
    let task_id = uuid::Uuid::new_v4().to_string();
    let dir = snapshot_dir(&task_id);
    fs::create_dir_all(&dir).with_context(|| format!("创建 {}", dir.display()))?;

    let root_snap = dir.join("root");
    let var_snap = dir.join("var");
    btrfs(&["subvolume", "snapshot", "-r", "/", root_snap.to_str().unwrap()])?;
    if let Err(e) = btrfs(&["subvolume", "snapshot", "-r", "/var", var_snap.to_str().unwrap()]) {
        // 快照路径失败了，需要回滚删除
        let _ = btrfs(&["subvolume", "delete", root_snap.to_str().unwrap()]);
        let _ = fs::remove_dir(&dir);
        return Err(e);
    }

    let task = Task {
        id: task_id.clone(),
        created_at: now(),
        state: "open".into(),
        summary: summary.to_string(),
        actor: actor.to_string(),
    };
    if let Err(e) = open_ledger().and_then(|l| l.insert(&task)) {
        let _ = delete_snapshots(&task_id);
        return Err(e.context("写账本失败，已经删除刚才的快照"));
    }
    Ok(task_id)
}

/// 删掉一个任务的两个快照和目录
pub(crate) fn delete_snapshots(task_id: &str) -> Result<()> {
    let dir = snapshot_dir(task_id);
    for name in ["root", "var"] {
        let p = dir.join(name);
        if p.exists() {
            btrfs(&["subvolume", "delete", p.to_str().unwrap()])?;
        }
    }
    if dir.exists() {
        fs::remove_dir(&dir).with_context(|| format!("删除目录 {}", dir.display()))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub path: String,
    pub kind: String,
}

/// 这些路径内容不进入差异列表
const DIFF_EXCLUDE: &[&str] = &[
    "/.snapshots",
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/tmp",
    "/var/cache",
    "/var/log",
    "/var/tmp",
    "/var/lib/yetxn",
];

fn excluded(abs: &str) -> bool {
    DIFF_EXCLUDE
        .iter()
        .any(|p| abs == *p || abs.starts_with(&format!("{p}/")))
}

/// 把子卷内的相对路径变为绝对路径
fn abs_path(prefix: &str, rel: &str) -> String {
    if prefix == "/" {
        format!("/{rel}")
    } else {
        format!("{prefix}/{rel}")
    }
}

/// 从 findnew 的输出取出绝对路径。
pub(crate) fn parse_find_new(text: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    for line in text.lines() {
        if !line.starts_with("inode ") {
            continue; // 最后一行是 "transid marker was N"
        }
        if let Some(path) = line.split_whitespace().last() {
            seen.insert(path.to_string());
        }
    }
    seen.into_iter().collect()
}

pub fn txn_diff(task_id: &str) -> Result<Vec<Change>> {
    let ledger = open_ledger()?;
    ledger.get(task_id)?; // 不存在就在这里报错
    let dir = snapshot_dir(task_id);
    ensure!(dir.join("root").exists(), "任务 {task_id} 的快照已被清理，无法比较");

    let mut all: BTreeMap<String, String> = BTreeMap::new();
    diff_subvol(&dir.join("root"), "/", &mut all)?;
    diff_subvol(&dir.join("var"), "/var", &mut all)?;
    Ok(all
        .into_iter()
        .map(|(path, kind)| Change { path, kind })
        .collect())
}

/// 比较一个快照和它对应的活动子卷，结果并入 `out`（键是绝对路径）。
fn diff_subvol(snap: &Path, live_prefix: &str, out: &mut BTreeMap<String, String>) -> Result<()> {
    let snap_str = snap.to_str().unwrap();

    // 新增与修改
    let gen = subvol_generation(snap_str)?;
    let text = btrfs(&["subvolume", "find-new", live_prefix, &gen.to_string()])?;
    for rel in parse_find_new(&text) {
        let abs = abs_path(live_prefix, &rel);
        if excluded(&abs) {
            continue;
        }
        let kind = if snap.join(&rel).symlink_metadata().is_ok() {
            "modified"
        } else {
            "added"
        };
        out.insert(abs, kind.to_string());
    }

    // 删除
    walk_missing(snap, snap, live_prefix, out)?;
    Ok(())
}

/// 递归遍历快照目录 `cur`，凡是当前系统里不存在的路径记为 deleted。
fn walk_missing(
    snap_base: &Path,
    cur: &Path,
    live_prefix: &str,
    out: &mut BTreeMap<String, String>,
) -> Result<()> {
    let entries = match fs::read_dir(cur) {
        Ok(e) => e,
        Err(_) => return Ok(()), // 权限之类的问题，跳过这一层
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let rel = path.strip_prefix(snap_base).unwrap().to_string_lossy().into_owned();
        let abs = abs_path(live_prefix, &rel);
        if excluded(&abs) {
            continue;
        }
        let live = Path::new(&abs);
        if live.symlink_metadata().is_err() {
            out.insert(abs, "deleted".to_string());
            continue; // 整个子树都没了，不往下走
        }
        // 用 symlink_metadata 判断类型，避免跟着符号链接跑出快照。
        if path.symlink_metadata()?.file_type().is_dir() {
            walk_missing(snap_base, &path, live_prefix, out)?;
        }
    }
    Ok(())
}

pub fn txn_commit(task_id: &str) -> Result<()> {
    ensure_root()?;
    let ledger = open_ledger()?;
    let t = ledger.get(task_id)?;
    ensure!(
        t.state == "open",
        "任务 {task_id} 当前状态是 {}，只有 open 状态可以提交",
        t.state
    );
    ledger.set_state(task_id, "committed")
}

#[derive(Serialize)]
pub struct RollbackOutcome {
    pub task_id: String,
    pub mode: String,            // "live" | "full"
    pub new_root_id: Option<u64>,
    pub reboot_required: bool,
}

pub fn txn_rollback(task_id: &str, live: bool) -> Result<RollbackOutcome> {
    ensure_root()?;
    let ledger = open_ledger()?;
    let t = ledger.get(task_id)?;
    ensure!(
        t.state == "open" || t.state == "commited",
        "任务 {task_id} 当前状态是 {}，无法回滚",
        t.state,
    );
    let dir = snapshot_dir(task_id);
    ensure!(dir.join("root").exists(), "任务 {task_id} 的快照已经被清理");

    if live {
        rollback_live(&dir)?;
        ledger.set_state(task_id, "rolled_back")?;
        return Ok(RollbackOutcome {
            task_id: task_id.to_string(),
            mode: "live".into(),
            new_root_id: None,
            reboot_required: false
        });
    }
    let new_root_id = rollback_full(task_id, &dir, &ledger)?;
    Ok(RollbackOutcome {
        task_id: task_id.to_string(),
        mode: "full".into(),
        new_root_id: Some(new_root_id),
        reboot_required: true,
    })
}

/// 只恢复 /etc。rsync 把目录内容而不是目录本身同步过去。
fn rollback_live(dir: &Path) -> Result<()> {
    let src = format!("{}/root/etc/", dir.display());
    run("rsync", &["-a", "--delete", &src, "/etc/"])?;
    Ok(())
}

fn mount_top() -> Result<PathBuf> {
    let dev = root_device()?;
    let top = PathBuf::from(YETXN_RUN_DIR).join("top");
    fs::create_dir_all(&top)?;
    run("mount", &["-o", "subvolid=5", &dev, top.to_str().unwrap()])?;
    Ok(top)
}

fn umount_top(top: &Path) -> Result<()> {
    run("umount", &[top.to_str().unwrap()])?;
    Ok(())
}

/// 根文件系统所在的块设备,给出形如 `/dev/vda2[/@]`
fn root_device() -> Result<String> {
    let out = run("findmnt", &["-no", "SOURCE", "/"])?;
    let dev = out.trim().split('[').next().unwrap_or("").to_string();
    ensure!(dev.starts_with("/dev/"), "无法从 findmnt 输出识别根设备：{out:?}");
    Ok(dev)
}

/// 把 fstab 里挂载点为 `/` 的那一行的 `subvol=@` 换成 `subvolid=<id>`。`path` 指向新根里的 /etc/fstab。
pub(crate) fn rewrite_fstab_root(path: &Path, new_id: u64) -> Result<()> {
    let text = fs::read_to_string(path).with_context(|| format!("读取 {}", path.display()))?;
    let mut out = String::with_capacity(text.len());
    let mut hit = false;
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let is_root = fields.len() >= 4 && fields[1] == "/" && !line.trim_start().starts_with('#');
        if is_root {
            let opts: Vec<String> = fields[3]
                .split(',')
                .map(|o| {
                    if o == "subvol=@" || o == "subvol=/@" || o.starts_with("subvolid=") {
                        hit = true;
                        format!("subvolid={new_id}")
                    } else {
                        o.to_string()
                    }
                })
                .collect();
            let mut f: Vec<String> = fields.iter().map(|s| s.to_string()).collect();
            f[3] = opts.join(",");
            out.push_str(&f.join("\t"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    ensure!(hit, "{} 里没有找到挂载 / 且带 subvol=@ 的条目", path.display());
    fs::write(path, out).with_context(|| format!("写入 {}", path.display()))
}

/// 完整回滚的主体。返回新根子卷的 id。
fn rollback_full(task_id: &str, dir: &Path, ledger: &Ledger) -> Result<u64> {
    // 首先临时挂载到/run/yetxn/top
    let short = &task_id[..8];
    let top = mount_top()?;
    let result = (|| -> Result<u64> {
        // 做一个可写快照
        let new_root = top.join(format!("@rb-{short}"));
        btrfs(&[
            "subvolume",
            "snapshot",
            dir.join("root").to_str().unwrap(),
            new_root.to_str().unwrap(),
        ])?;
        // 把新根里的 /etc/fstab 中根条目的 subvol=@ 改成 subvolid=<新 id>。
        let new_id = subvol_id(new_root.to_str().unwrap())?;
        rewrite_fstab_root(&new_root.join("etc/fstab"), new_id)?;
        btrfs(&["subvolume", "set-default", &new_id.to_string(), "/"])?;

        // @var 没法靠默认子卷切换，用换名办法
        let var_live = top.join(VAR_SUBVOL);
        let var_bak = top.join(format!("{VAR_SUBVOL}-bak-{short}"));
        fs::rename(&var_live, &var_bak)
            .with_context(|| format!("重命名 {} -> {}", var_live.display(), var_bak.display()))?;
        btrfs(&[
            "subvolume",
            "snapshot",
            dir.join("var").to_str().unwrap(),
            var_live.to_str().unwrap(),
        ])?;

        // 账本写两份，因为当前目录的db会被换掉
        ledger.set_state(task_id, "rolled_back")?;
        let new_ledger_path = var_live.join("lib/yetxn/ledger.db");
        Ledger::open(&new_ledger_path)?.set_state(task_id, "rolled_back")?;
        Ok(new_id)
    })();
    // 不管成功失败都要把顶层卸掉。
    let um = umount_top(&top);
    let new_id = result?;
    um?;
    Ok(new_id)
}

#[derive(Debug, Serialize)]
pub struct GcReport {
    pub pruned: Vec<String>,
    pub kept: usize,
}

/// 只清理 committed 和 rolled_back 的任务。
/// 只保留前keep个账本，不保留超过days的账本
pub fn txn_gc(keep: usize, days: u64) -> Result<GcReport> {
    ensure_root()?;
    let ledger = open_ledger()?;
    let cutoff = now() - (days as i64) * 86_400;
    let closed: Vec<Task> = ledger
        .list()? // 新的在前
        .into_iter()
        .filter(|t| t.state == "committed" || t.state == "rolled_back")
        .collect();

    let mut report = GcReport { pruned: Vec::new(), kept: 0 };
    for (rank, t) in closed.iter().enumerate() {
        let beyond_count = rank >= keep;
        let too_old = t.created_at < cutoff;
        if beyond_count || too_old {
            delete_snapshots(&t.id)?;
            ledger.set_state(&t.id, "pruned")?;
            report.pruned.push(t.id.clone());
        } else {
            report.kept += 1;
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_find_new_takes_last_token() {
        let text = "inode 1234 file offset 0 len 4096 disk start 0 offset 0 gen 77 flags INLINE etc/hosts\n\
                    inode 1235 file offset 0 len 10 disk start 0 offset 0 gen 78 flags NONE etc/motd\n\
                    transid marker was 80\n";
        assert_eq!(parse_find_new(text), vec!["etc/hosts", "etc/motd"]);
    }

    #[test]
    fn exclude_prefixes() {
        assert!(excluded("/var/log/syslog"));
        assert!(excluded("/.snapshots"));
        assert!(!excluded("/var/lib/dpkg/status"));
        assert!(!excluded("/etc/hosts"));
    }

    #[test]
    fn rewrite_fstab_root_replaces_only_root_line() {
        let dir = std::env::temp_dir().join(format!("yetxn-fstab-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("fstab");
        std::fs::write(
            &f,
            "# comment\n\
             LABEL=YE-ROOT  /      btrfs  subvol=@,compress=zstd  0 0\n\
             LABEL=YE-ROOT  /var   btrfs  subvol=@var             0 0\n\
             LABEL=YE-ESP   /boot  vfat   umask=0077              0 2\n",
        )
            .unwrap();
        rewrite_fstab_root(&f, 300).unwrap();
        let out = std::fs::read_to_string(&f).unwrap();
        assert!(out.contains("subvolid=300,compress=zstd"));
        assert!(out.contains("subvol=@var"));
        assert!(!out.contains("subvol=@,"));
        std::fs::remove_dir_all(dir).ok();
    }
}