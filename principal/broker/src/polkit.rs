use std::collections::HashMap;
use zbus::names::BusName;
use zbus::zvariant::Value;
use zbus::{proxy, Connection};
use crate::actions::Args;
use serde::Serialize;
use serde_json::{json, Value as Json};
use std::time::{SystemTime, UNIX_EPOCH};

#[proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait Authority {
    fn check_authorization(
        &self,
        subject: &(&str, HashMap<&str, Value<'_>>),
        action_id: &str,
        details: HashMap<&str, &str>,
        flags: u32,
        cancellation_id: &str,
    ) -> zbus::Result<(bool, bool, HashMap<String, String>)>;
}

/// 从内核获取uid和pid，防止篡改
pub async fn caller_identity(conn: &Connection, sender: &str) -> anyhow::Result<(u32, u32)> {
    let dbus = zbus::fdo::DBusProxy::new(conn).await?;
    let name = BusName::try_from(sender)?;
    let uid = dbus.get_connection_unix_user(name.clone()).await?;
    let pid = dbus.get_connection_unix_process_id(name).await?;
    Ok((uid, pid))
}

pub async fn check_polkit(conn: &Connection, sender: &str, action_id: &str) -> anyhow::Result<bool> {
    let authority = AuthorityProxy::new(conn).await?;
    let mut subject = HashMap::new();
    subject.insert("name", Value::from(sender));
    let (authorized, _is_challenge, _details) = authority
        .check_authorization(
            &("system-bus-name", subject),
            action_id,
            HashMap::new(),
            1,
            "",
        )
        .await?;
    Ok(authorized)
}

