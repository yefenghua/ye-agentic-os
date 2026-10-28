mod btrfs;
mod ledger;
mod ops;

use clap::{Parser, Subcommand};
use serde_json::json;

#[derive(Parser)]
#[command(name = "yetxn", about = "ye-os 事务：开始、查看、提交、回滚、清理")]
struct Cli {
    /// 单行 JSON 输出，供其它程序读取
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 打快照并登记一个新任务
    Begin {
        #[arg(long)]
        summary: String,
        #[arg(long, default_value = "manual")]
        actor: String,
    },
    /// 列出任务开始以来变化的文件
    Diff { task_id: String },
    /// 接受变更
    Commit { task_id: String },
    /// 撤销变更；默认完整回滚需重启，--live 只恢复 /etc
    Rollback {
        task_id: String,
        #[arg(long)]
        live: bool,
    },
    /// 清理已关闭任务的快照
    Gc {
        #[arg(long, default_value_t = 20)]
        keep: usize,
        #[arg(long, default_value_t = 14)]
        days: u64,
    },
    /// 列出所有任务
    List,
}

fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    if let Err(e) = dispatch(cli) {
        if json {
            println!("{}", json!({ "error": format!("{e:#}") }));
        } else {
            eprintln!("错误：{e:#}");
        }
        std::process::exit(1);
    }
}

fn dispatch(cli: Cli) -> anyhow::Result<()> {
    let json = cli.json;
    match cli.cmd {
        Cmd::Begin { summary, actor } => {
            let id = ops::txn_begin(&summary, &actor)?;
            if json {
                println!("{}", json!({ "task_id": id }));
            } else {
                println!("已开始任务 {id}");
            }
        }
        Cmd::Diff { task_id } => {
            let changes = ops::txn_diff(&task_id)?;
            if json {
                println!("{}", json!({ "changed": changes }));
            } else if changes.is_empty() {
                println!("没有变化");
            } else {
                for c in &changes {
                    let mark = match c.kind.as_str() {
                        "added" => "+",
                        "deleted" => "-",
                        _ => "~",
                    };
                    println!("{mark} {}", c.path);
                }
            }
        }
        Cmd::Commit { task_id } => {
            ops::txn_commit(&task_id)?;
            if json {
                println!("{}", json!({ "task_id": task_id, "state": "committed" }));
            } else {
                println!("已提交 {task_id}");
            }
        }
        Cmd::Rollback { task_id, live } => {
            let out = ops::txn_rollback(&task_id, live)?;
            if json {
                println!("{}", serde_json::to_string(&out)?);
            } else if out.reboot_required {
                println!(
                    "已把默认根子卷切到 id {}，请执行 sudo reboot 完成回滚",
                    out.new_root_id.unwrap_or(0)
                );
            } else {
                println!("已把 /etc 恢复到任务 {task_id} 开始时的状态");
            }
        }
        Cmd::Gc { keep, days } => {
            let r = ops::txn_gc(keep, days)?;
            if json {
                println!("{}", serde_json::to_string(&r)?);
            } else {
                println!("清理 {} 个，保留 {} 个", r.pruned.len(), r.kept);
            }
        }
        Cmd::List => {
            let tasks = ledger::Ledger::open(std::path::Path::new(ye_common::paths::LEDGER_DB))?
                .list()?;
            if json {
                println!("{}", json!({ "tasks": tasks }));
            } else {
                println!("{:<36}  {:<12}  {:<8}  {}", "任务 id", "状态", "调用者", "摘要");
                for t in tasks {
                    println!("{:<36}  {:<12}  {:<8}  {}", t.id, t.state, t.actor, t.summary);
                }
            }
        }
    }
    Ok(())
}
