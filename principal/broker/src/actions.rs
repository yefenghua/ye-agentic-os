use std::string::String;
use std::collections::HashMap;
use std::path::{Component, Path};
use std::process::Command;
use anyhow::{bail, Context, Result};
use zbus::zvariant::{OwnedValue, Value};

pub type Args = HashMap<String, OwnedValue>;

pub const ALL_ACTIONS: &[&str] = &[
    "pkg.install",
    "pkg.remove",
    "unit.restart",
    "file.write_etc",
    "snapshot.create",
    "snapshot.commit",
    "snapshot.rollback",
];

pub fn is_known(action: &str) -> bool {
    ALL_ACTIONS.contains(&action)
}

pub fn polkit_id(action: &str) -> String {
    format!("org.yeos.broker.{action}")
}

/// 参数读取与校验
fn arg_str(args: &Args, key: &str) -> Result<String> {
    match args.get(key).map(|v| &**v) {
        Some(Value::Str(s)) => Ok(s.to_string()),
        Some(_) => bail!("参数 {key} 必须是字符串"),
        None => bail!("缺少参数 {key}"),
    }
}

fn arg_str_list(args: &Args, key: &str) -> Result<Vec<String>> {
    match args.get(key).map(|v| &**v) {
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| match v {
                Value::Str(s) => Ok(s.to_string()),
                _ => bail!("参数 {key} 的元素必须是字符串"),
            })
            .collect(),
        Some(Value::Str(s)) => Ok(vec![s.to_string()]),
        Some(_) => bail!("参数 {key} 必须是字符串数组"),
        None => bail!("缺少参数 {key}"),
    }
}

fn arg_bool(args: &Args, key: &str) -> bool {
    matches!(args.get(key).map(|v| &**v), Some(Value::Bool(true)))
}

/// 包名、单元名、actor名的通用校验
fn check_name(s: &str, what: &str) -> Result<()> {
    let ok = !s.is_empty()
        && s.len() <= 128
        && !s.starts_with('-')
        && s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.+:@".contains(c));
    if !ok {
        bail!("{what} 含非法字符或格式不对: {s:?}");
    }
    Ok(())
}

/// task_id
fn check_uuid(s: &str) -> Result<()> {
    let ok = s.len() == 36
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
        && s.matches('-').count() == 4;
    if !ok {
        bail!("task_id 不是 UUID: {s:?}");
    }
    Ok(())
}

fn run(cmd: &str, argv: &[&str], env: &[(&str, &str)]) -> Result<String> {
    let out = Command::new(cmd)
        .args(argv)
        .envs(env.iter().copied())
        .output()
        .with_context(|| format!("无法执行 {cmd}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "{cmd} 退出码 {:?}: {}",
            out.status.code(),
            stderr.trim()
        )
    }
}

fn pkg_install(args: &Args) -> Result<String> {
    let pkgs = arg_str_list(args, "packages")?;
    for p in &pkgs {
        check_name(p, "包名")?;
    }
    let mut argv = vec!["install", "-y", "-q", "--no-install-recommends"];
    argv.extend(pkgs.iter().map(String::as_str));
    run("apt-get", &argv, &[("DEBIAN_FRONTEND", "noninteractive")])
}

fn pkg_remove(args: &Args) -> Result<String> {
    let pkgs = arg_str_list(args, "packages")?;
    for p in &pkgs {
        check_name(p, "包名")?;
    }
    let mut argv = vec!["remove", "-y", "-q"];
    argv.extend(pkgs.iter().map(String::as_str));
    run("apt-get", &argv, &[("DEBIAN_FRONTEND", "noninteractive")])
}

fn unit_restart(args: &Args) -> Result<String> {
    let unit = arg_str(args, "unit")?;
    check_name(&unit, "单元名")?;
    run("systemctl", &["restart", "--", &unit], &[])?;
    Ok(format!("restarted {unit}"))
}

/// 只允许/etc下的路径
fn file_write_etc(args: &Args) -> Result<String> {
    let path = arg_str(args, "path")?;
    let content = arg_str(args, "content")?;
    let p = Path::new(&path);
    if !p.is_absolute() || !p.starts_with("/etc") {
        bail!("path 必须在 /etc 下: {path}");
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        bail!("path 不能含 ..: {path}");
    }
    let parent = p.parent().context("path 没有父目录")?;
    std::fs::create_dir_all(parent)?;
    let real_parent = parent.canonicalize()?;
    if !real_parent.starts_with("/etc") {
        bail!("path 的真实位置不在 /etc 下: {}", real_parent.display());
    }
    let tmp = format!("{path}.ye-tmp");
    std::fs::write(&tmp, content.as_bytes())?;
    std::fs::rename(&tmp, p)?;
    Ok(format!("wrote {} bytes to {path}", content.len()))
}

const YETXN: &str = "/usr/local/bin/yetxn";

/// 安装yetxn
fn yetxn(argv: &[&str]) -> Result<String> {
    if !Path::new(YETXN).exists() {
        bail!("yetxn not installed（第 2 章安装）");
    }
    run(YETXN, argv, &[])
}

fn snapshot_create(args: &Args) -> Result<String> {
    let summary = arg_str(args, "summary").unwrap_or_else(|_| "broker".to_string());
    let actor = arg_str(args, "actor").unwrap_or_else(|_| "unknown".to_string());
    check_name(&actor, "actor")?;
    if summary.starts_with('-') || summary.len() > 512 {
        bail!("summary 格式不对");
    }
    yetxn(&["begin", "--json", "--summary", &summary, "--actor", &actor])
}

fn snapshot_commit(args: &Args) -> Result<String> {
    let id = arg_str(args, "task_id")?;
    check_uuid(&id)?;
    yetxn(&["commit", "--json", &id])
}

fn snapshot_rollback(args: &Args) -> Result<String> {
    let id = arg_str(args, "task_id")?;
    check_uuid(&id)?;
    let mut argv = vec!["rollback", "--json", id.as_str()];
    if arg_bool(args, "live") {
        argv.push("--live");
    }
    yetxn(&argv)
}

/// 动作名到实现的唯一分发点。调用它之前 polkit 已经同意。
pub fn run_action(action: &str, args: &Args) -> Result<String> {
    match action {
        "pkg.install" => pkg_install(args),
        "pkg.remove" => pkg_remove(args),
        "unit.restart" => unit_restart(args),
        "file.write_etc" => file_write_etc(args),
        "snapshot.create" => snapshot_create(args),
        "snapshot.commit" => snapshot_commit(args),
        "snapshot.rollback" => snapshot_rollback(args),
        other => bail!("未知动作 {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_actions() {
        assert!(is_known("pkg.install"));
        assert!(!is_known("pkg.purge"));
        assert_eq!(polkit_id("unit.restart"), "org.yeos.broker.unit.restart");
    }

    #[test]
    fn names() {
        assert!(check_name("libssl-dev", "包名").is_ok());
        assert!(check_name("systemd-journald@ai-audit.service", "单元").is_ok());
        assert!(check_name("--allow-downgrades", "包名").is_err());
        assert!(check_name("a;rm -rf /", "包名").is_err());
        assert!(check_uuid("3b1c6d4e-9f0a-4b2c-8d7e-1a2b3c4d5e6f").is_ok());
        assert!(check_uuid("not-a-uuid").is_err());
    }

    #[test]
    fn run_echo() {
        assert_eq!(run("echo", &["hi"], &[]).unwrap().trim(), "hi");
        assert!(run("false", &[], &[]).is_err());
        assert!(run("/nonexistent/bin", &[], &[]).is_err());
    }

    fn args_with(pairs: &[(&str, &str)]) -> Args {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OwnedValue::from(zbus::zvariant::Str::from(*v))))
            .collect()
    }

    #[test]
    fn write_etc_rejects_bad_paths() {
        for bad in ["/usr/lib/x", "etc/x", "/etc/../usr/x", "/etcetera/x"] {
            let a = args_with(&[("path", bad), ("content", "x")]);
            assert!(file_write_etc(&a).is_err(), "{bad} 应被拒绝");
        }
    }
}