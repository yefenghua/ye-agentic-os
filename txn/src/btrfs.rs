use std::fmt::format;
use anyhow::{bail, Context, Result};
use std::process::Command;

pub fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("无法启动 `{cmd}`，它是否已安装并在 PATH 里？"))?;
    if !out.status.success() {
        bail!(
            "`{} {}` 退出码 {}: {}",
            cmd,
            args.join(" "),
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn btrfs(args: &[&str]) -> Result<String> {
    run("btrfs", args)
}

/// 子卷的数字id
pub fn subvol_id(path: &str) -> Result<u64> {
    let text = btrfs(&["inspect-internal", "rootid", path])?;
    text.trim()
        .parse::<u64>()
        .with_context(|| format!("无法解析 {path} 的子卷 id: {text:?}"))
}

/// 子卷当前的generation
pub fn subvol_generation(path: &str) -> Result<u64> {
    let text = btrfs(&["subvolume", "show", path])?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Generation:") {
            return rest
                .trim()
                .parse::<u64>()
                .with_context(|| format!("无法解析 generation：{line:?}"));
        }
    }
    bail!("`btrfs subvolume show {path}` 的输出里没有 Generation 行");
}