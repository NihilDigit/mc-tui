//! All user-facing strings live here. UI / status code refers to them as
//! `app.lang.s().<field>` (static) or `fmt_<event>(lang, ...)` (parametric).
//! New strings: add to `Strings` + populate `EN` and `ZH`.

use std::path::Path;

use crate::{ServerAction, TabId, YamlView};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    En,
    Zh,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Zh => "zh",
        }
    }
    pub fn from_code(s: &str) -> Lang {
        match s {
            "zh" | "zh-CN" | "cn" => Lang::Zh,
            _ => Lang::En,
        }
    }
    pub fn toggle(self) -> Lang {
        match self {
            Lang::En => Lang::Zh,
            Lang::Zh => Lang::En,
        }
    }
    pub fn s(self) -> &'static Strings {
        match self {
            Lang::En => &EN,
            Lang::Zh => &ZH,
        }
    }
}

#[allow(dead_code)] // some fields populated for future tabs (backups/yaml)
pub struct Strings {
    // Status bar
    pub server_label: &'static str,
    pub level_label: &'static str,
    pub dir_label: &'static str,
    pub status_stopped: &'static str,
    // Tab names
    pub tab_worlds: &'static str,
    pub tab_whitelist: &'static str,
    pub tab_ops: &'static str,
    pub tab_config: &'static str,
    pub tab_logs: &'static str,
    pub tab_backups: &'static str,
    pub tab_ops_panel: &'static str,
    // Pane titles
    pub title_worlds: &'static str,
    pub title_whitelist: &'static str,
    pub title_ops: &'static str,
    pub title_config: &'static str,
    pub title_logs_prefix: &'static str, // prefix; full title = `<prefix><path> `
    // Hints
    pub hint_worlds: &'static str,
    pub hint_whitelist: &'static str,
    pub hint_ops: &'static str,
    pub hint_config: &'static str,
    pub hint_logs: &'static str,
    // Prompt chrome
    pub prompt_confirm_cancel: &'static str,
    pub prompt_label_player: &'static str,
    pub prompt_label_world: &'static str,
    pub prompt_label_value: &'static str,
    pub prompt_label_path: &'static str,
    pub prompt_title_add_whitelist: &'static str,
    pub prompt_title_op_player: &'static str,
    pub prompt_title_new_world: &'static str,
    pub prompt_title_change_dir: &'static str,
    // Static status / errors
    pub ready: &'static str,
    pub refreshed: &'static str,
    pub cancelled: &'static str,
    pub err_already_running: &'static str,
    pub err_not_running: &'static str,
    pub err_stop_first: &'static str,
    pub err_already_current_world: &'static str,
    pub no_logs_yet: &'static str,
    pub spawn_started: &'static str,
    // Detail-panel headers (used by hover-detail feature)
    pub detail_header: &'static str,
    pub detail_default: &'static str,
    pub detail_range: &'static str,
    pub detail_no_info: &'static str,
    pub detail_title: &'static str,
    pub detail_no_selection: &'static str,
    pub detail_no_metadata: &'static str,
    pub detail_path: &'static str,
    pub detail_size: &'static str,
    pub detail_modified: &'static str,
    pub detail_uuid: &'static str,
    pub detail_level: &'static str,
    pub detail_level_meaning: &'static str,
    pub detail_bypass: &'static str,
    pub detail_restart_required: &'static str,
    pub detail_description: &'static str,
    pub detail_playerdata_count: &'static str,
    pub detail_has_level_dat: &'static str,
    pub detail_offline_uuid_note: &'static str,
    pub detail_op_level_1: &'static str,
    pub detail_op_level_2: &'static str,
    pub detail_op_level_3: &'static str,
    pub detail_op_level_4: &'static str,
    pub detail_is_current: &'static str,
    pub detail_key: &'static str,
    pub detail_value: &'static str,
    pub detail_yes: &'static str,
    pub detail_no: &'static str,

    // v0.5 / v0.6 new tabs
    pub title_yaml_files: &'static str,
    pub title_yaml_edit_fmt: &'static str,
    pub title_backups: &'static str,
    pub title_server: &'static str,
    pub hint_yaml_files: &'static str,
    pub hint_yaml_edit: &'static str,
    pub hint_backups: &'static str,
    pub hint_server: &'static str,
    pub yaml_no_files: &'static str,
    pub yaml_branch_marker: &'static str,
    pub backups_none: &'static str,
    pub backups_age_label: &'static str,
    pub server_action_restart_now: &'static str,
    pub server_action_backup_now: &'static str,
    pub server_action_sched_restart: &'static str,
    pub server_action_sched_backup: &'static str,
    pub server_action_pregen: &'static str,
    pub server_action_systemd_status: &'static str,
    pub server_action_attach: &'static str,
    pub server_action_set_frp_address: &'static str,
    pub server_action_set_frp_container: &'static str,
    pub server_action_frp_start: &'static str,
    pub server_action_frp_stop: &'static str,
    pub server_action_frp_restart: &'static str,
    pub server_action_frp_show_logs: &'static str,
    pub server_prompt_time_title: &'static str,
    pub server_prompt_time_label: &'static str,
    pub server_prompt_radius_title: &'static str,
    pub server_prompt_radius_label: &'static str,
    pub frp_prompt_address_title: &'static str,
    pub frp_prompt_address_label: &'static str,
    pub frp_prompt_container_title: &'static str,
    pub frp_prompt_container_label: &'static str,
    pub frp_label: &'static str,
    pub frp_state_running: &'static str,
    pub frp_state_stopped: &'static str,
    pub frp_state_missing: &'static str,
    pub frp_state_unknown: &'static str,
    pub server_systemd_unit_dir: &'static str,
    pub server_systemd_unit_dir_hint: &'static str,
    pub server_pregen_no_running: &'static str,
    pub join_section_title: &'static str,
    pub join_no_interfaces: &'static str,
    pub join_port_label: &'static str,
    pub server_actions_section: &'static str,

    // v0.10 — SakuraFrp tab
    pub tab_sakurafrp: &'static str,
    pub title_sakurafrp_user: &'static str,
    pub title_sakurafrp_tunnels: &'static str,
    pub title_sakurafrp_actions: &'static str,
    pub hint_sakurafrp: &'static str,
    pub sf_user_no_token: &'static str,
    pub sf_user_loading: &'static str,
    pub sf_user_token_label: &'static str,
    pub sf_user_plan_label: &'static str,
    pub sf_user_traffic_label: &'static str,
    pub sf_tunnels_none: &'static str,
    pub sf_tunnels_loading: &'static str,
    pub sf_col_id: &'static str,
    pub sf_col_name: &'static str,
    pub sf_col_node: &'static str,
    pub sf_col_type: &'static str,
    pub sf_col_address: &'static str,
    pub sf_col_status: &'static str,
    pub sf_action_refresh: &'static str,
    pub sf_action_set_token: &'static str,
    pub sf_action_copy_address: &'static str,
    pub sf_prompt_token_title: &'static str,
    pub sf_prompt_token_label: &'static str,
    pub sf_status_online: &'static str,
    pub sf_status_offline: &'static str,
    pub sf_no_selected_tunnel: &'static str,
    pub sf_token_saved: &'static str,
    pub sf_refreshing: &'static str,
}

pub const EN: Strings = Strings {
    server_label: "server: ",
    level_label: "level: ",
    dir_label: "dir: ",
    status_stopped: "○ stopped",
    tab_worlds: "Worlds",
    tab_whitelist: "Whitelist",
    tab_ops: "Ops",
    tab_config: "Config",
    tab_logs: "Logs",
    tab_backups: "Backups",
    tab_ops_panel: "Server",
    title_worlds: " Worlds (●=current) ",
    title_whitelist: " Whitelist ",
    title_ops: " Operators (←/→ change level) ",
    title_config: " server.properties (Enter = edit) ",
    title_logs_prefix: " Logs — tail of ",
    hint_worlds: "↑/↓ select   Enter switch   N new   S start   X stop   D dir   L lang   r refresh   q quit",
    hint_whitelist: "↑/↓ select   a add   d remove   S start   X stop   D dir   L lang   r refresh   q quit",
    hint_ops: "↑/↓ select   a add   d remove   ←/→ level   S start   X stop   D dir   L lang   q quit",
    hint_config: "↑/↓ select   Enter edit   S start   X stop   D dir   L lang   r refresh   q quit",
    hint_logs: "S start   X stop   D dir   L lang   r refresh   Tab/1-8 tabs   q quit",
    prompt_confirm_cancel: "Enter = confirm    Esc = cancel",
    prompt_label_player: "player name",
    prompt_label_world: "world name",
    prompt_label_value: "value",
    prompt_label_path: "path",
    prompt_title_add_whitelist: "Add to whitelist",
    prompt_title_op_player: "Op a player",
    prompt_title_new_world: "Create new world",
    prompt_title_change_dir: "Switch server-dir",
    ready: "Ready.",
    refreshed: "Refreshed.",
    cancelled: "Cancelled.",
    err_already_running: "✗ Server already running.",
    err_not_running: "✗ Server not running.",
    err_stop_first: "✗ Stop the server first (it's running).",
    err_already_current_world: "→ Already current world.",
    no_logs_yet: "(no logs yet)",
    spawn_started: "→ Spawned start.sh (detached). Waiting for pid…",
    detail_header: "Details",
    detail_default: "Default",
    detail_range: "Range",
    detail_no_info: "(no extra info)",
    detail_title: " Details ",
    detail_no_selection: "(nothing selected)",
    detail_no_metadata: "(no metadata for this key)",
    detail_path: "Path",
    detail_size: "Size",
    detail_modified: "Modified",
    detail_uuid: "UUID",
    detail_level: "Level",
    detail_level_meaning: "Means",
    detail_bypass: "Bypasses player limit",
    detail_restart_required: "Restart required",
    detail_description: "Description",
    detail_playerdata_count: "Playerdata files",
    detail_has_level_dat: "level.dat",
    detail_offline_uuid_note: "Offline UUID = md5(\"OfflinePlayer:\" + name) (Java/Paper offline mode).",
    detail_op_level_1: "Spawn-protection bypass; basic OP commands.",
    detail_op_level_2: "Cheat-style commands (/give, /tp, /gamemode).",
    detail_op_level_3: "Multi-player admin (/ban, /kick, /op, /deop).",
    detail_op_level_4: "Server admin (/stop, /save-all, /reload).",
    detail_is_current: "Currently loaded",
    detail_key: "Key",
    detail_value: "Value",
    detail_yes: "yes",
    detail_no: "no",
    title_yaml_files: " YAML files (Enter = open) ",
    title_yaml_edit_fmt: " YAML — ", // suffix is the file path
    title_backups: " Backups ",
    title_server: " Server ops ",
    hint_yaml_files: "↑/↓ select   Enter open   r refresh   Tab/1-8 tabs   q quit",
    hint_yaml_edit: "↑/↓ select   Enter edit leaf   Esc back to files   r refresh   q quit",
    hint_backups: "↑/↓ select   r refresh   Tab/1-8 tabs   q quit",
    hint_server: "↑/↓ select   Enter run   r refresh   Tab/1-8 tabs   q quit",
    yaml_no_files: "(no known YAMLs in this server-dir)",
    yaml_branch_marker: " ▸ ",
    backups_none: "(no backups found in candidate dirs)",
    backups_age_label: "Age",
    server_action_restart_now: "Restart now (X then S)",
    server_action_backup_now: "Run backup.sh now",
    server_action_sched_restart: "Schedule daily restart…",
    server_action_sched_backup: "Schedule daily backup…",
    server_action_pregen: "Pre-generate chunks (chunky via tmux console)…",
    server_action_systemd_status: "Show systemd unit paths",
    server_action_attach: "Show `tmux attach` command",
    server_action_set_frp_address: "Set SakuraFrp public address…",
    server_action_set_frp_container: "Set SakuraFrp container name…",
    server_action_frp_start: "Start SakuraFrp container",
    server_action_frp_stop: "Stop SakuraFrp container",
    server_action_frp_restart: "Restart SakuraFrp container",
    server_action_frp_show_logs: "Copy `docker logs -f` command for SakuraFrp",
    server_prompt_time_title: "Daily time (HH:MM, 24h)",
    server_prompt_time_label: "time",
    server_prompt_radius_title: "Pre-gen radius (chunks from spawn)",
    server_prompt_radius_label: "radius",
    frp_prompt_address_title: "SakuraFrp tunnel address (host:port)",
    frp_prompt_address_label: "address",
    frp_prompt_container_title: "SakuraFrp Docker container name",
    frp_prompt_container_label: "name",
    frp_label: "SakuraFrp",
    frp_state_running: "running",
    frp_state_stopped: "stopped",
    frp_state_missing: "missing",
    frp_state_unknown: "?",
    server_systemd_unit_dir: "systemd user units",
    server_systemd_unit_dir_hint: "Run: systemctl --user daemon-reload && systemctl --user enable --now <name>.timer",
    server_pregen_no_running: "✗ Server is not running — start it first.",
    join_section_title: " Join addresses (port from server.properties) ",
    join_no_interfaces: "(no IPv4 interfaces detected — is `ip` in PATH?)",
    join_port_label: "port",
    server_actions_section: " Actions ",
    tab_sakurafrp: "SakuraFrp",
    title_sakurafrp_user: " User ",
    title_sakurafrp_tunnels: " Tunnels ",
    title_sakurafrp_actions: " Actions ",
    hint_sakurafrp: "↑/↓ select   Enter copy address   t set token   r refresh   q quit",
    sf_user_no_token: "(no token set — pick \"Set token\" below to enter one)",
    sf_user_loading: "(loading…)",
    sf_user_token_label: "token",
    sf_user_plan_label: "plan",
    sf_user_traffic_label: "traffic",
    sf_tunnels_none: "(no tunnels — create one in the SakuraFrp dashboard, then refresh)",
    sf_tunnels_loading: "(fetching tunnels…)",
    sf_col_id: "ID",
    sf_col_name: "Name",
    sf_col_node: "Node",
    sf_col_type: "Type",
    sf_col_address: "Public address",
    sf_col_status: "Online",
    sf_action_refresh: "Refresh",
    sf_action_set_token: "Set token…",
    sf_action_copy_address: "Copy public address (selected tunnel)",
    sf_prompt_token_title: "SakuraFrp API token",
    sf_prompt_token_label: "token",
    sf_status_online: "● online",
    sf_status_offline: "○ offline",
    sf_no_selected_tunnel: "✗ Select a tunnel first.",
    sf_token_saved: "✓ Token saved (~/.config/mc-tui/natfrp.token, 0600).",
    sf_refreshing: "→ Fetching from api.natfrp.com…",
};

pub const ZH: Strings = Strings {
    server_label: "服务器: ",
    level_label: "世界: ",
    dir_label: "目录: ",
    status_stopped: "○ 已停止",
    tab_worlds: "世界",
    tab_whitelist: "白名单",
    tab_ops: "管理员",
    tab_config: "配置",
    tab_logs: "日志",
    tab_backups: "备份",
    tab_ops_panel: "运维",
    title_worlds: " 世界 (●=当前) ",
    title_whitelist: " 白名单 ",
    title_ops: " 管理员 (←/→ 调整级别) ",
    title_config: " server.properties (Enter 编辑) ",
    title_logs_prefix: " 日志 — 末尾来自 ",
    hint_worlds: "↑/↓ 选择   Enter 切换   N 新建   S 启动   X 停止   D 切换目录   L 语言   r 刷新   q 退出",
    hint_whitelist: "↑/↓ 选择   a 添加   d 移除   S 启动   X 停止   D 切换目录   L 语言   r 刷新   q 退出",
    hint_ops: "↑/↓ 选择   a 添加   d 移除   ←/→ 级别   S 启动   X 停止   D 切换目录   L 语言   q 退出",
    hint_config: "↑/↓ 选择   Enter 编辑   S 启动   X 停止   D 切换目录   L 语言   r 刷新   q 退出",
    hint_logs: "S 启动   X 停止   D 切换目录   L 语言   r 刷新   Tab/1-8 切换   q 退出",
    prompt_confirm_cancel: "Enter 确认    Esc 取消",
    prompt_label_player: "玩家名",
    prompt_label_world: "世界名",
    prompt_label_value: "值",
    prompt_label_path: "路径",
    prompt_title_add_whitelist: "加入白名单",
    prompt_title_op_player: "提升为 OP",
    prompt_title_new_world: "创建新世界",
    prompt_title_change_dir: "切换 server-dir",
    ready: "就绪。",
    refreshed: "已刷新。",
    cancelled: "已取消。",
    err_already_running: "✗ 服务器已在运行。",
    err_not_running: "✗ 服务器未运行。",
    err_stop_first: "✗ 请先停止运行中的服务器。",
    err_already_current_world: "→ 已是当前世界。",
    no_logs_yet: "(暂无日志)",
    spawn_started: "→ 已分离启动 start.sh，等待 pid 出现…",
    detail_header: "详情",
    detail_default: "默认",
    detail_range: "取值",
    detail_no_info: "(暂无说明)",
    detail_title: " 详情 ",
    detail_no_selection: "(未选中)",
    detail_no_metadata: "(此键暂无元信息)",
    detail_path: "路径",
    detail_size: "大小",
    detail_modified: "修改时间",
    detail_uuid: "UUID",
    detail_level: "级别",
    detail_level_meaning: "含义",
    detail_bypass: "绕过玩家上限",
    detail_restart_required: "需要重启",
    detail_description: "说明",
    detail_playerdata_count: "玩家数据文件数",
    detail_has_level_dat: "level.dat",
    detail_offline_uuid_note: "离线 UUID = md5(\"OfflinePlayer:\" + name)（Java/Paper 离线模式）。",
    detail_op_level_1: "绕过出生点保护；基础 OP 命令。",
    detail_op_level_2: "作弊类命令 (/give, /tp, /gamemode)。",
    detail_op_level_3: "多人管理 (/ban, /kick, /op, /deop)。",
    detail_op_level_4: "服务器管理 (/stop, /save-all, /reload)。",
    detail_is_current: "正在加载",
    detail_key: "键",
    detail_value: "值",
    detail_yes: "是",
    detail_no: "否",
    title_yaml_files: " YAML 文件 (Enter 打开) ",
    title_yaml_edit_fmt: " YAML — ",
    title_backups: " 备份 ",
    title_server: " 服务器运维 ",
    hint_yaml_files: "↑/↓ 选择   Enter 打开   r 刷新   Tab/1-8 切页   q 退出",
    hint_yaml_edit: "↑/↓ 选择   Enter 编辑叶子   Esc 返回   r 刷新   q 退出",
    hint_backups: "↑/↓ 选择   r 刷新   Tab/1-8 切页   q 退出",
    hint_server: "↑/↓ 选择   Enter 执行   r 刷新   Tab/1-8 切页   q 退出",
    yaml_no_files: "(此服务器目录里没有已知 YAML)",
    yaml_branch_marker: " ▸ ",
    backups_none: "(候选目录里没找到备份)",
    backups_age_label: "时间",
    server_action_restart_now: "立即重启 (X 然后 S)",
    server_action_backup_now: "立即跑 backup.sh",
    server_action_sched_restart: "设置每日定时重启…",
    server_action_sched_backup: "设置每日定时备份…",
    server_action_pregen: "区块预加载 (经 tmux 调 chunky)…",
    server_action_systemd_status: "显示 systemd unit 路径",
    server_action_attach: "显示 `tmux attach` 命令",
    server_action_set_frp_address: "设置 SakuraFrp 公网地址…",
    server_action_set_frp_container: "设置 SakuraFrp 容器名…",
    server_action_frp_start: "启动 SakuraFrp 容器",
    server_action_frp_stop: "停止 SakuraFrp 容器",
    server_action_frp_restart: "重启 SakuraFrp 容器",
    server_action_frp_show_logs: "复制 SakuraFrp 容器日志查看命令",
    server_prompt_time_title: "每日时间 (HH:MM, 24h 制)",
    server_prompt_time_label: "时间",
    server_prompt_radius_title: "预加载半径 (出生点附近 N 区块)",
    server_prompt_radius_label: "半径",
    frp_prompt_address_title: "SakuraFrp 隧道地址 (host:port)",
    frp_prompt_address_label: "地址",
    frp_prompt_container_title: "SakuraFrp Docker 容器名",
    frp_prompt_container_label: "容器名",
    frp_label: "SakuraFrp",
    frp_state_running: "运行中",
    frp_state_stopped: "已停止",
    frp_state_missing: "未安装",
    frp_state_unknown: "?",
    server_systemd_unit_dir: "systemd 用户 unit",
    server_systemd_unit_dir_hint: "执行: systemctl --user daemon-reload && systemctl --user enable --now <name>.timer",
    server_pregen_no_running: "✗ 服务器未运行 — 请先启动。",
    join_section_title: " 连接地址（端口取自 server.properties）",
    join_no_interfaces: "(没检测到 IPv4 接口 — `ip` 命令在 PATH 里吗？)",
    join_port_label: "端口",
    server_actions_section: " 操作 ",
    tab_sakurafrp: "SakuraFrp",
    title_sakurafrp_user: " 账户 ",
    title_sakurafrp_tunnels: " 隧道 ",
    title_sakurafrp_actions: " 操作 ",
    hint_sakurafrp: "↑/↓ 选择   Enter 复制公网地址   t 设置 token   r 刷新   q 退出",
    sf_user_no_token: "(未设置 token — 在下方选「设置 token」录入)",
    sf_user_loading: "(加载中…)",
    sf_user_token_label: "token",
    sf_user_plan_label: "套餐",
    sf_user_traffic_label: "流量",
    sf_tunnels_none: "(没有隧道 — 在 SakuraFrp 后台创建后刷新)",
    sf_tunnels_loading: "(正在拉取隧道列表…)",
    sf_col_id: "ID",
    sf_col_name: "名称",
    sf_col_node: "节点",
    sf_col_type: "类型",
    sf_col_address: "公网地址",
    sf_col_status: "在线",
    sf_action_refresh: "刷新",
    sf_action_set_token: "设置 token…",
    sf_action_copy_address: "复制公网地址 (选中隧道)",
    sf_prompt_token_title: "SakuraFrp API token",
    sf_prompt_token_label: "token",
    sf_status_online: "● 在线",
    sf_status_offline: "○ 离线",
    sf_no_selected_tunnel: "✗ 请先选中一个隧道。",
    sf_token_saved: "✓ token 已保存 (~/.config/mc-tui/natfrp.token, 0600)。",
    sf_refreshing: "→ 正在请求 api.natfrp.com…",
};

// Parametric messages — return owned Strings.

pub fn tab_name(lang: Lang, id: TabId) -> &'static str {
    let s = lang.s();
    match id {
        TabId::Worlds => s.tab_worlds,
        TabId::Whitelist => s.tab_whitelist,
        TabId::Ops => s.tab_ops,
        TabId::Config => s.tab_config,
        TabId::Logs => s.tab_logs,
        TabId::Yaml => "YAML",
        TabId::Backups => s.tab_backups,
        TabId::Server => s.tab_ops_panel,
        TabId::SakuraFrp => s.tab_sakurafrp,
    }
}

pub fn hint_for(lang: Lang, id: TabId, yaml_view: &YamlView) -> &'static str {
    let s = lang.s();
    match id {
        TabId::Worlds => s.hint_worlds,
        TabId::Whitelist => s.hint_whitelist,
        TabId::Ops => s.hint_ops,
        TabId::Config => s.hint_config,
        TabId::Logs => s.hint_logs,
        TabId::Yaml => match yaml_view {
            YamlView::Files => s.hint_yaml_files,
            YamlView::Editing { .. } => s.hint_yaml_edit,
        },
        TabId::Backups => s.hint_backups,
        TabId::Server => s.hint_server,
        TabId::SakuraFrp => s.hint_sakurafrp,
    }
}

pub fn server_action_label(lang: Lang, a: ServerAction) -> &'static str {
    let s = lang.s();
    match a {
        ServerAction::RestartNow => s.server_action_restart_now,
        ServerAction::BackupNow => s.server_action_backup_now,
        ServerAction::ScheduleDailyRestart => s.server_action_sched_restart,
        ServerAction::ScheduleDailyBackup => s.server_action_sched_backup,
        ServerAction::PreGenChunks => s.server_action_pregen,
        ServerAction::OpenSystemdStatus => s.server_action_systemd_status,
        ServerAction::ShowAttachCommand => s.server_action_attach,
        ServerAction::SetSakuraFrpAddress => s.server_action_set_frp_address,
        ServerAction::SetSakuraFrpContainer => s.server_action_set_frp_container,
        ServerAction::SakuraFrpStart => s.server_action_frp_start,
        ServerAction::SakuraFrpStop => s.server_action_frp_stop,
        ServerAction::SakuraFrpRestart => s.server_action_frp_restart,
        ServerAction::SakuraFrpShowLogs => s.server_action_frp_show_logs,
    }
}

pub fn fmt_status_running(lang: Lang, pid: u32) -> String {
    match lang {
        Lang::En => format!("● running (pid {})", pid),
        Lang::Zh => format!("● 运行中 (pid {})", pid),
    }
}
pub fn fmt_world_switched(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✓ Switched to '{}'. Restart the server to load it.", name),
        Lang::Zh => format!("✓ 已切换到 '{}'。请重启服务器以加载。", name),
    }
}
pub fn fmt_world_created(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✓ level-name='{}'. Next start will generate the world.", name),
        Lang::Zh => format!("✓ level-name='{}'。下次启动将生成该世界。", name),
    }
}
pub fn fmt_world_invalid(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✗ Invalid world name: '{}'.", name),
        Lang::Zh => format!("✗ 非法世界名: '{}'。", name),
    }
}
pub fn fmt_world_exists(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✗ '{}' already exists.", name),
        Lang::Zh => format!("✗ '{}' 已存在。", name),
    }
}
pub fn fmt_dir_no_properties(lang: Lang, path: &Path) -> String {
    match lang {
        Lang::En => format!("✗ {} has no server.properties.", path.display()),
        Lang::Zh => format!("✗ {} 中没有 server.properties。", path.display()),
    }
}
pub fn fmt_dir_canon_failed(lang: Lang, path: &Path, err: &str) -> String {
    match lang {
        Lang::En => format!("✗ {}: {}", path.display(), err),
        Lang::Zh => format!("✗ {}：{}", path.display(), err),
    }
}
pub fn fmt_dir_switched(lang: Lang, path: &Path) -> String {
    match lang {
        Lang::En => format!("✓ Switched to {}.", path.display()),
        Lang::Zh => format!("✓ 已切换到 {}。", path.display()),
    }
}
pub fn fmt_already_whitelisted(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("→ '{}' already whitelisted.", name),
        Lang::Zh => format!("→ '{}' 已在白名单。", name),
    }
}
pub fn fmt_whitelist_added(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✓ Whitelisted {}.", name),
        Lang::Zh => format!("✓ 已加入白名单：{}。", name),
    }
}
pub fn fmt_whitelist_removed(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✓ Removed {} from whitelist.", name),
        Lang::Zh => format!("✓ 已从白名单移除：{}。", name),
    }
}
pub fn fmt_already_op(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("→ '{}' already op.", name),
        Lang::Zh => format!("→ '{}' 已是 OP。", name),
    }
}
pub fn fmt_op_added(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✓ Op'd {} (level 4).", name),
        Lang::Zh => format!("✓ 已设为 OP：{}（级别 4）。", name),
    }
}
pub fn fmt_op_removed(lang: Lang, name: &str) -> String {
    match lang {
        Lang::En => format!("✓ De-op'd {}.", name),
        Lang::Zh => format!("✓ 已撤销 OP：{}。", name),
    }
}
pub fn fmt_op_level_changed(lang: Lang, name: &str, level: u8) -> String {
    match lang {
        Lang::En => format!("✓ {} → level {}.", name, level),
        Lang::Zh => format!("✓ {} → 级别 {}。", name, level),
    }
}
pub fn fmt_config_saved(lang: Lang, key: &str, value: &str) -> String {
    match lang {
        Lang::En => format!("✓ {} = {}", key, value),
        Lang::Zh => format!("✓ {} = {}", key, value),
    }
}
pub fn fmt_lang_toggled(lang: Lang) -> String {
    match lang {
        Lang::En => "✓ Language: English.".into(),
        Lang::Zh => "✓ 语言：中文。".into(),
    }
}
pub fn fmt_start_script_missing(lang: Lang, path: &Path) -> String {
    match lang {
        Lang::En => format!("✗ {} not found. Create a start.sh first.", path.display()),
        Lang::Zh => format!("✗ {} 不存在，请先创建 start.sh。", path.display()),
    }
}
pub fn fmt_spawn_failed(lang: Lang, err: &str) -> String {
    match lang {
        Lang::En => format!("✗ Spawn failed: {}", err),
        Lang::Zh => format!("✗ 启动失败: {}", err),
    }
}
pub fn fmt_kill_failed(lang: Lang, err: &str) -> String {
    match lang {
        Lang::En => format!("✗ kill failed: {}", err),
        Lang::Zh => format!("✗ kill 失败: {}", err),
    }
}
pub fn fmt_stop_sent(lang: Lang, pid: u32) -> String {
    match lang {
        Lang::En => format!("→ SIGTERM → pid {}. Waiting for graceful shutdown…", pid),
        Lang::Zh => format!("→ 已发送 SIGTERM → pid {}。等待平滑停服…", pid),
    }
}
pub fn fmt_log_read_error(lang: Lang, err: &str) -> String {
    match lang {
        Lang::En => format!("(read error: {})", err),
        Lang::Zh => format!("(读取失败: {})", err),
    }
}

/// Pick `en` or `zh` based on `lang`. Kept for ad-hoc spots (rare); prefer
/// `lang.s().<field>` or `fmt_*` helpers above.
#[allow(dead_code)]
fn t<'a>(lang: Lang, en: &'a str, zh: &'a str) -> &'a str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

/// Chinese annotation for common `server.properties` keys.
/// Returns `None` for unknown keys (caller should fall back to showing only the raw key).
pub fn property_zh(key: &str) -> Option<&'static str> {
    Some(match key {
        "max-players" => "最大玩家数",
        "view-distance" => "视距",
        "simulation-distance" => "模拟距离",
        "difficulty" => "难度",
        "gamemode" => "默认游戏模式",
        "pvp" => "PVP",
        "hardcore" => "极限模式",
        "online-mode" => "正版验证",
        "white-list" => "启用白名单",
        "enforce-whitelist" => "强制白名单",
        "spawn-protection" => "出生点保护(格)",
        "motd" => "服务器描述(MOTD)",
        "level-name" => "世界名",
        "level-type" => "世界类型",
        "level-seed" => "世界种子",
        "server-port" => "端口",
        "server-ip" => "绑定 IP",
        "max-world-size" => "世界大小上限",
        "allow-flight" => "允许飞行",
        "allow-nether" => "启用下界",
        "spawn-monsters" => "生成怪物",
        "spawn-animals" => "生成动物",
        "spawn-npcs" => "生成村民等 NPC",
        "enable-rcon" => "启用 RCON",
        "rcon.password" => "RCON 密码",
        "rcon.port" => "RCON 端口",
        "enable-query" => "启用 Query",
        "query.port" => "Query 端口",
        "max-tick-time" => "最大 tick 时长",
        "network-compression-threshold" => "网络压缩阈值",
        "op-permission-level" => "OP 默认权限级别",
        "function-permission-level" => "函数权限级别",
        "broadcast-console-to-ops" => "控制台广播给 OP",
        "broadcast-rcon-to-ops" => "RCON 广播给 OP",
        "force-gamemode" => "强制默认模式",
        "use-native-transport" => "使用原生传输",
        "prevent-proxy-connections" => "禁止代理连接",
        "generate-structures" => "生成结构",
        "resource-pack" => "资源包 URL",
        "resource-pack-prompt" => "资源包提示",
        "require-resource-pack" => "强制资源包",
        "hide-online-players" => "隐藏在线玩家",
        "rate-limit" => "速率限制",
        "player-idle-timeout" => "玩家闲置超时(分钟)",
        "max-chained-neighbor-updates" => "链式邻接更新上限",
        "sync-chunk-writes" => "同步区块写入",
        "entity-broadcast-range-percentage" => "实体广播范围 %",
        "log-ips" => "记录 IP",
        "previews-chat" => "聊天预览",
        "enable-status" => "启用状态查询",
        "enable-jmx-monitoring" => "启用 JMX 监控",
        "enable-command-block" => "启用命令方块",
        "enforce-secure-profile" => "强制安全档案",
        "snooper-enabled" => "发送统计信息",
        "max-build-height" => "建筑高度上限",
        "spawn-protection-radius" => "出生点保护半径",
        "accepts-transfers" => "接受转移连接",
        "bug-report-link" => "Bug 反馈链接",
        "debug" => "调试模式",
        "region-file-compression" => "区域文件压缩",
        "text-filtering-config" => "文本过滤配置",
        "text-filtering-version" => "文本过滤版本",
        "pause-when-empty-seconds" => "无人时暂停秒数",
        "enable-code-of-conduct" => "启用行为准则",
        "initial-enabled-packs" => "初始启用数据包",
        "initial-disabled-packs" => "初始禁用数据包",
        _ => return None,
    })
}

// ---------- Property metadata (for the Config detail panel) ----------

pub struct PropertyMeta {
    pub description_en: &'static str,
    pub description_zh: &'static str,
    pub default: &'static str,
    pub range: &'static str,
    pub restart_required: bool,
}

pub fn property_metadata(key: &str) -> Option<&'static PropertyMeta> {
    PROPERTY_META.iter().find(|(k, _)| *k == key).map(|(_, m)| m)
}

#[rustfmt::skip]
const PROPERTY_META: &[(&str, PropertyMeta)] = &[
    ("max-players", PropertyMeta {
        description_en: "Maximum number of players that can join the server at once.",
        description_zh: "服务器同时在线玩家上限。",
        default: "20", range: "1–2147483647", restart_required: true,
    }),
    ("view-distance", PropertyMeta {
        description_en: "Server-side render distance, in chunks. Larger = more CPU/RAM.",
        description_zh: "服务器视距（区块）。越大越吃 CPU/内存。",
        default: "10", range: "3–32", restart_required: true,
    }),
    ("simulation-distance", PropertyMeta {
        description_en: "Distance (chunks) within which entities tick / mobs spawn.",
        description_zh: "实体 tick 与生物刷新的距离（区块）。",
        default: "10", range: "3–32", restart_required: true,
    }),
    ("difficulty", PropertyMeta {
        description_en: "World difficulty level.",
        description_zh: "世界难度。",
        default: "easy", range: "peaceful|easy|normal|hard", restart_required: false,
    }),
    ("gamemode", PropertyMeta {
        description_en: "Default game mode for joining players.",
        description_zh: "新加入玩家的默认游戏模式。",
        default: "survival", range: "survival|creative|adventure|spectator", restart_required: false,
    }),
    ("pvp", PropertyMeta {
        description_en: "Allow players to damage each other.",
        description_zh: "是否允许玩家间伤害。",
        default: "true", range: "true|false", restart_required: false,
    }),
    ("hardcore", PropertyMeta {
        description_en: "Hardcore mode: death = banned, difficulty locked to hard.",
        description_zh: "极限模式：死亡即封禁，难度锁定 hard。",
        default: "false", range: "true|false", restart_required: true,
    }),
    ("online-mode", PropertyMeta {
        description_en: "Verify players against Mojang auth. false = offline / cracked.",
        description_zh: "是否对 Mojang 验证玩家。false 为离线/盗版。",
        default: "true", range: "true|false", restart_required: true,
    }),
    ("white-list", PropertyMeta {
        description_en: "Enable the whitelist. Only listed players can join.",
        description_zh: "启用白名单，只有名单内玩家可加入。",
        default: "false", range: "true|false", restart_required: false,
    }),
    ("enforce-whitelist", PropertyMeta {
        description_en: "Kick non-whitelisted players already online when whitelist is reloaded.",
        description_zh: "重载白名单时，踢出已在线但不在名单的玩家。",
        default: "false", range: "true|false", restart_required: false,
    }),
    ("spawn-protection", PropertyMeta {
        description_en: "Radius (blocks) around spawn that non-ops cannot break.",
        description_zh: "出生点保护半径（方块），非 op 无法破坏。",
        default: "16", range: "0–...", restart_required: false,
    }),
    ("motd", PropertyMeta {
        description_en: "Message of the day shown in the server list.",
        description_zh: "在服务器列表中显示的欢迎语。",
        default: "A Minecraft Server", range: "any string", restart_required: false,
    }),
    ("level-name", PropertyMeta {
        description_en: "Folder name of the world to load on next start.",
        description_zh: "下次启动时加载的世界文件夹名。",
        default: "world", range: "directory name", restart_required: true,
    }),
    ("level-type", PropertyMeta {
        description_en: "World generation preset (normal, flat, large_biomes, ...).",
        description_zh: "世界生成预设（normal、flat、large_biomes 等）。",
        default: "minecraft\\:normal", range: "minecraft:<type>", restart_required: true,
    }),
    ("level-seed", PropertyMeta {
        description_en: "Seed for world generation. Empty = random.",
        description_zh: "世界生成种子。留空则随机。",
        default: "", range: "any string or number", restart_required: true,
    }),
    ("server-port", PropertyMeta {
        description_en: "TCP port the server listens on.",
        description_zh: "服务器监听的 TCP 端口。",
        default: "25565", range: "1–65535", restart_required: true,
    }),
    ("allow-flight", PropertyMeta {
        description_en: "Permit clients with flight mods (e.g. creative-style flight).",
        description_zh: "允许带飞行 mod 的客户端飞行。",
        default: "false", range: "true|false", restart_required: false,
    }),
    ("allow-nether", PropertyMeta {
        description_en: "Allow players to enter the Nether.",
        description_zh: "是否允许进入下界。",
        default: "true", range: "true|false", restart_required: true,
    }),
    ("spawn-monsters", PropertyMeta {
        description_en: "Whether hostile mobs spawn.",
        description_zh: "是否刷新敌对生物。",
        default: "true", range: "true|false", restart_required: false,
    }),
    ("spawn-animals", PropertyMeta {
        description_en: "Whether passive animals spawn.",
        description_zh: "是否刷新被动动物。",
        default: "true", range: "true|false", restart_required: false,
    }),
    ("enable-rcon", PropertyMeta {
        description_en: "Expose an RCON port for remote console.",
        description_zh: "开启 RCON 远程控制台。",
        default: "false", range: "true|false", restart_required: true,
    }),
    ("rcon.password", PropertyMeta {
        description_en: "Password for RCON. Required if enable-rcon=true.",
        description_zh: "RCON 密码。enable-rcon 为 true 时必填。",
        default: "", range: "any string", restart_required: true,
    }),
    ("rcon.port", PropertyMeta {
        description_en: "TCP port for the RCON server.",
        description_zh: "RCON 监听的 TCP 端口。",
        default: "25575", range: "1–65535", restart_required: true,
    }),
    ("op-permission-level", PropertyMeta {
        description_en: "Default permission level granted to a newly added op (1–4).",
        description_zh: "新增 op 时默认授予的权限级别（1–4）。",
        default: "4", range: "1–4", restart_required: false,
    }),
    ("function-permission-level", PropertyMeta {
        description_en: "Permission level used when running /function and command-block commands.",
        description_zh: "执行 /function 与命令方块命令时的权限级别。",
        default: "2", range: "1–4", restart_required: false,
    }),
    ("network-compression-threshold", PropertyMeta {
        description_en: "Packet size (bytes) above which packets are compressed. -1 = off.",
        description_zh: "超过该字节数的数据包会被压缩。-1 为关闭。",
        default: "256", range: "-1, 0–...", restart_required: true,
    }),
    ("max-tick-time", PropertyMeta {
        description_en: "Watchdog timeout in ms — server is killed if a single tick exceeds this.",
        description_zh: "看门狗超时（毫秒）。单 tick 超过此值会强制结束服务器。",
        default: "60000", range: "milliseconds, -1 to disable", restart_required: true,
    }),
    ("force-gamemode", PropertyMeta {
        description_en: "Force players to default gamemode every time they join.",
        description_zh: "每次加入都强制玩家回到默认游戏模式。",
        default: "false", range: "true|false", restart_required: false,
    }),
    ("generate-structures", PropertyMeta {
        description_en: "Generate structures (villages, fortresses, ...) in newly loaded chunks.",
        description_zh: "新区块是否生成结构（村庄、要塞等）。",
        default: "true", range: "true|false", restart_required: true,
    }),
    ("resource-pack", PropertyMeta {
        description_en: "URL of an optional server resource pack.",
        description_zh: "可选的服务器资源包 URL。",
        default: "", range: "URL", restart_required: false,
    }),
    ("require-resource-pack", PropertyMeta {
        description_en: "Disconnect players who reject the server resource pack.",
        description_zh: "拒绝服务器资源包的玩家会被踢出。",
        default: "false", range: "true|false", restart_required: false,
    }),
    ("player-idle-timeout", PropertyMeta {
        description_en: "Minutes of idle before a player is kicked. 0 = never.",
        description_zh: "玩家挂机超过该分钟数会被踢出。0 = 不踢。",
        default: "0", range: "minutes", restart_required: false,
    }),
    ("entity-broadcast-range-percentage", PropertyMeta {
        description_en: "Distance (percent of view-distance) at which entities are sent to clients.",
        description_zh: "实体广播给客户端的距离（视距百分比）。",
        default: "100", range: "10–1000", restart_required: false,
    }),
];

