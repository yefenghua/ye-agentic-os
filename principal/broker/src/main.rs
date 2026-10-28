mod actions;
mod audit;
mod polkit;

use std::collections::HashMap;
use zbus::message::Header;
use zbus::zvariant::OwnedValue;
use zbus::{interface, Connection};

struct Broker;

#[interface(name = "org.yeos.Broker1")]
impl Broker {
    async fn list_actions(&self) -> Vec<String> {
        actions::ALL_ACTIONS.iter().map(|s| s.to_string()).collect()
    }

    async fn perform(
        &self,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
        action: String,
        args: HashMap<String, OwnedValue>,
    ) -> (bool, String) {
        let sender = match hdr.sender() {
            Some(s) => s.to_string(),
            None => return (false, "no sender".to_string()),
        };
        // 身份查不到也要记审计，用 u32::MAX 标出来。
        let (uid, pid) = polkit::caller_identity(conn, &sender)
            .await
            .unwrap_or((u32::MAX, 0));

        let mut entry = audit::AuditEntry {
            ts: audit::now(),
            caller_uid: uid,
            caller_pid: pid,
            caller_app: format!("uid:{uid}"),
            action: &action,
            args: audit::args_to_json(&args),
            decision: "deny",
            result: String::new(),
        };

        if !actions::is_known(&action) {
            entry.result = "unknown action".to_string();
            audit::emit(&entry);
            return (false, entry.result);
        }

        let allowed = match polkit::check_polkit(conn, &sender, &actions::polkit_id(&action)).await {
            Ok(b) => b,
            Err(e) => {
                entry.result = format!("polkit error: {e}");
                audit::emit(&entry);
                return (false, entry.result);
            }
        };
        if !allowed {
            entry.result = "not authorized".to_string();
            audit::emit(&entry);
            return (false, entry.result);
        }
        entry.decision = "allow";

        // 避免长期占着 tokio 的线程。
        let action_owned = action.clone();
        let args_owned = args.clone();
        let outcome = tokio::task::spawn_blocking(move || {
            actions::run_action(&action_owned, &args_owned)
        })
            .await;

        match outcome {
            Ok(Ok(output)) => {
                entry.result = "ok".to_string();
                audit::emit(&entry);
                (true, output)
            }
            Ok(Err(e)) => {
                entry.result = format!("error: {e}");
                audit::emit(&entry);
                (false, format!("{e}"))
            }
            Err(e) => {
                entry.result = format!("panic: {e}");
                audit::emit(&entry);
                (false, "internal error".to_string())
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let _conn = zbus::connection::Builder::system()?
        .name("org.yeos.Broker1")?
        .serve_at("/org/yeos/Broker1", Broker)?
        .build()
        .await?;
    tracing::warn!("ye-broker 已在系统总线上就位");

    std::future::pending::<()>().await;
    Ok(())
}