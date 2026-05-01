//! All ratatui rendering. The dispatcher is `ui()`; per-tab `draw_*` functions
//! handle each tab. This module reads `App` state but never mutates business
//! data — disk writes go through `App::*` methods in main.rs.

use std::fs;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap},
};

use crate::data::{
    detect_interfaces, fmt_bytes, get_property, nic_kind_color, nic_kind_label, NicInfo, NicKind,
    YamlDisplay,
};
use crate::i18n::{
    fmt_log_read_error, hint_for, property_metadata, property_zh, tab_name,
    Lang, Strings,
};
use crate::{
    App, InputPrompt, LogsView, NodePickerPurpose, NodePickerState, Overlay, TabId, ToastKind,
    YamlView, PALETTE_COMMANDS, TABS,
};

pub fn ui(f: &mut Frame, app: &mut App) {
    // Compact chrome: header + tabs each take a single line (no border boxes).
    // Saves ~6 vertical lines compared to the old 3+3+3 layout, leaving the
    // content pane breathing room on small terminals.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header: status + level + dir + primary connect chip
            Constraint::Length(1), // tabs
            Constraint::Min(3),    // content
            Constraint::Length(1), // hints (no border; toast floats above content)
        ])
        .split(f.area());

    draw_header_line(f, chunks[0], app);
    draw_tabs(f, chunks[1], app);
    app.tabs_rect = chunks[1];
    app.list_rect = chunks[2];
    match app.tab {
        TabId::Overview => draw_overview(f, chunks[2], app),
        TabId::Players => draw_players(f, chunks[2], app),
        TabId::Worlds => draw_worlds(f, chunks[2], app),
        // Settings tab — file picker → property editor / yaml row editor.
        TabId::Settings => match &app.yaml_view {
            YamlView::Files => draw_settings_files(f, chunks[2], app),
            YamlView::Properties => draw_config(f, chunks[2], app),
            YamlView::Editing { .. } => draw_yaml(f, chunks[2], app),
        },
        // Phase 8 will absorb frpc lifecycle + collapsed NIC list.
        TabId::Network => draw_sakurafrp(f, chunks[2], app),
    }
    draw_hints(f, chunks[3], app);

    // v0.16 — floating toast: render before overlays so a long-running
    // overlay (like Logs) still surfaces transient feedback.
    draw_toast(f, chunks[2], app);

    // v0.16 — overlay (Help / Palette / Logs) covers tab content but sits
    // under the node picker + prompt (those are deeper modals).
    match &app.overlay {
        Overlay::None => {}
        Overlay::Help => draw_help_overlay(f, app),
        Overlay::Palette { .. } => draw_palette_overlay(f, app),
        Overlay::Logs(_) => draw_logs_overlay(f, app),
    }

    // v0.13 — node picker overlays the entire frame above the tab content.
    // Drawn after the tab so it takes visual priority; key handling routes
    // here too. Drawn before prompts so a confirm dialog can stack on top
    // (currently no flow does this, but the layering is intentional).
    if app.node_picker.is_some() {
        draw_node_picker(f, app);
    }

    if let Some(prompt) = app.prompt.clone() {
        draw_prompt(f, &prompt, app.lang);
    }
}

/// Full-screen node picker overlay. Game-friendly nodes float to the top;
/// the user navigates with ↑/↓ and confirms with Enter.
fn draw_node_picker(f: &mut Frame, app: &mut App) {
    let s = app.lang.s();
    let area = f.area();
    f.render_widget(ratatui::widgets::Clear, area);

    let Some(picker) = app.node_picker.as_mut() else {
        return;
    };

    let title: &str = match &picker.purpose {
        NodePickerPurpose::CreateTunnel { .. } => s.sf_picker_title_create,
        NodePickerPurpose::MigrateTunnel { .. } => s.sf_picker_title_migrate,
    };

    // Top: 2-line legend; middle: list; bottom: 1-line hint.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    let legend = Paragraph::new(vec![
        Line::from(Span::styled(
            format!(" {}", s.sf_picker_legend_game),
            Style::default().fg(Color::Yellow),
        )),
        Line::from(Span::styled(
            format!(" {}", s.sf_picker_legend_vip),
            Style::default().fg(Color::Gray),
        )),
    ]);
    f.render_widget(legend, chunks[0]);

    let items: Vec<ListItem> = if picker.entries.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            s.sf_picker_no_nodes,
            Style::default().fg(Color::Gray),
        )))]
    } else {
        picker
            .entries
            .iter()
            .map(|e| {
                let star = if e.is_game { "★" } else { " " };
                let star_color = if e.is_game { Color::Yellow } else { Color::Gray };
                let host_marker = if e.host_present { " " } else { "·" };
                let name_color = if e.is_game { Color::White } else { Color::Gray };
                let truncated_desc = truncate_display(&e.description, 40);
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {} ", star), Style::default().fg(star_color)),
                    Span::styled(
                        format!("#{:<5}", e.node_id),
                        Style::default().fg(Color::Gray),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        format!("{}{:<28}", host_marker, truncate_display(&e.name, 28)),
                        Style::default().fg(name_color).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        format!("VIP{:<2}", e.vip),
                        Style::default().fg(if e.vip == 0 {
                            Color::Green
                        } else {
                            Color::Magenta
                        }),
                    ),
                    Span::raw("  "),
                    Span::styled(truncated_desc, Style::default().fg(Color::Gray)),
                ]))
            })
            .collect()
    };

    // Node picker is a modal overlay — keep the full border to make the
    // "I'm in a dialog" state unambiguous.
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, chunks[1], &mut picker.list_state);

    let hint = Paragraph::new(Line::from(vec![Span::styled(
        format!(" {}", s.sf_picker_hint),
        Style::default().fg(Color::Gray),
    )]));
    f.render_widget(hint, chunks[2]);
}

/// Helper used by main.rs's mouse handler to test whether the picker is up;
/// kept here so ratatui-specific types stay in this module.
#[allow(dead_code)]
pub fn picker_is_open(state: &Option<NodePickerState>) -> bool {
    state.is_some()
}

/// `?` overlay — symbol legend at top, tab-grouped key list below. Read-only.
/// Esc or `?` again closes.
fn draw_help_overlay(f: &mut Frame, app: &App) {
    let area = f.area();
    f.render_widget(ratatui::widgets::Clear, area);
    let zh = matches!(app.lang, Lang::Zh);

    let title = if zh { " 帮助 (Esc 关闭) " } else { " Help (Esc closes) " };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Two-pane layout: legend on top (4 rows), keys below.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(3)])
        .split(inner);

    let legend = vec![
        Line::from(Span::styled(
            if zh { " 图例" } else { " Legend" },
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::raw("   "),
            Span::styled("●", Style::default().fg(Color::Green)),
            Span::styled(if zh { " 进程在跑 / 当前活动" } else { " running / current" }, Style::default().fg(Color::Gray)),
            Span::raw("   "),
            Span::styled("○", Style::default().fg(Color::Gray)),
            Span::styled(if zh { " 未运行 / 非活动" } else { " idle / not active" }, Style::default().fg(Color::Gray)),
            Span::raw("   "),
            Span::styled("✗", Style::default().fg(Color::Red)),
            Span::styled(if zh { " 缺失 / 错误" } else { " missing / error" }, Style::default().fg(Color::Gray)),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled("▶", Style::default().fg(Color::Green)),
            Span::styled(if zh { " 启用 / 在线" } else { " enabled / joined" }, Style::default().fg(Color::Gray)),
            Span::raw("   "),
            Span::styled("■", Style::default().fg(Color::Yellow)),
            Span::styled(if zh { " 停用" } else { " disabled" }, Style::default().fg(Color::Gray)),
            Span::raw("   "),
            Span::styled("★n", Style::default().fg(Color::Yellow)),
            Span::styled(if zh { " OP 等级 (n=1..4)" } else { " op level (n=1..4)" }, Style::default().fg(Color::Gray)),
            Span::raw("   "),
            Span::styled("⚠", Style::default().fg(Color::Yellow)),
            Span::styled(if zh { " 警告" } else { " warning" }, Style::default().fg(Color::Gray)),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            if zh { " 颜色契约" } else { " Color contract" },
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::raw("   "),
            Span::styled("绿", Style::default().fg(Color::Green)),
            Span::raw(if zh { "=正常  " } else { " = ok  " }),
            Span::styled("黄", Style::default().fg(Color::Yellow)),
            Span::raw(if zh { "=警告  " } else { " = warn  " }),
            Span::styled("红", Style::default().fg(Color::Red)),
            Span::raw(if zh { "=错误  " } else { " = err  " }),
            Span::styled("青", Style::default().fg(Color::Cyan)),
            Span::raw(if zh { "=焦点  " } else { " = focus  " }),
            Span::styled("品红", Style::default().fg(Color::Magenta)),
            Span::raw(if zh { "=frp 地址" } else { " = frp address" }),
        ]),
    ];
    f.render_widget(Paragraph::new(legend).wrap(Wrap { trim: false }), chunks[0]);

    // Tab-grouped key list. Two columns to fit on 32-row terminals.
    let key_block = if zh { vec![
        ("全局", vec![
            ("S / X / R", "启动 / 停止 / 重启服务器"),
            ("B", "立即跑 backup.sh"),
            ("L", "全屏日志（可滚动 + 等级过滤）"),
            (": (冒号)", "命令面板（运维杂项）"),
            ("? / Esc", "本帮助 / 关闭弹窗"),
            ("1-5", "切换到对应 tab"),
            ("Tab / Shift-Tab", "下一 / 上一 tab"),
            ("D / T / r", "切目录 / 切语言 / 刷新"),
            ("q", "退出"),
        ]),
        ("玩家", vec![
            ("Enter", "切换白名单"),
            ("o / ←/→", "切换 OP / 切级别 1-4"),
            ("a / d", "添加 / 清除"),
            ("w", "白名单总开关"),
        ]),
        ("世界", vec![
            ("Enter", "切到该世界（需先停服）"),
            ("N", "新建世界"),
        ]),
        ("网络", vec![
            ("Enter", "复制公网地址"),
            ("i", "一键配置（下载 frpc + 启用所有隧道）"),
            ("e / x", "启用 / 停用选中隧道"),
            ("c / m / d", "创建 / 迁移 / 删除"),
            ("t / o", "设置 token / 打开 natfrp.com"),
        ]),
    ] } else { vec![
        ("Global", vec![
            ("S / X / R", "start / stop / restart server"),
            ("B", "run backup.sh now"),
            ("L", "fullscreen logs (scroll + level filter)"),
            (": (colon)", "command palette (advanced ops)"),
            ("? / Esc", "this help / close overlay"),
            ("1-5", "jump to tab"),
            ("Tab / Shift-Tab", "next / prev tab"),
            ("D / T / r", "switch dir / lang / refresh"),
            ("q", "quit"),
        ]),
        ("Players", vec![
            ("Enter", "toggle whitelist"),
            ("o / ←/→", "toggle op / cycle level 1-4"),
            ("a / d", "add / purge"),
            ("w", "whitelist on/off"),
        ]),
        ("Worlds", vec![
            ("Enter", "switch to world (server must be stopped)"),
            ("N", "new world"),
        ]),
        ("Network", vec![
            ("Enter", "copy public address"),
            ("i", "one-key setup (download frpc + enable all)"),
            ("e / x", "enable / disable selected"),
            ("c / m / d", "create / migrate / delete"),
            ("t / o", "set token / open natfrp.com"),
        ]),
    ] };

    let mut lines: Vec<Line> = Vec::new();
    for (heading, entries) in key_block {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!(" {}", heading),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )));
        for (key, desc) in entries {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(format!("{:<18}", key), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(desc.to_string(), Style::default().fg(Color::Gray)),
            ]));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), chunks[1]);
}

/// `:` overlay — command palette. List of advanced operations migrated from
/// the deleted Server tab. Enter executes; Esc closes.
fn draw_palette_overlay(f: &mut Frame, app: &mut App) {
    let area = centered_rect(70, 16, f.area());
    f.render_widget(ratatui::widgets::Clear, area);
    let zh = matches!(app.lang, Lang::Zh);
    let title = if zh { " : 命令面板 " } else { " : Command palette " };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(inner);

    let items: Vec<ListItem> = PALETTE_COMMANDS
        .iter()
        .map(|a| {
            ListItem::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    crate::i18n::server_action_label(app.lang, *a).to_string(),
                    Style::default().fg(Color::White),
                ),
            ]))
        })
        .collect();

    if let Overlay::Palette { state } = &mut app.overlay {
        let list = List::new(items)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD))
            .highlight_symbol("> ");
        f.render_stateful_widget(list, chunks[0], state);
    }

    let hint = Paragraph::new(Line::from(Span::styled(
        if zh {
            " ↑↓ 选择   Enter 执行   Esc 关闭"
        } else {
            " ↑↓ select   Enter execute   Esc close"
        },
        Style::default().fg(Color::Gray),
    )));
    f.render_widget(hint, chunks[1]);
}

/// `L` overlay — fullscreen scrollable log viewer. Phase 4 fills in the real
/// scroll/filter/source logic; Phase 3 stub renders the existing tail with a
/// header showing the current source + filter state.
fn draw_logs_overlay(f: &mut Frame, app: &App) {
    use unicode_width::UnicodeWidthStr as _;
    let area = f.area();
    f.render_widget(ratatui::widgets::Clear, area);
    let zh = matches!(app.lang, Lang::Zh);

    let Overlay::Logs(state) = &app.overlay else { return };

    let source_label = match (state.source, zh) {
        (LogsView::Server, true) => "服务器日志",
        (LogsView::Server, false) => "server log",
        (LogsView::Frpc, true) => "网络转发日志",
        (LogsView::Frpc, false) => "tunnel log",
    };
    let filter_label = match (state.filter, zh) {
        (crate::LogsLevelFilter::All, true) => "全部",
        (crate::LogsLevelFilter::All, false) => "ALL",
        (crate::LogsLevelFilter::Info, _) => "INFO",
        (crate::LogsLevelFilter::Warn, _) => "WARN",
        (crate::LogsLevelFilter::Error, _) => "ERROR",
    };
    let title = if zh {
        format!(" 日志 — {} · 等级 {} (Esc 关闭) ", source_label, filter_label)
    } else {
        format!(" Logs — {} · level {} (Esc closes) ", source_label, filter_label)
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Body: pull from current source. Simple tail for Phase 3; Phase 4 adds
    // scroll offset + filter respect.
    let body: String = match state.source {
        LogsView::Server => {
            let p = app.server_dir.join("logs/latest.log");
            std::fs::read_to_string(&p).unwrap_or_else(|e| {
                fmt_log_read_error(app.lang, &e.to_string())
            })
        }
        LogsView::Frpc => {
            app.frpc_console.capture_recent(1000).unwrap_or_else(|e| {
                if zh {
                    format!("(网络转发尚未启动 — 详情：{})", e)
                } else {
                    format!("(tunnel not running yet — detail: {})", e)
                }
            })
        }
    };
    let mut lines_in: Vec<&str> = body.lines().collect();
    if state.filter != crate::LogsLevelFilter::All {
        lines_in.retain(|l| line_matches_level(l, state.filter));
    }

    let take = (inner.height as usize).max(1);
    let total = lines_in.len();
    // Clamp scroll_back to [0, max_back] where max_back = total.saturating_sub(take).
    // 0 = autotail (window aligned with last `take` lines).
    let max_back = total.saturating_sub(take);
    let scroll_back = state.scroll_back.min(max_back);
    let start = total.saturating_sub(take + scroll_back);
    let end = (start + take).min(total);
    let visible: &[&str] = if total == 0 { &[] } else { &lines_in[start..end] };

    let rendered: Vec<Line> = visible
        .iter()
        .map(|line| Line::from(colorize_log_line(line)))
        .collect();
    let p = Paragraph::new(rendered).wrap(Wrap { trim: false });
    f.render_widget(p, inner);

    // Bottom-right: position indicator. "tail" while autotail, otherwise
    // current line / total — helps the user know if they've left the tail.
    let pos = if state.is_autotail() {
        if zh { "末尾 (autotail)".to_string() } else { "tail (autotail)".to_string() }
    } else {
        format!("{}/{}", end, total)
    };
    let pos_w = pos.width() as u16;
    let pos_rect = Rect {
        x: area.x + area.width.saturating_sub(pos_w + 2),
        y: area.y + area.height.saturating_sub(1),
        width: pos_w,
        height: 1,
    };
    f.render_widget(
        Paragraph::new(Span::styled(pos, Style::default().fg(Color::Gray))),
        pos_rect,
    );
}

/// Phase 3 stub level matcher — Phase 4 will replace with proper severity
/// classification (currently lives in colorize_log_line as substring scan).
fn line_matches_level(line: &str, filter: crate::LogsLevelFilter) -> bool {
    let lower = line.to_ascii_lowercase();
    let is_err = line.contains("[ERROR]") || lower.contains("/error]:");
    let is_warn = line.contains("[WARN]") || lower.contains("/warn]:");
    match filter {
        crate::LogsLevelFilter::All => true,
        crate::LogsLevelFilter::Error => is_err,
        crate::LogsLevelFilter::Warn => is_warn || is_err,
        crate::LogsLevelFilter::Info => !is_err && !is_warn,
    }
}

/// One-line header: status + level + dir + primary connect chip.
/// No border — saves vertical space and keeps key info above the tab bar.
/// Click the chip to copy `<ip>:<port>` to the clipboard via wl-copy.
/// One-line "diagnose-at-a-glance" header. Format:
///   `mc <state> <world> · ▶<online>     LAN <ip>:<port>   frp <state> [<addr>]    ?`
/// State glyph escalates to ⚠ if recent log activity contains errors. Click
/// either chip to copy that address. Path & "目录:" label are deliberately
/// dropped — `D` toasts the dir on switch and `?` shows it on demand.
fn draw_header_line(f: &mut Frame, area: Rect, app: &mut App) {
    use unicode_width::UnicodeWidthStr;
    let zh = matches!(app.lang, Lang::Zh);

    let nics = detect_interfaces();
    let port: u16 = get_property(&app.properties, "server-port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(25565);
    let primary = nics.iter().find(|n| {
        !matches!(n.kind, NicKind::Loopback | NicKind::Docker | NicKind::Tun)
    });
    let frp_addr = app.effective_sakurafrp_address();
    let online_count = app.players.iter().filter(|p| p.is_online).count();

    app.join_chips.clear();

    // ---- Left half: server state + world + online ----
    let mut spans: Vec<Span> = vec![
        Span::styled(" mc ", Style::default().fg(Color::Gray)),
    ];
    match app.pid {
        Some(_p) => spans.push(Span::styled(
            "●",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )),
        None => spans.push(Span::styled(
            "○",
            Style::default().fg(Color::Gray),
        )),
    }
    spans.push(Span::raw(" "));
    spans.push(Span::styled(
        app.current_level().to_string(),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    ));
    if app.pid.is_some() {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            format!("▶{}", online_count),
            Style::default()
                .fg(if online_count > 0 { Color::Green } else { Color::Gray })
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            if zh { "(S 启动)" } else { "(S to start)" },
            Style::default().fg(Color::Gray),
        ));
    }

    // ---- Compute right-half spans (LAN + frp + ?) so we can right-align ----
    let mut right_spans: Vec<Span> = Vec::new();
    if let Some(n) = primary {
        let kind_label = nic_kind_label(app.lang, n.kind);
        right_spans.push(Span::styled(
            format!("{} ", kind_label),
            Style::default().fg(nic_kind_color(n.kind)),
        ));
        let lan_chip = format!("{}:{}", n.ip, port);
        right_spans.push(Span::styled(
            lan_chip.clone(),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ));
        right_spans.push(Span::raw("   "));
    }
    let (frp_marker, frp_color) = if app.frpc_pid.is_some() {
        ("●", Color::Green)
    } else if app.frpc_binary.is_none() {
        ("✗", Color::Red)
    } else if !app.frpc_enabled_ids.is_empty() {
        ("○", Color::Yellow)
    } else {
        ("○", Color::Gray)
    };
    right_spans.push(Span::styled("frp ", Style::default().fg(Color::Gray)));
    right_spans.push(Span::styled(frp_marker, Style::default().fg(frp_color)));
    if let Some(addr) = &frp_addr {
        right_spans.push(Span::raw(" "));
        right_spans.push(Span::styled(
            addr.clone(),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ));
    }
    right_spans.push(Span::raw("   "));
    right_spans.push(Span::styled("?", Style::default().fg(Color::Gray)));
    right_spans.push(Span::raw(" "));

    // Compute widths for click-region tracking.
    let left_w: u16 = spans.iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()) as u16)
        .sum();
    let right_w: u16 = right_spans.iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()) as u16)
        .sum();
    let pad = area.width.saturating_sub(left_w).saturating_sub(right_w);
    spans.push(Span::raw(" ".repeat(pad as usize)));

    // Track LAN chip rect for click-to-copy. We need the x position where the
    // chip text starts — which is left_w + pad + (kind_label width) + 1 (for
    // the space after kind_label).
    if let Some(n) = primary {
        let kind_w = UnicodeWidthStr::width(format!("{} ", nic_kind_label(app.lang, n.kind)).as_str()) as u16;
        let chip_text = format!("{}:{}", n.ip, port);
        let chip_w = UnicodeWidthStr::width(chip_text.as_str()) as u16;
        let chip_x = area.x + left_w + pad + kind_w;
        app.join_chips.push((Rect { x: chip_x, y: area.y, width: chip_w, height: 1 }, chip_text));
    }
    if let Some(addr) = &frp_addr {
        // frp chip x: left_w + pad + (kind+chip+pad + "frp " + marker + " ").
        // Easier: walk right_spans up to "frp " then advance through marker
        // and " ", that's where the addr begins.
        let mut x = area.x + left_w + pad;
        for sp in &right_spans {
            // The first span whose content equals the address is our chip.
            if sp.content.as_ref() == addr.as_str() {
                let w = UnicodeWidthStr::width(addr.as_str()) as u16;
                app.join_chips.push((Rect { x, y: area.y, width: w, height: 1 }, addr.clone()));
                break;
            }
            x += UnicodeWidthStr::width(sp.content.as_ref()) as u16;
        }
    }

    spans.extend(right_spans);
    let p = Paragraph::new(Line::from(spans));
    f.render_widget(p, area);
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let titles: Vec<Line> = TABS
        .iter()
        .enumerate()
        .map(|(i, (id, _en))| {
            Line::from(vec![
                Span::styled(
                    format!(" {} ", i + 1),
                    Style::default().fg(Color::Gray),
                ),
                Span::raw(format!("{} ", tab_name(app.lang, *id))),
            ])
        })
        .collect();
    let selected = TABS.iter().position(|(t, _)| *t == app.tab).unwrap_or(0);
    // No background block — selected tab gets bold + underline only. Reads the
    // same against any terminal theme; cyan reverse was always the loud one.
    // Empty divider (default is `│`) — spacing alone is enough.
    let tabs = Tabs::new(titles)
        .divider(" ")
        .select(selected)
        .style(Style::default().fg(Color::Gray))
        .highlight_style(
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        );
    f.render_widget(tabs, area);
}

/// Overview tab — the "is everything OK" landing page. Two regions only:
///   1. A single line with a state sentence + 1-line action hints.
///   2. Filtered + translated recent activity (joins / leaves / start / stop /
///      WARN / ERROR), one event per row, max ~last 12.
///
/// Server-state, level name, frp address — all in the header bar already, no
/// reason to repeat them here. Online players, when any, get a tiny inline
/// list under the status line.
fn draw_overview(f: &mut Frame, area: Rect, app: &mut App) {
    let zh = matches!(app.lang, Lang::Zh);
    let online: Vec<&crate::data::PlayerEntry> = app
        .players
        .iter()
        .filter(|p| p.is_online)
        .collect();

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw(""));

    // ── Status line ──
    let status = match app.pid {
        Some(_) if !online.is_empty() => Line::from(vec![
            Span::raw("  "),
            Span::styled("●", Style::default().fg(Color::Green)),
            Span::raw("  "),
            Span::styled(
                if zh {
                    format!("{} 人在线", online.len())
                } else {
                    format!("{} online", online.len())
                },
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::raw("    "),
            dim_kbd("Y"),
            Span::styled(
                if zh { " 复制邀请   " } else { " copy invite   " },
                Style::default().fg(Color::Gray),
            ),
            dim_kbd("X"),
            Span::styled(
                if zh { " 关服   " } else { " stop   " },
                Style::default().fg(Color::Gray),
            ),
            dim_kbd("B"),
            Span::styled(
                if zh { " 备份" } else { " backup" },
                Style::default().fg(Color::Gray),
            ),
        ]),
        Some(_) => Line::from(vec![
            Span::raw("  "),
            Span::styled("●", Style::default().fg(Color::Green)),
            Span::raw("  "),
            Span::styled(
                if zh { "服务器运行中 · 暂无人在线" } else { "running · nobody online" },
                Style::default().fg(Color::White),
            ),
            Span::raw("    "),
            dim_kbd("Y"),
            Span::styled(
                if zh { " 复制邀请   " } else { " copy invite   " },
                Style::default().fg(Color::Gray),
            ),
            dim_kbd("X"),
            Span::styled(
                if zh { " 关服" } else { " stop" },
                Style::default().fg(Color::Gray),
            ),
        ]),
        None => Line::from(vec![
            Span::raw("  "),
            Span::styled("○", Style::default().fg(Color::Gray)),
            Span::raw("  "),
            Span::styled(
                if zh { "服务器未启动" } else { "server stopped" },
                Style::default().fg(Color::White),
            ),
            Span::raw("    "),
            dim_kbd("S"),
            Span::styled(
                if zh { " 启动   " } else { " start   " },
                Style::default().fg(Color::Gray),
            ),
            dim_kbd("N"),
            Span::styled(
                if zh { " 新建世界   " } else { " new world   " },
                Style::default().fg(Color::Gray),
            ),
            dim_kbd("R"),
            Span::styled(
                if zh { " 重启" } else { " restart" },
                Style::default().fg(Color::Gray),
            ),
        ]),
    };
    lines.push(status);

    // Online players (compact inline list). Skipped when stopped or empty.
    if !online.is_empty() {
        lines.push(Line::raw(""));
        for p in online.iter().take(6) {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled("▶ ", Style::default().fg(Color::Green)),
                Span::styled(
                    p.name.clone(),
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
                ),
            ]));
        }
        if online.len() > 6 {
            lines.push(Line::from(Span::styled(
                format!(
                    "    {}",
                    if zh {
                        format!("… 还有 {} 人", online.len() - 6)
                    } else {
                        format!("… {} more", online.len() - 6)
                    }
                ),
                Style::default().fg(Color::Gray),
            )));
        }
    }

    // ── Activity section ──
    lines.push(Line::raw(""));
    lines.push(Line::raw(""));
    lines.push(divider_with_action_hint(
        area.width,
        if zh { " 最近活动 " } else { " Recent activity " },
        if zh { "L 完整日志" } else { "L full log" },
    ));
    lines.push(Line::raw(""));

    // Filter + translate. Walk in reverse, take only "interesting" events.
    let body = read_recent_log_tail(&app.server_dir, app.lang);
    let used_above = lines.len() as u16;
    let event_room = (area.height as usize)
        .saturating_sub(used_above as usize)
        .max(1);
    let events = filter_overview_events(&body, app.lang, event_room);
    if events.is_empty() {
        lines.push(Line::from(Span::styled(
            if zh {
                "    (没有可显示的事件 — 启动一次服务器再回来)"
            } else {
                "    (no events to show — start the server then come back)"
            },
            Style::default().fg(Color::Gray).add_modifier(Modifier::ITALIC),
        )));
    } else {
        lines.extend(events);
    }

    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

/// Render a `K` keyboard chip — single bold cyan cap so the eye finds it
/// even amid darkgray hint prose.
fn dim_kbd(key: &str) -> Span<'static> {
    Span::styled(
        key.to_string(),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )
}

/// Build a section divider that fills `width`: `── <title> ──...── <hint> ──`.
/// The hint segment is right-aligned and dim-cyan to suggest a clickable key.
fn divider_with_action_hint(width: u16, title: &str, hint: &str) -> Line<'static> {
    use unicode_width::UnicodeWidthStr;
    let title_w = UnicodeWidthStr::width(title) as u16;
    let hint_w = UnicodeWidthStr::width(hint) as u16;
    // Layout: " ── <title> " <fill> " <hint> ──"
    // Reserve: 4 chars `── ` prefix, 1 leading space, 1 trailing ` ──` (3 chars)
    //          + 1 space between title and fill, 1 between fill and hint
    let chrome_w: u16 = 1 + 2 + 1 /*"── "*/ + 1 + 2 /*" ──"*/;
    let inside = width.saturating_sub(chrome_w + title_w + hint_w + 2);
    let fill = "─".repeat(inside as usize);
    Line::from(vec![
        Span::styled(" ── ", Style::default().fg(Color::Gray)),
        Span::styled(
            title.trim().to_string(),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(fill, Style::default().fg(Color::Gray)),
        Span::raw(" "),
        Span::styled(
            hint.to_string(),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(" ──", Style::default().fg(Color::Gray)),
    ])
}

/// Walk `body` in reverse, collect "interesting" events, translate to a
/// localized one-line `Line`. Stops at `max` events; returns oldest-first.
/// Filters out chunk system / I/O halt / save spam — only joins / leaves /
/// disconnects / errors / warns / server start-stop / whitelist denials.
fn filter_overview_events(body: &str, lang: Lang, max: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::with_capacity(max);
    for line in body.lines().rev() {
        if let Some(ev) = translate_event(line, lang) {
            out.push(ev);
            if out.len() >= max {
                break;
            }
        }
    }
    out.reverse();
    out
}

/// Match a single Paper/Purpur log line against the small set of "interesting
/// to the host" patterns and return a translated `Line`. Returns `None` for
/// noise.
fn translate_event(line: &str, lang: Lang) -> Option<Line<'static>> {
    let zh = matches!(lang, Lang::Zh);
    // Extract `[HH:MM:SS]` timestamp prefix. If absent, skip — we don't
    // surface untimestamped output.
    let close = line.find(']')?;
    let ts_inner = line.get(1..close)?;
    if !is_hms(ts_inner) {
        return None;
    }
    let ts: String = ts_inner[..5].to_string(); // HH:MM
    let after = &line[close + 1..];

    // Player joined.
    if let Some(name) = strip_player_action(after, "joined the game") {
        return Some(format_event(
            &ts,
            "→",
            Color::Green,
            &name,
            if zh { "加入" } else { "joined" },
            Color::Green,
        ));
    }
    // Player left.
    if let Some(name) = strip_player_action(after, "left the game") {
        return Some(format_event(
            &ts,
            "←",
            Color::Cyan,
            &name,
            if zh { "离开" } else { "left" },
            Color::Cyan,
        ));
    }
    // Whitelist denial.
    if line.contains("not whitelisted on this server") {
        let name = extract_name_field(line).unwrap_or_else(|| "?".into());
        return Some(format_event(
            &ts,
            "✗",
            Color::Red,
            &name,
            if zh { "被拒（不在白名单）" } else { "denied (not whitelisted)" },
            Color::Red,
        ));
    }
    // Server done starting.
    if line.contains("Done (") && line.contains("s)! For help, type") {
        let dur = extract_done_duration(line).unwrap_or_else(|| "?".into());
        return Some(Line::from(vec![
            Span::raw("   "),
            Span::styled(format!("{}  ", ts), Style::default().fg(Color::Gray)),
            Span::styled("● ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled(
                if zh {
                    format!("服务器启动完成 ({})", dur)
                } else {
                    format!("server started ({})", dur)
                },
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    // Server stopping.
    if line.contains("Stopping the server") || line.contains("Stopping server") {
        return Some(Line::from(vec![
            Span::raw("   "),
            Span::styled(format!("{}  ", ts), Style::default().fg(Color::Gray)),
            Span::styled("○ ", Style::default().fg(Color::Gray)),
            Span::styled(
                if zh { "服务器停止" } else { "server stopping" },
                Style::default().fg(Color::Gray),
            ),
        ]));
    }
    // ERROR.
    if line.contains("[ERROR]") || line.to_ascii_lowercase().contains("/error]:") {
        let msg = clean_log_msg(after);
        return Some(Line::from(vec![
            Span::raw("   "),
            Span::styled(format!("{}  ", ts), Style::default().fg(Color::Gray)),
            Span::styled("⚠ ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::styled(
                truncate_display(&msg, 90),
                Style::default().fg(Color::Red),
            ),
        ]));
    }
    // WARN.
    if line.contains("[WARN]") || line.to_ascii_lowercase().contains("/warn]:") {
        let msg = clean_log_msg(after);
        return Some(Line::from(vec![
            Span::raw("   "),
            Span::styled(format!("{}  ", ts), Style::default().fg(Color::Gray)),
            Span::styled("⚠ ", Style::default().fg(Color::Yellow)),
            Span::styled(
                truncate_display(&msg, 90),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    }
    None
}

/// Format a `<ts>  <glyph> <subject> <action>` event row.
fn format_event(
    ts: &str,
    glyph: &str,
    glyph_color: Color,
    subject: &str,
    action: &str,
    action_color: Color,
) -> Line<'static> {
    Line::from(vec![
        Span::raw("   "),
        Span::styled(format!("{}  ", ts), Style::default().fg(Color::Gray)),
        Span::styled(
            format!("{} ", glyph),
            Style::default().fg(glyph_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            subject.to_string(),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(action.to_string(), Style::default().fg(action_color)),
    ])
}

/// Match `[Server thread/INFO]: <name> <action>` and return `<name>` if the
/// action suffix matches. Returns None for non-matching lines.
fn strip_player_action(after: &str, action: &str) -> Option<String> {
    let after = after.trim_start();
    let bracket_close = after.find("]:")?;
    let body = after[bracket_close + 2..].trim();
    let stripped = body.strip_suffix(action)?;
    let name = stripped.trim().to_string();
    if name.is_empty() {
        return None;
    }
    Some(name)
}

/// Pull the username out of a `name=<X>,` field (used in the disconnect /
/// not-whitelisted log lines from Paper).
fn extract_name_field(line: &str) -> Option<String> {
    let i = line.find("name=")?;
    let from = &line[i + 5..];
    let end = from
        .find(|c: char| c == ',' || c == ']' || c == ' ')
        .unwrap_or(from.len());
    let name = from[..end].trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Pull `Xs` out of a `Done (Xs)! For help, type` line.
fn extract_done_duration(line: &str) -> Option<String> {
    let i = line.find("Done (")?;
    let from = &line[i + 6..];
    let end = from.find("s)")?;
    Some(format!("{}s", &from[..end]))
}

/// Strip the `[<thread>/<level>]: ` prefix from a log message body, leaving
/// just the human-readable part.
fn clean_log_msg(after: &str) -> String {
    if let Some(close) = after.find("]:") {
        after[close + 2..].trim().to_string()
    } else {
        after.trim().to_string()
    }
}

/// Cheap "is this `HH:MM:SS`?" check — at least 8 chars, colons at 2 and 5.
fn is_hms(s: &str) -> bool {
    s.len() >= 8 && s.as_bytes().get(2) == Some(&b':') && s.as_bytes().get(5) == Some(&b':')
}

/// Read the tail of `logs/latest.log` if present. Caller cares only about a
/// few lines; we return the full body and let the caller take the bottom.
fn read_recent_log_tail(server_dir: &std::path::Path, lang: Lang) -> String {
    let p = server_dir.join("logs/latest.log");
    if !p.exists() {
        return lang.s().no_logs_yet.to_string();
    }
    fs::read_to_string(&p).unwrap_or_else(|e| fmt_log_read_error(lang, &e.to_string()))
}

/// Compose the multi-line share text the user copies with `Y`. Picks frp if
/// configured and reachable; otherwise LAN. `lan` is `(ip, port)` — the IP is
/// passed by display string so the caller can pick whichever Ipv4Addr-display
/// form they want without re-formatting here.
pub fn build_share_text(
    app: &App,
    lan: Option<(String, u16)>,
    frp: Option<&str>,
) -> String {
    let zh = matches!(app.lang, Lang::Zh);
    let level = app.current_level().to_string();
    let mut out = String::new();
    if zh {
        out.push_str(&format!("MC 服务器：{}\n", level));
        if let Some(addr) = frp {
            out.push_str(&format!("地址（公网）：{}\n", addr));
            if let Some((ip, port)) = lan.as_ref() {
                out.push_str(&format!("（同局域网备用：{}:{}）\n", ip, port));
            }
        } else if let Some((ip, port)) = lan {
            out.push_str(&format!("地址（局域网）：{}:{}\n", ip, port));
        }
        out.push_str("把你的 Minecraft 用户名发我，加进白名单后就能进。\n");
    } else {
        out.push_str(&format!("MC server: {}\n", level));
        if let Some(addr) = frp {
            out.push_str(&format!("Address (public): {}\n", addr));
            if let Some((ip, port)) = lan.as_ref() {
                out.push_str(&format!("(same LAN fallback: {}:{})\n", ip, port));
            }
        } else if let Some((ip, port)) = lan {
            out.push_str(&format!("Address (LAN): {}:{}\n", ip, port));
        }
        out.push_str("Send me your Minecraft username and I'll whitelist you.\n");
    }
    out
}

fn draw_worlds(f: &mut Frame, area: Rect, app: &mut App) {
    let (list_area, detail_area) = split_list_detail(area);
    let items: Vec<ListItem> = app
        .worlds
        .iter()
        .map(|w| {
            let mark = if w.is_current { "●" } else { " " };
            let when = w
                .last_modified
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {} ", mark), Style::default().fg(Color::Green)),
                Span::styled(format!("{:30}", w.name), Style::default().fg(Color::White)),
                Span::styled(
                    format!("{:>10}  ", fmt_bytes(w.size_bytes)),
                    Style::default().fg(Color::Gray),
                ),
                Span::styled(when, Style::default().fg(Color::Gray)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::TOP).title(app.lang.s().title_worlds))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, list_area, &mut app.worlds_state);
    if let Some(da) = detail_area {
        draw_world_detail(f, da, app);
    }
}

/// Split a content area horizontally into `(list, detail)`. If the screen is
/// narrower than 90 cols the detail panel is hidden (single-pane fallback).
pub fn split_list_detail(area: Rect) -> (Rect, Option<Rect>) {
    if area.width < 90 {
        return (area, None);
    }
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(area);
    (chunks[0], Some(chunks[1]))
}

fn kv_line_label(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{}: ", label), Style::default().fg(Color::Gray)),
        Span::styled(value.to_string(), Style::default().fg(Color::White)),
    ])
}

fn kv_line_bold(value: &str, color: Color) -> Line<'static> {
    Line::from(Span::styled(
        value.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))
}

fn draw_world_detail(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    let zh = matches!(app.lang, Lang::Zh);
    // Detail panel: TOP separator (with title) + LEFT separator (vertical
    // line between list and detail). Together they form an "L" that reads as
    // a clear section divider without a full box.
    let block = Block::default()
        .borders(Borders::TOP | Borders::LEFT)
        .title(s.detail_title);
    let mut lines: Vec<Line> = match app.worlds_state.selected().and_then(|i| app.worlds.get(i)) {
        None => vec![Line::from(Span::styled(
            s.detail_no_selection,
            Style::default().fg(Color::Gray),
        ))],
        Some(w) => {
            let when = w
                .last_modified
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_else(|| "?".into());
            let yn = |b: bool| if b { s.detail_yes } else { s.detail_no };
            vec![
                kv_line_bold(&w.name, Color::Cyan),
                Line::raw(""),
                kv_line_label(s.detail_path, &w.path.display().to_string()),
                kv_line_label(s.detail_size, &fmt_bytes(w.size_bytes)),
                kv_line_label(s.detail_modified, &when),
                kv_line_label(s.detail_is_current, yn(w.is_current)),
                kv_line_label(s.detail_has_level_dat, yn(w.has_level_dat)),
                kv_line_label(s.detail_playerdata_count, &w.playerdata_count.to_string()),
            ]
        }
    };

    // v0.16 — Backups roll into the Worlds detail panel (the standalone
    // Backups tab is gone). Show the most recent N for context. If a backup
    // filename contains the world name, prefer those — otherwise just the
    // newest. This is read-only; restore is intentionally still manual.
    let world_name = app
        .worlds_state
        .selected()
        .and_then(|i| app.worlds.get(i))
        .map(|w| w.name.as_str());
    let candidates: Vec<&crate::data::BackupEntry> = if let Some(name) = world_name {
        let mut filtered: Vec<&crate::data::BackupEntry> = app
            .backups
            .iter()
            .filter(|b| b.name.contains(name))
            .collect();
        if filtered.is_empty() {
            filtered = app.backups.iter().collect();
        }
        filtered
    } else {
        app.backups.iter().collect()
    };
    let total = candidates.len();
    if total > 0 {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            if zh {
                format!("── 备份 ({}) ──────────", total)
            } else {
                format!("── Backups ({}) ──────────", total)
            },
            Style::default().fg(Color::Gray),
        )));
        let now = chrono::Local::now();
        for b in candidates.iter().take(5) {
            let age = b
                .modified
                .map(|t| fmt_age(now - t))
                .unwrap_or_else(|| "?".into());
            // Strip the redundant `<world>-` prefix and `.tar.zst` suffix —
            // what's left is the timestamp the user cares about. Keeps the
            // line readable in the 30%-wide detail column.
            let short = backup_short_name(&b.name, world_name);
            // Compact one-line format that fits the ~38-col detail column:
            // `· <age>  <short_name>`. Size is dropped — backups for one
            // world are similar size, the user already saw total size above.
            lines.push(Line::from(vec![
                Span::styled(" · ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("{:>8}", age),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw("  "),
                Span::styled(
                    truncate_display(&short, 22),
                    Style::default().fg(Color::White),
                ),
            ]));
        }
        if total > 5 {
            lines.push(Line::from(Span::styled(
                if zh {
                    format!(" … 还有 {} 个", total - 5)
                } else {
                    format!(" … {} more", total - 5)
                },
                Style::default().fg(Color::Gray),
            )));
        }
    }

    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

pub fn op_level_meaning(s: &Strings, level: u8) -> &'static str {
    match level {
        1 => s.detail_op_level_1,
        2 => s.detail_op_level_2,
        3 => s.detail_op_level_3,
        4 => s.detail_op_level_4,
        _ => "?",
    }
}

/// One-line OP-level legend: `OP 等级  ★1 出生保护豁免 · ★2 作弊指令 · ★3 多人管理 · ★4 服务器管理`.
/// Compact compared to the prose in `detail_op_level_*`, but uses the same
/// star glyph as the player rows so the user can connect "row 4 has ★4" to
/// "★4 = server admin" without leaving the tab.
fn build_op_legend(lang: Lang) -> Line<'static> {
    let zh = matches!(lang, Lang::Zh);
    let pairs: [(&str, &str); 4] = if zh {
        [
            ("★1", "出生保护豁免"),
            ("★2", "作弊指令"),
            ("★3", "多人管理"),
            ("★4", "服务器管理"),
        ]
    } else {
        [
            ("★1", "spawn bypass"),
            ("★2", "cheat cmds"),
            ("★3", "player admin"),
            ("★4", "server admin"),
        ]
    };
    let mut spans: Vec<Span> = vec![
        Span::raw(" "),
        Span::styled(
            if zh { "OP 等级  " } else { "OP levels  " },
            Style::default().fg(Color::Gray),
        ),
    ];
    for (i, (mark, desc)) in pairs.iter().enumerate() {
        spans.push(Span::styled(
            mark.to_string(),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            desc.to_string(),
            Style::default().fg(Color::Gray),
        ));
        if i < pairs.len() - 1 {
            spans.push(Span::styled("  ·  ", Style::default().fg(Color::Gray)));
        }
    }
    Line::from(spans)
}

fn draw_players(f: &mut Frame, area: Rect, app: &mut App) {
    // Two-line header: row 1 = online count + whitelist state; row 2 = OP
    // level legend (always visible so the user doesn't have to memorize what
    // ★1..4 mean — same info as the detail panel but reachable at a glance).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(3)])
        .split(area);

    let s = app.lang.s();
    let legend_text = if app.whitelist_enabled {
        s.players_legend_wl_on
    } else {
        s.players_legend_wl_off
    };
    let legend_color = if app.whitelist_enabled {
        Color::Green
    } else {
        Color::Gray
    };
    let online_count = app.players.iter().filter(|p| p.is_online).count();
    let online_label = match app.lang {
        Lang::En => format!("▶ {} online", online_count),
        Lang::Zh => format!("▶ 在线 {} 人", online_count),
    };
    let row1 = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            online_label,
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        ),
        Span::raw("    "),
        Span::styled(legend_text, Style::default().fg(legend_color).add_modifier(Modifier::BOLD)),
    ]);
    let row2 = build_op_legend(app.lang);
    f.render_widget(Paragraph::new(vec![row1, row2]), chunks[0]);

    let (list_area, detail_area) = split_list_detail(chunks[1]);
    let wl_enabled = app.whitelist_enabled;

    let items: Vec<ListItem> = if app.players.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            s.players_none,
            Style::default().fg(Color::Gray),
        )))]
    } else {
        app.players
            .iter()
            .map(|p| player_row(p, wl_enabled, app.lang))
            .collect()
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::TOP).title(s.title_players))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, list_area, &mut app.players_state);
    if let Some(da) = detail_area {
        draw_players_detail(f, da, app);
    }
}

fn player_row(p: &crate::data::PlayerEntry, wl_enabled: bool, lang: Lang) -> ListItem<'static> {
    let s = lang.s();

    // Online marker (column 0): ▶ green when player is currently logged in,
    // empty otherwise. Distinct glyph from the WL ●/○ to avoid conflation;
    // the host sees who's *playing right now* at a glance without scrolling.
    let online_span = if p.is_online {
        Span::styled(
            " ▶ ",
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("   ")
    };

    // OP marker: ★n (yellow) when op, "  " (gray dot) when not.
    let op_span = match p.op_level {
        Some(level) => Span::styled(
            format!(" ★{} ", level),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        None => Span::styled("    ", Style::default()),
    };

    // Whitelist marker: ●/○ when whitelist is enabled; hidden when off.
    let wl_span = if wl_enabled {
        if p.in_whitelist {
            Span::styled(" ● ", Style::default().fg(Color::Green))
        } else {
            Span::styled(" ○ ", Style::default().fg(Color::Gray))
        }
    } else {
        Span::raw("   ")
    };

    // Online players get bright green + bold names (overrides the gray
    // historical-only color, which can't be true for online players anyway —
    // scan_players already flips historical_only=false for online).
    let (name_color, name_modifier) = if p.is_online {
        (Color::Green, Modifier::BOLD)
    } else if p.historical_only {
        (Color::Gray, Modifier::empty())
    } else {
        (Color::White, Modifier::empty())
    };
    let name_label = if p.historical_only {
        format!(" {} {} ", p.name, s.players_historical_marker)
    } else {
        format!(" {} ", p.name)
    };

    let mut spans: Vec<Span> = vec![
        online_span,
        wl_span,
        op_span,
        Span::styled(
            format!("{:24}", truncate_display(&name_label, 24)),
            Style::default().fg(name_color).add_modifier(name_modifier),
        ),
        Span::styled(
            format!(" {:36} ", &p.uuid),
            Style::default().fg(Color::Gray),
        ),
    ];
    if let Some(ts) = p.last_denied_at {
        let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0);
        let date_str = dt
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "?".to_string());
        spans.push(Span::styled(
            format!("{} {}", s.players_denied_recently, date_str),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }
    ListItem::new(Line::from(spans))
}

fn draw_players_detail(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    // Detail panel: TOP separator (with title) + LEFT separator (vertical
    // line between list and detail). Together they form an "L" that reads as
    // a clear section divider without a full box.
    let block = Block::default()
        .borders(Borders::TOP | Borders::LEFT)
        .title(s.detail_title);
    let lines: Vec<Line> = match app.players_state.selected().and_then(|i| app.players.get(i)) {
        None => vec![Line::from(Span::styled(
            s.detail_no_selection,
            Style::default().fg(Color::Gray),
        ))],
        Some(p) => {
            let yn = |b: bool| if b { s.detail_yes } else { s.detail_no };
            let mut out = vec![
                kv_line_bold(&p.name, Color::Cyan),
                Line::raw(""),
                Line::from(Span::styled(
                    format!("{}:", s.detail_uuid),
                    Style::default().fg(Color::Gray),
                )),
                Line::from(Span::styled(
                    p.uuid.clone(),
                    Style::default().fg(Color::White),
                )),
                Line::raw(""),
                kv_line_label(s.players_col_wl, yn(p.in_whitelist)),
            ];
            match p.op_level {
                Some(level) => {
                    out.push(kv_line_label(s.detail_level, &level.to_string()));
                    out.push(kv_line_label(s.detail_level_meaning, op_level_meaning(s, level)));
                }
                None => {
                    out.push(kv_line_label(s.players_col_op, s.detail_no));
                }
            }
            if let Some(ts) = p.last_denied_at {
                let date_str = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
                    .map(|d| d.format("%Y-%m-%d").to_string())
                    .unwrap_or_else(|| "?".into());
                out.push(kv_line_label(s.players_col_denied, &date_str));
            }
            out.push(Line::raw(""));
            out.push(Line::from(Span::styled(
                s.detail_offline_uuid_note.to_string(),
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::ITALIC),
            )));
            out
        }
    };
    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_config(f: &mut Frame, area: Rect, app: &mut App) {
    let lang = app.lang;
    let (list_area, detail_area) = split_list_detail(area);
    let items: Vec<ListItem> = app
        .properties
        .iter()
        .map(|(k, v)| {
            let value_color = match v.as_str() {
                "true" => Color::Green,
                "false" => Color::Red,
                _ => Color::Cyan,
            };
            let mut spans = vec![
                Span::styled(format!(" {:35}", k), Style::default().fg(Color::White)),
                Span::raw("= "),
                Span::styled(v.clone(), Style::default().fg(value_color)),
            ];
            // In zh mode, append a dim Chinese annotation if we know one for this key.
            if lang == Lang::Zh {
                if let Some(annot) = property_zh(k) {
                    spans.push(Span::raw("    "));
                    spans.push(Span::styled(
                        format!("// {}", annot),
                        Style::default().fg(Color::Gray).add_modifier(Modifier::ITALIC),
                    ));
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .title(app.lang.s().title_config),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, list_area, &mut app.config_state);
    if let Some(da) = detail_area {
        draw_config_detail(f, da, app);
    }
}

fn draw_config_detail(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    // Detail panel: TOP separator (with title) + LEFT separator (vertical
    // line between list and detail). Together they form an "L" that reads as
    // a clear section divider without a full box.
    let block = Block::default()
        .borders(Borders::TOP | Borders::LEFT)
        .title(s.detail_title);
    let lines: Vec<Line> = match app
        .config_state
        .selected()
        .and_then(|i| app.properties.get(i))
    {
        None => vec![Line::from(Span::styled(
            s.detail_no_selection,
            Style::default().fg(Color::Gray),
        ))],
        Some((k, v)) => {
            let mut out = vec![
                kv_line_label(s.detail_key, k),
                kv_line_label(s.detail_value, v),
                Line::raw(""),
            ];
            match property_metadata(k) {
                Some(m) => {
                    let yn = if m.restart_required { s.detail_yes } else { s.detail_no };
                    out.push(kv_line_label(s.detail_default, m.default));
                    out.push(kv_line_label(s.detail_range, m.range));
                    out.push(kv_line_label(s.detail_restart_required, yn));
                    out.push(Line::raw(""));
                    out.push(Line::from(Span::styled(
                        format!("{}:", s.detail_description),
                        Style::default().fg(Color::Gray),
                    )));
                    let desc = match app.lang {
                        Lang::En => m.description_en,
                        Lang::Zh => m.description_zh,
                    };
                    out.push(Line::from(Span::styled(
                        desc.to_string(),
                        Style::default().fg(Color::White),
                    )));
                }
                None => {
                    out.push(Line::raw(""));
                    out.push(Line::from(Span::styled(
                        s.detail_no_metadata.to_string(),
                        Style::default()
                            .fg(Color::Gray)
                            .add_modifier(Modifier::ITALIC),
                    )));
                }
            }
            out
        }
    };
    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}


/// Colorize one log line based on a substring scan. Cheap (single linear scan)
/// and good enough — a real log parser would be overkill for tail-mode.
pub fn colorize_log_line(line: &str) -> Vec<Span<'static>> {
    let lower = line.to_ascii_lowercase();
    let style = if line.contains("[ERROR]") || lower.contains("/error]:") {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if line.contains("[WARN]") || lower.contains("/warn]:") {
        Style::default().fg(Color::Yellow)
    } else if line.contains("joined the game") {
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else if line.contains("left the game") || line.contains("lost connection") {
        Style::default().fg(Color::Cyan)
    } else if line.contains("Done (") && line.contains("s)! For help, type") {
        // Server fully started — important breadcrumb when scanning by eye.
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::White)
    };
    vec![Span::styled(line.to_string(), style)]
}

/// Settings-tab file picker. Shows `server.properties` as a virtual entry at
/// index 0 (so it always appears even though it isn't in `yaml_files`),
/// followed by the discovered YAML files. Selecting an entry transitions into
/// the appropriate editor view (Properties or YAML row editor).
fn draw_settings_files(f: &mut Frame, area: Rect, app: &mut App) {
    let zh = matches!(app.lang, Lang::Zh);
    let title = if zh {
        " 设置 — 选文件 (Enter 打开) "
    } else {
        " Settings — pick file (Enter opens) "
    };

    let mut items: Vec<ListItem> = Vec::with_capacity(1 + app.yaml_files.len());
    // Virtual entry 0: server.properties — keyed differently so it stands
    // out as the canonical settings file.
    items.push(ListItem::new(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "server.properties",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if zh { "    (核心配置)" } else { "    (core config)" },
            Style::default().fg(Color::Gray),
        ),
    ])));
    for p in &app.yaml_files {
        let display = p
            .strip_prefix(&app.server_dir)
            .unwrap_or(p)
            .display()
            .to_string();
        items.push(ListItem::new(Line::from(Span::styled(
            format!(" {}", display),
            Style::default().fg(Color::White),
        ))));
    }

    let list = List::new(items)
        .block(Block::default().borders(Borders::TOP).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD))
        .highlight_symbol("> ");
    // Default to entry 0 (server.properties) so a fresh tab visit always has
    // something selected.
    if app.yaml_files_state.selected().is_none() {
        app.yaml_files_state.select(Some(0));
    }
    f.render_stateful_widget(list, area, &mut app.yaml_files_state);
}

fn draw_yaml(f: &mut Frame, area: Rect, app: &mut App) {
    let s = app.lang.s();
    match &app.yaml_view {
        // Files / Properties are handled by draw_settings_files / draw_config
        // before this fn is reached. Treat them as no-op safety nets.
        YamlView::Files | YamlView::Properties => {}
        YamlView::Editing { file_idx } => {
            let path = app
                .yaml_files
                .get(*file_idx)
                .cloned()
                .unwrap_or_default();
            let title = format!("{}{} ", s.title_yaml_edit_fmt, path.display());
            let items: Vec<ListItem> = app
                .yaml_rows
                .iter()
                .map(|row| {
                    let indent_str: String = (0..row.indent).map(|_| "  ").collect();
                    let mut spans = vec![
                        Span::raw(" "),
                        Span::raw(indent_str),
                        Span::styled(row.label.clone(), Style::default().fg(Color::White)),
                    ];
                    match &row.value {
                        YamlDisplay::Branch => {
                            spans.push(Span::styled(
                                s.yaml_branch_marker,
                                Style::default().fg(Color::Gray),
                            ));
                        }
                        YamlDisplay::Scalar(v) => {
                            spans.push(Span::raw(": "));
                            let color = match v.as_str() {
                                "true" => Color::Green,
                                "false" => Color::Red,
                                _ => Color::Cyan,
                            };
                            spans.push(Span::styled(v.clone(), Style::default().fg(color)));
                        }
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();
            let list = List::new(items)
                .block(Block::default().borders(Borders::TOP).title(title))
                .highlight_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD))
                .highlight_symbol("> ");
            f.render_stateful_widget(list, area, &mut app.yaml_rows_state);
        }
    }
}

pub fn fmt_age(d: chrono::Duration) -> String {
    let total_secs = d.num_seconds().max(0);
    if total_secs < 60 {
        format!("{}s ago", total_secs)
    } else if total_secs < 3600 {
        format!("{}m ago", total_secs / 60)
    } else if total_secs < 86400 {
        format!("{}h ago", total_secs / 3600)
    } else if total_secs < 86400 * 60 {
        format!("{}d ago", total_secs / 86400)
    } else {
        format!("{}mo ago", total_secs / (86400 * 30))
    }
}

fn draw_sakurafrp(f: &mut Frame, area: Rect, app: &mut App) {
    // v0.17 — Network tab layout:
    //   * user panel (4 or 5 lines based on token state)
    //   * tunnel list (flex)
    //   * NIC list — collapsed (0) by default; expand with `n` (varies)
    //
    // The 4-line "actions" panel from v0.15.1 is gone — keys live in the
    // bottom hint row + `?` overlay covers any forgotten ones.
    let user_h: u16 = if app.natfrp_token.is_none() { 5 } else { 4 };
    let nics = detect_interfaces();
    let nics_h: u16 = if app.network_show_nics {
        // 1 header + per-NIC rows + 1 frp row + 1 trailing blank
        (nics.len() as u16 + 2).min(10)
    } else {
        1 // single "n 展开 NIC 列表" hint row
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(user_h),
            Constraint::Min(3),
            Constraint::Length(nics_h),
        ])
        .split(area);

    draw_sakurafrp_user(f, chunks[0], app);
    draw_sakurafrp_tunnels(f, chunks[1], app);
    draw_network_nics(f, chunks[2], app, &nics);
}

/// Network-tab NIC list. Collapsed by default — single hint line says
/// "press `n` to expand". When expanded, lists every detected interface +
/// the frp address marker so the user can pick a non-primary IP for the
/// rare "friend on a specific VPN" case.
fn draw_network_nics(f: &mut Frame, area: Rect, app: &mut App, nics: &[NicInfo]) {
    let zh = matches!(app.lang, Lang::Zh);
    let port: u16 = get_property(&app.properties, "server-port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(25565);

    if !app.network_show_nics {
        let p = Paragraph::new(Line::from(vec![
            Span::styled(
                if zh {
                    " ── 网卡 (n 展开) ──────────"
                } else {
                    " ── NICs (n to expand) ──────────"
                },
                Style::default().fg(Color::Gray),
            ),
        ]));
        f.render_widget(p, area);
        return;
    }

    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        if zh { " ── 网卡 (n 折叠) ──────────" } else { " ── NICs (n to collapse) ──────────" },
        Style::default().fg(Color::Gray),
    ))];
    for n in nics {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(format!("{:14}", n.name), Style::default().fg(Color::White)),
            Span::raw("  "),
            Span::styled(
                format!("{}:{}", n.ip, port),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!("[{}]", nic_kind_label(app.lang, n.kind)),
                Style::default().fg(nic_kind_color(n.kind)),
            ),
        ]));
    }
    if let Some(addr) = app.effective_sakurafrp_address() {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(format!("{:14}", "frp"), Style::default().fg(Color::White)),
            Span::raw("  "),
            Span::styled(
                addr,
                Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn draw_sakurafrp_user(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    let lines: Vec<Line> = if app.natfrp_token.is_none() {
        // v0.12 onboarding: numbered 3-step intro replacing the cryptic "(no
        // token set)" one-liner. Each step calls out the action so users with
        // ADHD don't lose the thread when they alt-tab to the browser.
        vec![
            Line::from(Span::styled(
                format!(" {}", s.sf_onboarding_step1),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!(" {}", s.sf_onboarding_step2),
                Style::default().fg(Color::White),
            )),
            Line::from(Span::styled(
                format!(" {}", s.sf_onboarding_step3),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
        ]
    } else if let Some(u) = &app.natfrp_user {
        let token_disp = app
            .natfrp_token
            .as_deref()
            .map(crate::natfrp::redact_token)
            .unwrap_or_default();

        // v0.12 — color-code traffic by % used. Color choice extracted to
        // `traffic_color_for` so it can be unit-tested without spinning up a
        // ratatui frame. Plan stops short of hard-blocking the user.
        let (traffic_text, traffic_color) = if u.traffic.len() == 2 {
            let used = u.traffic[0];
            let total = u.traffic[1];
            let pct = traffic_pct(used, total);
            (
                format!(
                    "{}: {} / {} ({:.0}%)  ({})",
                    s.sf_user_traffic_label,
                    crate::natfrp::fmt_bytes(used),
                    crate::natfrp::fmt_bytes(total),
                    pct,
                    u.speed,
                ),
                traffic_color_for(pct),
            )
        } else {
            (
                format!("{}: ({})", s.sf_user_traffic_label, u.speed),
                Color::White,
            )
        };

        vec![
            Line::from(vec![
                Span::styled(
                    format!(" {} ", u.name),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{}: {}", s.sf_user_token_label, token_disp),
                    Style::default().fg(Color::Gray),
                ),
                Span::raw("   "),
                Span::styled(
                    format!("{}: {}", s.sf_user_plan_label, u.group.name),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::raw(" "),
                Span::styled(traffic_text, Style::default().fg(traffic_color)),
            ]),
        ]
    } else if let Some(err) = &app.natfrp_last_error {
        // err is already translated by refresh_natfrp; render verbatim.
        vec![Line::from(Span::styled(
            format!(" {}", err),
            Style::default().fg(Color::Red),
        ))]
    } else {
        vec![Line::from(Span::styled(
            s.sf_user_loading,
            Style::default().fg(Color::Gray),
        ))]
    };

    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::TOP)
            .title(s.title_sakurafrp_user),
    );
    f.render_widget(p, area);
}

fn draw_sakurafrp_tunnels(f: &mut Frame, area: Rect, app: &mut App) {
    let s = app.lang.s();

    // No token: tunnels panel mirrors the user-panel onboarding so the user's
    // eye doesn't have to scan two contradicting empty messages.
    if app.natfrp_token.is_none() {
        let p = Paragraph::new(vec![Line::from(Span::styled(
            format!(" {}", s.sf_user_no_token),
            Style::default().fg(Color::Gray),
        ))])
        .block(
            Block::default()
                .borders(Borders::TOP)
                .title(s.title_sakurafrp_tunnels),
        );
        f.render_widget(p, area);
        return;
    }

    // Token set, refresh hasn't fired yet.
    if app.natfrp_tunnels.is_empty() && !app.natfrp_loaded {
        let p = Paragraph::new(Line::from(Span::styled(
            s.sf_tunnels_loading,
            Style::default().fg(Color::Gray),
        )))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .title(s.title_sakurafrp_tunnels),
        );
        f.render_widget(p, area);
        return;
    }

    // If the API call errored before we got any user info, don't claim "no
    // tunnels" — we genuinely don't know. Show a neutral message and let the
    // error in the user panel above carry the actionable diagnosis.
    if app.natfrp_tunnels.is_empty()
        && app.natfrp_last_error.is_some()
        && app.natfrp_user.is_none()
    {
        let p = Paragraph::new(Line::from(Span::styled(
            format!(" {}", s.sf_tunnels_loading),
            Style::default().fg(Color::Gray),
        )))
        .block(
            Block::default()
                .borders(Borders::TOP)
                .title(s.title_sakurafrp_tunnels),
        );
        f.render_widget(p, area);
        return;
    }

    // Refresh succeeded, account exists, but the user hasn't created any
    // tunnels yet. v0.12 swaps the curt one-liner for a 3-option fork that
    // forwards-references v0.13's `c` command without depending on it.
    if app.natfrp_tunnels.is_empty() {
        let lines = vec![
            Line::from(Span::styled(
                format!(" {}", s.sf_tunnels_empty_header),
                Style::default().fg(Color::Gray),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                s.sf_tunnels_empty_option_v013,
                Style::default().fg(Color::Gray),
            )),
            Line::from(Span::styled(
                s.sf_tunnels_empty_option_browser_a,
                Style::default().fg(Color::White),
            )),
            Line::from(Span::styled(
                s.sf_tunnels_empty_option_browser_b,
                Style::default().fg(Color::White),
            )),
            Line::from(Span::styled(
                s.sf_tunnels_empty_option_launcher,
                Style::default().fg(Color::White),
            )),
        ];
        let p = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::TOP)
                .title(s.title_sakurafrp_tunnels),
        );
        f.render_widget(p, area);
        return;
    }

    // Column widths chosen for the common case (10-char id, 16-char tunnel
    // name, 28-char node label, 4-char type, public address rest). On narrow
    // terminals ratatui will truncate naturally.
    let header = Line::from(vec![
        // Two leading spaces match the online marker column; the third covers
        // the v0.14 enable/disable marker column.
        Span::raw("    "),
        Span::styled(
            format!("{:<10}", s.sf_col_id),
            Style::default().fg(Color::Gray),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:<18}", s.sf_col_name),
            Style::default().fg(Color::Gray),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:<28}", s.sf_col_node),
            Style::default().fg(Color::Gray),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:<5}", s.sf_col_type),
            Style::default().fg(Color::Gray),
        ),
        Span::raw(" "),
        Span::styled(s.sf_col_address, Style::default().fg(Color::Gray)),
    ]);

    let nodes = &app.natfrp_nodes;
    let enabled_map = app.natfrp_tunnel_enabled.clone();
    let items: Vec<ListItem> = std::iter::once(ListItem::new(header))
        .chain(app.natfrp_tunnels.iter().map(|t| {
            let node_label = crate::natfrp::node_label(t.node, nodes);
            let addr = crate::natfrp::public_address(t, nodes).unwrap_or_else(|| "—".to_string());
            let online_marker = if t.online { "●" } else { "○" };
            let online_color = if t.online { Color::Green } else { Color::Gray };
            // v0.14 — enable/disable marker from launcher state. `?` when the
            // launcher hasn't been reached this session (no docker, no password,
            // TLS failure, …).
            let (enable_marker, enable_color) = match enabled_map.get(&t.id) {
                Some(true) => ("▶", Color::Green),
                Some(false) => ("■", Color::Yellow),
                None => ("?", Color::Gray),
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{} ", online_marker),
                    Style::default().fg(online_color),
                ),
                Span::styled(
                    format!("{} ", enable_marker),
                    Style::default().fg(enable_color),
                ),
                Span::styled(
                    format!("{:<10}", t.id),
                    Style::default().fg(Color::White),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<18}", truncate_display(&t.name, 18)),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<28}", truncate_display(&node_label, 28)),
                    Style::default().fg(Color::White),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<5}", t.kind),
                    Style::default().fg(Color::Magenta),
                ),
                Span::raw(" "),
                Span::styled(
                    addr,
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ),
            ]))
        }))
        .collect();

    // Selection state in the App is indexed against `natfrp_tunnels`; we render
    // a header row at index 0 plus tunnels at index 1+. So we shadow `app.natfrp_state`
    // with a temporary state shifted by +1 for the duration of this render.
    let mut shifted = app.natfrp_state.clone();
    if let Some(i) = shifted.selected() {
        shifted.select(Some(i + 1));
    }
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .title(s.title_sakurafrp_tunnels),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD))
        .highlight_symbol("");
    f.render_stateful_widget(list, area, &mut shifted);
}

/// Percentage-used for the SakuraFrp traffic plan. Guards against division by
/// zero (some accounts / responses report `total = 0` for unlimited plans).
pub fn traffic_pct(used: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    (used as f64 / total as f64) * 100.0
}

/// Map a usage % to the color the traffic line should render in. Thresholds
/// match the v0.12 plan: ≥95 red, ≥80 yellow, else white.
pub fn traffic_color_for(pct: f64) -> Color {
    if pct >= 95.0 {
        Color::Red
    } else if pct >= 80.0 {
        Color::Yellow
    } else {
        Color::White
    }
}

/// Strip a backup file's redundant world-name prefix + archive extension so
/// the inline backup list in Worlds detail is scan-able. `world-20260501.tar.zst`
/// for world `world` becomes just `20260501`. Pure: tested in unit tests.
pub fn backup_short_name(filename: &str, world: Option<&str>) -> String {
    let mut s = filename.to_string();
    if let Some(w) = world {
        let prefix = format!("{}-", w);
        if let Some(rest) = s.strip_prefix(&prefix) {
            s = rest.to_string();
        }
    }
    for suffix in &[".tar.zst", ".tar.gz", ".tar.xz", ".tar.bz2", ".tar", ".zip"] {
        if let Some(rest) = s.strip_suffix(suffix) {
            s = rest.to_string();
            break;
        }
    }
    s
}

/// Truncate `s` to at most `max_cols` display columns (unicode-width aware),
/// adding an ellipsis if truncation occurred.
fn truncate_display(s: &str, max_cols: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if UnicodeWidthStr::width(s) <= max_cols {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw + 1 > max_cols {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

fn draw_hints(f: &mut Frame, area: Rect, app: &App) {
    // 1-line plain footer. Hint only — transient status moved to floating
    // toast in v0.16 (rendered separately by `draw_toast`).
    let hint = hint_for(app.lang, app.tab, &app.yaml_view);
    let line = Line::from(Span::styled(
        format!(" {}", hint),
        Style::default().fg(Color::Gray),
    ));
    let p = Paragraph::new(line);
    f.render_widget(p, area);
}

/// Floating toast in the bottom-right of the content area. Active toast wins;
/// otherwise legacy `app.status` is rendered as a non-fading Info line so the
/// pre-toast callsites still surface their messages without each one having
/// to be migrated. Phase 10 cleanup will retire `app.status` entirely.
fn draw_toast(f: &mut Frame, content_area: Rect, app: &App) {
    use unicode_width::UnicodeWidthStr;

    let (msg, kind): (String, ToastKind) = if let Some(t) = app.active_toast() {
        (t.message.clone(), t.kind)
    } else if !app.status.is_empty() {
        (app.status.clone(), crate::classify_status(&app.status))
    } else {
        return;
    };

    let color = match kind {
        ToastKind::Ok => Color::Green,
        ToastKind::Info => Color::Cyan,
        ToastKind::Warn => Color::Yellow,
        ToastKind::Err => Color::Red,
    };

    // Render at the very last row of the content area, right-aligned. Single
    // line; truncate if it doesn't fit. Leave a 1-col right margin so the
    // text doesn't kiss the screen edge.
    let max_w = content_area.width.saturating_sub(2) as usize;
    let display = if UnicodeWidthStr::width(msg.as_str()) > max_w {
        truncate_display(&msg, max_w)
    } else {
        msg.clone()
    };
    let w = UnicodeWidthStr::width(display.as_str()) as u16;
    let y = content_area.y + content_area.height.saturating_sub(1);
    let x = content_area.x + content_area.width.saturating_sub(w + 1);
    let toast_rect = Rect {
        x,
        y,
        width: w,
        height: 1,
    };
    let p = Paragraph::new(Span::styled(
        display,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
    f.render_widget(ratatui::widgets::Clear, toast_rect);
    f.render_widget(p, toast_rect);
}

fn draw_prompt(f: &mut Frame, prompt: &InputPrompt, lang: Lang) {
    let area = centered_rect(60, 5, f.area());
    f.render_widget(ratatui::widgets::Clear, area);
    // Modal — full border to differentiate from in-tab content.
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", prompt.title));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines = vec![
        Line::from(vec![
            Span::styled(format!("{}: ", prompt.label), Style::default().fg(Color::White)),
            Span::styled(&prompt.buffer, Style::default().fg(Color::Yellow)),
            Span::styled(
                "█",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::SLOW_BLINK),
            ),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            lang.s().prompt_confirm_cancel,
            Style::default().fg(Color::Gray),
        )),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn centered_rect(w_pct: u16, h_lines: u16, area: Rect) -> Rect {
    let w = area.width.saturating_mul(w_pct) / 100;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h_lines)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h_lines.min(area.height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LogsLevelFilter;

    #[test]
    fn is_hms_recognizes_paper_timestamps() {
        assert!(is_hms("17:59:18"));
        assert!(is_hms("17:59:18.523")); // longer suffix is fine
        assert!(!is_hms("17:5918"));
        assert!(!is_hms("17-59-18"));
        assert!(!is_hms("foo"));
    }

    #[test]
    fn strip_player_action_handles_join_leave_lines() {
        // The `after` arg is everything after the timestamp's closing `]`.
        let after = " [Server thread/INFO]: NihilDigit joined the game";
        assert_eq!(
            strip_player_action(after, "joined the game").as_deref(),
            Some("NihilDigit")
        );
        assert_eq!(
            strip_player_action(after, "left the game"),
            None,
            "wrong action shouldn't match"
        );
        // Empty name is rejected so we don't surface "  joined" rows.
        let empty = " [Server thread/INFO]:  joined the game";
        assert_eq!(strip_player_action(empty, "joined the game"), None);
    }

    #[test]
    fn extract_name_field_pulls_username_from_disconnect_line() {
        let line = "[20:00:00] [User Authenticator/INFO]: Disconnecting com.mojang.authlib.GameProfile@xx[id=<...>,name=NihilDigit,properties={},legacy=false] (/192.168.1.1:55555): You are not whitelisted on this server!";
        assert_eq!(extract_name_field(line).as_deref(), Some("NihilDigit"));
        // Line without a name= field returns None.
        assert_eq!(extract_name_field("plain log line"), None);
    }

    #[test]
    fn extract_done_duration_grabs_seconds() {
        let line = "[20:07:18] [Server thread/INFO]: Done (5.996s)! For help, type \"help\"";
        assert_eq!(extract_done_duration(line).as_deref(), Some("5.996s"));
        // Server logs always have decimal — but be defensive.
        assert_eq!(
            extract_done_duration("[20:07:18] [Server thread/INFO]: Done (12s)! For help").as_deref(),
            Some("12s")
        );
    }

    #[test]
    fn translate_event_filters_chunk_system_noise() {
        // The kind of line we DON'T want to surface — it has a timestamp and
        // looks log-shaped but isn't an event the host cares about.
        let noise = "[17:59:18] [Server thread/INFO]: [ChunkHolderManager] Halted I/O scheduler for world 'minecraft:overworld'";
        assert!(translate_event(noise, Lang::En).is_none());
        // And the kind we DO want.
        let join = "[17:59:18] [Server thread/INFO]: NihilDigit joined the game";
        assert!(translate_event(join, Lang::En).is_some());
        let warn = "[17:59:18] [Server thread/WARN]: Some warning";
        assert!(translate_event(warn, Lang::En).is_some());
        let err = "[17:59:18] [Server thread/ERROR]: Boom";
        assert!(translate_event(err, Lang::En).is_some());
    }

    #[test]
    fn translate_event_skips_non_timestamped_lines() {
        // Wrap-continuation lines (like "'minecraft:the_nether' in 0.00s")
        // don't have a leading timestamp — drop them.
        assert!(translate_event("'minecraft:the_nether' in 0.00s", Lang::En).is_none());
    }

    #[test]
    fn backup_short_name_strips_world_and_extension() {
        // World prefix removed when matching, archive extension dropped.
        assert_eq!(
            backup_short_name("fuchenling-20260501-180012.tar.zst", Some("fuchenling")),
            "20260501-180012"
        );
        // Different world → only extension stripped.
        assert_eq!(
            backup_short_name("world-20260501-180012.tar.zst", Some("fuchenling")),
            "world-20260501-180012"
        );
        // No world hint → leave the name except the extension.
        assert_eq!(
            backup_short_name("backup-20260501.tar.gz", None),
            "backup-20260501"
        );
        // Unknown extension → leave intact.
        assert_eq!(
            backup_short_name("snap-20260501.dat", Some("snap")),
            "20260501.dat"
        );
    }

    #[test]
    fn level_filter_passes_everything_when_all() {
        // ALL filter: every line passes regardless of bracket markers.
        assert!(line_matches_level("[12:00:00] [Server thread/INFO]: hi", LogsLevelFilter::All));
        assert!(line_matches_level("[12:00:00] [Server thread/WARN]: hm", LogsLevelFilter::All));
        assert!(line_matches_level("[12:00:00] [Server thread/ERROR]: !!", LogsLevelFilter::All));
        assert!(line_matches_level("plain line", LogsLevelFilter::All));
    }

    #[test]
    fn level_filter_error_only_lets_errors_through() {
        assert!(line_matches_level("[12:00] [worker/ERROR]: boom", LogsLevelFilter::Error));
        assert!(line_matches_level("Found [ERROR] tag", LogsLevelFilter::Error));
        assert!(!line_matches_level("[12:00] [Server thread/INFO]: hi", LogsLevelFilter::Error));
        assert!(!line_matches_level("[12:00] [Server thread/WARN]: hm", LogsLevelFilter::Error));
    }

    #[test]
    fn level_filter_warn_includes_errors() {
        // WARN filter: include WARN *and* ERROR (errors are at-least-as-bad).
        // The user looking for "interesting" output sees both.
        assert!(line_matches_level("[Server/WARN]: hm", LogsLevelFilter::Warn));
        assert!(line_matches_level("[Server/ERROR]: !!", LogsLevelFilter::Warn));
        assert!(!line_matches_level("[Server/INFO]: hi", LogsLevelFilter::Warn));
    }

    #[test]
    fn level_filter_info_excludes_warn_and_error() {
        // INFO filter: hide WARN/ERROR so the user can see just normal flow.
        assert!(line_matches_level("[Server/INFO]: hi", LogsLevelFilter::Info));
        assert!(line_matches_level("plain line", LogsLevelFilter::Info));
        assert!(!line_matches_level("[Server/WARN]: hm", LogsLevelFilter::Info));
        assert!(!line_matches_level("[Server/ERROR]: !!", LogsLevelFilter::Info));
    }

    #[test]
    fn traffic_pct_handles_zero_total() {
        // Unlimited / unconfigured plans report total=0; we must not divide by
        // zero (would render as NaN% in the UI).
        assert_eq!(traffic_pct(123_456, 0), 0.0);
    }

    #[test]
    fn traffic_pct_examples() {
        assert!((traffic_pct(0, 100) - 0.0).abs() < 1e-9);
        assert!((traffic_pct(50, 100) - 50.0).abs() < 1e-9);
        assert!((traffic_pct(100, 100) - 100.0).abs() < 1e-9);
        // Over-quota: don't clamp — the user should see >100% so they know
        // they've burst past the plan.
        assert!((traffic_pct(150, 100) - 150.0).abs() < 1e-9);
    }

    #[test]
    fn traffic_color_thresholds() {
        // Below 80% → white (default; not "alarming")
        assert_eq!(traffic_color_for(0.0), Color::White);
        assert_eq!(traffic_color_for(79.9), Color::White);
        // 80-94.9% → yellow heads-up
        assert_eq!(traffic_color_for(80.0), Color::Yellow);
        assert_eq!(traffic_color_for(94.9), Color::Yellow);
        // 95%+ → red, tunnels may stop forwarding
        assert_eq!(traffic_color_for(95.0), Color::Red);
        assert_eq!(traffic_color_for(100.0), Color::Red);
        assert_eq!(traffic_color_for(150.0), Color::Red);
    }
}
