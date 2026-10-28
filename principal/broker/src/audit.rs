use zbus::zvariant::Value;
use crate::actions::Args;
use serde_json::{json, Value as Json};
use std::time::{SystemTime, UNIX_EPOCH};
use serde::Serialize;

#[derive(Serialize)]
pub struct AuditEntry<'a> {
    pub ts: f64,
    pub caller_uid: u32,
    pub caller_pid: u32,
    pub caller_app: String,
    pub action: &'a str,
    pub args: Json,
    pub decision: &'a str,
    pub result: String,
}

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn args_to_json(args: &Args) -> Json {
    let mut m = serde_json::Map::new();
    for (k, v) in args {
        let j = match &**v {
            Value::Str(s) if k == "content" => json!({ "len": s.len() }),
            Value::Str(s) => Json::String(s.to_string()),
            Value::Bool(b) => Json::Bool(*b),
            Value::Array(a) => Json::Array(
                a.iter()
                    .map(|x| match x {
                        Value::Str(s) => Json::String(s.to_string()),
                        other => Json::String(format!("{other:?}")),
                    })
                    .collect(),
            ),
            other => Json::String(format!("{other:?}")),
        };
        m.insert(k.clone(), j);
    }
    Json::Object(m)
}

pub fn emit(entry: &AuditEntry<'_>) {
    match serde_json::to_string(entry) {
        Ok(line) => eprintln!("{line}"),
        Err(e) => eprintln!("{{\"audit_error\":\"{e}\"}}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::{OwnedValue, Str};

    #[test]
    fn content_is_not_copied() {
        let mut args = Args::new();
        args.insert("path".into(), OwnedValue::from(Str::from("/etc/x")));
        args.insert("content".into(), OwnedValue::from(Str::from("secret")));
        let j = args_to_json(&args);
        assert_eq!(j["path"], "/etc/x");
        assert_eq!(j["content"]["len"], 6);
        assert!(j.to_string().find("secret").is_none());
    }
}