//! 只增不改：改了这里就等于改了接口约定。

/// `@snapshots` 子卷的挂载点，事务快照都放在它下面。
pub const SNAPSHOTS_DIR: &str = "/.snapshots";
/// yetxn 的账本。
pub const LEDGER_DB: &str = "/var/lib/yetxn/ledger.db";
/// yetxn 的运行时目录，回滚时用来临时挂载 Btrfs 顶层。
pub const YETXN_RUN_DIR: &str = "/run/yetxn";

/// 顶层子卷的名字（相对于 subvolid=5 的顶层）。
pub const ROOT_SUBVOL: &str = "@";
pub const VAR_SUBVOL: &str = "@var";

pub const MODELD_SOCK: &str = "/run/modeld/modeld.sock";
pub const AGENTD_TRACES: &str = "/var/lib/agentd/traces";
pub const PERMS_DB: &str = "/var/lib/yeperm/perms.db";
