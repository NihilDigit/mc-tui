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
    detect_interfaces, fmt_bytes, get_property, nic_kind_color, nic_kind_label, DockerState,
    NicInfo, NicKind, YamlDisplay,
};
use crate::i18n::{
    fmt_log_read_error, fmt_status_running, hint_for, property_metadata, property_zh,
    server_action_label, tab_name, Lang, Strings,
};
use crate::{App, InputPrompt, TabId, YamlView, SERVER_ACTIONS, TABS};

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
            Constraint::Length(3), // hints / status line
        ])
        .split(f.area());

    draw_header_line(f, chunks[0], app);
    draw_tabs(f, chunks[1], app);
    app.tabs_rect = chunks[1];
    app.list_rect = chunks[2];
    match app.tab {
        TabId::Worlds => draw_worlds(f, chunks[2], app),
        TabId::Whitelist => draw_whitelist(f, chunks[2], app),
        TabId::Ops => draw_ops(f, chunks[2], app),
        TabId::Config => draw_config(f, chunks[2], app),
        TabId::Logs => draw_logs(f, chunks[2], app),
        TabId::Yaml => draw_yaml(f, chunks[2], app),
        TabId::Backups => draw_backups(f, chunks[2], app),
        TabId::Server => draw_server(f, chunks[2], app),
        TabId::SakuraFrp => draw_sakurafrp(f, chunks[2], app),
    }
    draw_hints(f, chunks[3], app);

    if let Some(prompt) = app.prompt.clone() {
        draw_prompt(f, &prompt, app.lang);
    }
}

/// One-line header: status + level + dir + primary connect chip.
/// No border — saves vertical space and keeps key info above the tab bar.
/// Click the chip to copy `<ip>:<port>` to the clipboard via wl-copy.
fn draw_header_line(f: &mut Frame, area: Rect, app: &mut App) {
    use unicode_width::UnicodeWidthStr;

    let s = app.lang.s();
    let pid_text = match app.pid {
        Some(p) => Span::styled(fmt_status_running(app.lang, p), Style::default().fg(Color::Green)),
        None => Span::styled(s.status_stopped, Style::default().fg(Color::DarkGray)),
    };

    let nics = detect_interfaces();
    let port: u16 = get_property(&app.properties, "server-port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(25565);
    let primary = nics.iter().find(|n| {
        !matches!(n.kind, NicKind::Loopback | NicKind::Docker | NicKind::Tun)
    });

    app.join_chips.clear();

    // Build the line as spans; track chip x by accumulating display widths.
    let sep = "   ";
    let mut spans: Vec<Span> = vec![
        Span::styled(s.server_label, Style::default().add_modifier(Modifier::DIM)),
        pid_text,
        Span::raw(sep),
        Span::styled(s.level_label, Style::default().add_modifier(Modifier::DIM)),
        Span::styled(app.current_level().to_string(), Style::default().fg(Color::Cyan)),
        Span::raw(sep),
        Span::styled(s.dir_label, Style::default().add_modifier(Modifier::DIM)),
        Span::raw(app.server_dir.display().to_string()),
        Span::raw(sep),
    ];

    if let Some(n) = primary {
        let kind_label = nic_kind_label(app.lang, n.kind);
        let kind_span_text = format!("[{}]", kind_label);
        let chip_text = format!("{}:{}", n.ip, port);

        // Sum the display width of every span before the chip — that's where the
        // chip starts on screen. UnicodeWidthStr handles wide CJK + emoji correctly.
        let mut chip_x = area.x;
        for sp in &spans {
            chip_x = chip_x.saturating_add(UnicodeWidthStr::width(sp.content.as_ref()) as u16);
        }
        chip_x = chip_x.saturating_add(UnicodeWidthStr::width(kind_span_text.as_str()) as u16);
        chip_x = chip_x.saturating_add(1); // space between [kind] and chip
        let chip_w = UnicodeWidthStr::width(chip_text.as_str()) as u16;
        let chip_rect = Rect {
            x: chip_x,
            y: area.y,
            width: chip_w,
            height: 1,
        };
        app.join_chips.push((chip_rect, chip_text.clone()));

        spans.push(Span::styled(
            kind_span_text,
            Style::default().fg(nic_kind_color(n.kind)).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            chip_text,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ));
    } else {
        spans.push(Span::styled(
            match app.lang {
                Lang::En => "(no LAN/Public IPv4)",
                Lang::Zh => "(无可用 IPv4)",
            },
            Style::default().fg(Color::DarkGray),
        ));
    }

    let p = Paragraph::new(Line::from(spans));
    f.render_widget(p, area);
}

fn draw_tabs(f: &mut Frame, area: Rect, app: &App) {
    let titles: Vec<Line> = TABS
        .iter()
        .enumerate()
        .map(|(i, (id, _en))| {
            Line::from(format!(" {} {} ", i + 1, tab_name(app.lang, *id)))
        })
        .collect();
    let selected = TABS.iter().position(|(t, _)| *t == app.tab).unwrap_or(0);
    let tabs = Tabs::new(titles)
        .select(selected)
        .style(Style::default().fg(Color::White))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, area);
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
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(when, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(app.lang.s().title_worlds))
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
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
        Span::styled(format!("{}: ", label), Style::default().fg(Color::DarkGray)),
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
    let block = Block::default().borders(Borders::ALL).title(s.detail_title);
    let lines: Vec<Line> = match app.worlds_state.selected().and_then(|i| app.worlds.get(i)) {
        None => vec![Line::from(Span::styled(
            s.detail_no_selection,
            Style::default().fg(Color::DarkGray),
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
    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_whitelist(f: &mut Frame, area: Rect, app: &mut App) {
    let (list_area, detail_area) = split_list_detail(area);
    let items: Vec<ListItem> = app
        .whitelist
        .iter()
        .map(|e| {
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {:20} ", e.name), Style::default().fg(Color::White)),
                Span::styled(&e.uuid, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(app.lang.s().title_whitelist))
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, list_area, &mut app.whitelist_state);
    if let Some(da) = detail_area {
        draw_whitelist_detail(f, da, app);
    }
}

fn draw_whitelist_detail(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    let block = Block::default().borders(Borders::ALL).title(s.detail_title);
    let lines: Vec<Line> = match app
        .whitelist_state
        .selected()
        .and_then(|i| app.whitelist.get(i))
    {
        None => vec![Line::from(Span::styled(
            s.detail_no_selection,
            Style::default().fg(Color::DarkGray),
        ))],
        Some(e) => vec![
            kv_line_bold(&e.name, Color::Cyan),
            Line::raw(""),
            Line::from(Span::styled(
                format!("{}:", s.detail_uuid),
                Style::default().fg(Color::DarkGray),
            )),
            Line::from(Span::styled(
                e.uuid.clone(),
                Style::default().fg(Color::White),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                s.detail_offline_uuid_note.to_string(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )),
        ],
    };
    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_ops(f: &mut Frame, area: Rect, app: &mut App) {
    let (list_area, detail_area) = split_list_detail(area);
    let items: Vec<ListItem> = app
        .ops
        .iter()
        .map(|e| {
            ListItem::new(Line::from(vec![
                Span::styled(format!(" {:20} ", e.name), Style::default().fg(Color::White)),
                Span::styled(format!("level {} ", e.level), Style::default().fg(Color::Yellow)),
                Span::styled(&e.uuid, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(app.lang.s().title_ops))
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, list_area, &mut app.ops_state);
    if let Some(da) = detail_area {
        draw_ops_detail(f, da, app);
    }
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

fn draw_ops_detail(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    let block = Block::default().borders(Borders::ALL).title(s.detail_title);
    let lines: Vec<Line> = match app.ops_state.selected().and_then(|i| app.ops.get(i)) {
        None => vec![Line::from(Span::styled(
            s.detail_no_selection,
            Style::default().fg(Color::DarkGray),
        ))],
        Some(e) => {
            let yn = |b: bool| if b { s.detail_yes } else { s.detail_no };
            vec![
                kv_line_bold(&e.name, Color::Cyan),
                Line::raw(""),
                Line::from(Span::styled(
                    format!("{}:", s.detail_uuid),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    e.uuid.clone(),
                    Style::default().fg(Color::White),
                )),
                Line::raw(""),
                kv_line_label(s.detail_level, &e.level.to_string()),
                kv_line_label(s.detail_level_meaning, op_level_meaning(s, e.level)),
                kv_line_label(s.detail_bypass, yn(e.bypasses_player_limit)),
            ]
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
                        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                    ));
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(app.lang.s().title_config),
        )
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, list_area, &mut app.config_state);
    if let Some(da) = detail_area {
        draw_config_detail(f, da, app);
    }
}

fn draw_config_detail(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    let block = Block::default().borders(Borders::ALL).title(s.detail_title);
    let lines: Vec<Line> = match app
        .config_state
        .selected()
        .and_then(|i| app.properties.get(i))
    {
        None => vec![Line::from(Span::styled(
            s.detail_no_selection,
            Style::default().fg(Color::DarkGray),
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
                        Style::default().fg(Color::DarkGray),
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
                            .fg(Color::DarkGray)
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

fn draw_logs(f: &mut Frame, area: Rect, app: &App) {
    let log_path = app.server_dir.join("logs/latest.log");
    let body = if log_path.exists() {
        match fs::read_to_string(&log_path) {
            Ok(s) => {
                let lines: Vec<&str> = s.lines().collect();
                let n = lines.len();
                let take = (area.height as usize).saturating_sub(2).max(1);
                let start = n.saturating_sub(take);
                lines[start..].join("\n")
            }
            Err(e) => fmt_log_read_error(app.lang, &e.to_string()),
        }
    } else {
        app.lang.s().no_logs_yet.to_string()
    };
    let title = format!("{}{} ", app.lang.s().title_logs_prefix, log_path.display());
    let p = Paragraph::new(body)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_yaml(f: &mut Frame, area: Rect, app: &mut App) {
    let s = app.lang.s();
    match &app.yaml_view {
        YamlView::Files => {
            let items: Vec<ListItem> = if app.yaml_files.is_empty() {
                vec![ListItem::new(Line::from(Span::styled(
                    s.yaml_no_files,
                    Style::default().fg(Color::DarkGray),
                )))]
            } else {
                app.yaml_files
                    .iter()
                    .map(|p| {
                        let display = p
                            .strip_prefix(&app.server_dir)
                            .unwrap_or(p)
                            .display()
                            .to_string();
                        ListItem::new(Line::from(Span::styled(
                            format!(" {}", display),
                            Style::default().fg(Color::White),
                        )))
                    })
                    .collect()
            };
            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title(s.title_yaml_files))
                .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
                .highlight_symbol("> ");
            f.render_stateful_widget(list, area, &mut app.yaml_files_state);
        }
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
                                Style::default().fg(Color::DarkGray),
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
                .block(Block::default().borders(Borders::ALL).title(title))
                .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
                .highlight_symbol("> ");
            f.render_stateful_widget(list, area, &mut app.yaml_rows_state);
        }
    }
}

fn draw_backups(f: &mut Frame, area: Rect, app: &mut App) {
    let s = app.lang.s();
    let items: Vec<ListItem> = if app.backups.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            s.backups_none,
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        let now = chrono::Local::now();
        app.backups
            .iter()
            .map(|b| {
                let age = b
                    .modified
                    .map(|t| fmt_age(now - t))
                    .unwrap_or_else(|| "?".into());
                let when = b
                    .modified
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                ListItem::new(Line::from(vec![
                    Span::styled(format!(" {:40}", b.name), Style::default().fg(Color::White)),
                    Span::styled(
                        format!("{:>10}  ", fmt_bytes(b.size_bytes)),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(when, Style::default().fg(Color::DarkGray)),
                    Span::raw("  "),
                    Span::styled(age, Style::default().fg(Color::Yellow)),
                ]))
            })
            .collect()
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(s.title_backups))
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, area, &mut app.backups_state);
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

fn draw_server(f: &mut Frame, area: Rect, app: &mut App) {
    // Vertical split: top = join info (auto-sized to # of interfaces + optional
    // SakuraFrp row, capped), bottom = actions list.
    let nics = detect_interfaces();
    let frp_addr = app.effective_sakurafrp_address();
    let frp_extra: u16 = if frp_addr.is_some() { 1 } else { 0 };
    let join_h = (nics.len() as u16 + frp_extra + 2).max(3).min(13); // border(2) + lines
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(join_h), Constraint::Min(3)])
        .split(area);

    draw_join_info(f, chunks[0], app, &nics);
    draw_server_actions(f, chunks[1], app);
}

fn draw_join_info(f: &mut Frame, area: Rect, app: &mut App, nics: &[NicInfo]) {
    use unicode_width::UnicodeWidthStr;
    let s = app.lang.s();
    let port: u16 = get_property(&app.properties, "server-port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(25565);

    let mut lines: Vec<Line> = if nics.is_empty() {
        vec![Line::from(Span::styled(
            s.join_no_interfaces,
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        nics.iter()
            .map(|n| {
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        format!("{:14}", n.name),
                        Style::default().fg(Color::White),
                    ),
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
                ])
            })
            .collect()
    };

    // Optional SakuraFrp row. The user-set address is the literal string they
    // need to share — already includes port (e.g. `frp-way.com:36192`) because
    // frp tunnels remap the local 25565 to whatever port the provider
    // assigned, not server.properties' server-port. Click → wl-copy. The
    // bracketed kind label also embeds a Docker state marker (●/○/✗/?) so the
    // user can see at a glance whether the launcher container is up.
    if let Some(addr) = app.effective_sakurafrp_address() {
        let (state_marker, state_color) = match app.sakurafrp_docker.state {
            DockerState::Running => ("●", Color::Green),
            DockerState::Stopped => ("○", Color::Yellow),
            DockerState::Missing => ("✗", Color::Red),
            DockerState::Unknown => ("?", Color::DarkGray),
        };

        let name_span = Span::styled(
            format!("{:14}", "frp"),
            Style::default().fg(Color::White),
        );
        let chip_text = addr.clone();
        let addr_span = Span::styled(
            chip_text.clone(),
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        );
        let kind_open = Span::styled("[", Style::default().fg(Color::Magenta));
        let kind_label = Span::styled(s.frp_label, Style::default().fg(Color::Magenta));
        let kind_marker = Span::styled(format!(" {}", state_marker), Style::default().fg(state_color));
        let kind_close = Span::styled("]", Style::default().fg(Color::Magenta));

        // Compute chip rect for click-to-copy. Account for the block's 1-char
        // top border and the leading space + name column.
        let line_y = area.y + 1 + lines.len() as u16;
        let prefix_width = 1u16/*border*/ + 1u16/*leading space*/
            + UnicodeWidthStr::width(name_span.content.as_ref()) as u16
            + 2u16 /*"  "*/;
        let chip_rect = Rect {
            x: area.x + prefix_width,
            y: line_y,
            width: UnicodeWidthStr::width(chip_text.as_str()) as u16,
            height: 1,
        };
        if chip_rect.y < area.y + area.height {
            app.join_chips.push((chip_rect, chip_text));
        }

        lines.push(Line::from(vec![
            Span::raw(" "),
            name_span,
            Span::raw("  "),
            addr_span,
            Span::raw("  "),
            kind_open,
            kind_label,
            kind_marker,
            kind_close,
        ]));
    }

    let p = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(s.join_section_title),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(p, area);
}

fn draw_server_actions(f: &mut Frame, area: Rect, app: &mut App) {
    let s = app.lang.s();
    let items: Vec<ListItem> = SERVER_ACTIONS
        .iter()
        .map(|a| {
            ListItem::new(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    server_action_label(app.lang, *a),
                    Style::default().fg(Color::White),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(s.server_actions_section),
        )
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, area, &mut app.server_state);
    // Note: title_server (s.title_server) is intentionally not rendered as a
    // border title here — Server tab uses two stacked blocks ("Join addresses"
    // + "Actions") and the tab name in the tab bar already conveys context.
    let _ = s.title_server;
}

fn draw_sakurafrp(f: &mut Frame, area: Rect, app: &mut App) {
    let s = app.lang.s();

    // Top: 4-line "User" panel (border + 2 content lines + slack).
    // Middle: tunnel list, takes whatever's left.
    // Bottom: actions hint (3 lines, last token-state line included).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(area);

    draw_sakurafrp_user(f, chunks[0], app);
    draw_sakurafrp_tunnels(f, chunks[1], app);
    draw_sakurafrp_actions_hint(f, chunks[2], app);
    let _ = s; // referenced through nested fns
}

fn draw_sakurafrp_user(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    let lines: Vec<Line> = if app.natfrp_token.is_none() {
        vec![Line::from(Span::styled(
            s.sf_user_no_token,
            Style::default().fg(Color::DarkGray),
        ))]
    } else if let Some(u) = &app.natfrp_user {
        let token_disp = app
            .natfrp_token
            .as_deref()
            .map(crate::natfrp::redact_token)
            .unwrap_or_default();
        let traffic_line = if u.traffic.len() == 2 {
            format!(
                "{}: {} / {}  ({})",
                s.sf_user_traffic_label,
                crate::natfrp::fmt_bytes(u.traffic[0]),
                crate::natfrp::fmt_bytes(u.traffic[1]),
                u.speed,
            )
        } else {
            format!("{}: ({})", s.sf_user_traffic_label, u.speed)
        };
        vec![
            Line::from(vec![
                Span::styled(
                    format!(" {} ", u.name),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{}: {}", s.sf_user_token_label, token_disp),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw("   "),
                Span::styled(
                    format!("{}: {}", s.sf_user_plan_label, u.group.name),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::raw(" "),
                Span::styled(traffic_line, Style::default().fg(Color::White)),
            ]),
        ]
    } else if let Some(err) = &app.natfrp_last_error {
        vec![Line::from(Span::styled(
            format!(" ✗ {}", err),
            Style::default().fg(Color::Red),
        ))]
    } else {
        vec![Line::from(Span::styled(
            s.sf_user_loading,
            Style::default().fg(Color::DarkGray),
        ))]
    };

    let p = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(s.title_sakurafrp_user),
    );
    f.render_widget(p, area);
}

fn draw_sakurafrp_tunnels(f: &mut Frame, area: Rect, app: &mut App) {
    let s = app.lang.s();
    if app.natfrp_token.is_none() || (app.natfrp_tunnels.is_empty() && !app.natfrp_loaded) {
        let msg = if app.natfrp_token.is_none() {
            s.sf_user_no_token
        } else {
            s.sf_tunnels_loading
        };
        let p = Paragraph::new(Line::from(Span::styled(
            msg,
            Style::default().fg(Color::DarkGray),
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(s.title_sakurafrp_tunnels),
        );
        f.render_widget(p, area);
        return;
    }
    if app.natfrp_tunnels.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            s.sf_tunnels_none,
            Style::default().fg(Color::DarkGray),
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(s.title_sakurafrp_tunnels),
        );
        f.render_widget(p, area);
        return;
    }

    // Column widths chosen for the common case (10-char id, 16-char tunnel
    // name, 28-char node label, 4-char type, public address rest). On narrow
    // terminals ratatui will truncate naturally.
    let header = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{:<10}", s.sf_col_id),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:<18}", s.sf_col_name),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:<28}", s.sf_col_node),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:<5}", s.sf_col_type),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(" "),
        Span::styled(s.sf_col_address, Style::default().fg(Color::DarkGray)),
    ]);

    let nodes = &app.natfrp_nodes;
    let items: Vec<ListItem> = std::iter::once(ListItem::new(header))
        .chain(app.natfrp_tunnels.iter().map(|t| {
            let node_label = crate::natfrp::node_label(t.node, nodes);
            let addr = crate::natfrp::public_address(t, nodes).unwrap_or_else(|| "—".to_string());
            let online_marker = if t.online { "●" } else { "○" };
            let online_color = if t.online { Color::Green } else { Color::DarkGray };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{} ", online_marker),
                    Style::default().fg(online_color),
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
                .borders(Borders::ALL)
                .title(s.title_sakurafrp_tunnels),
        )
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("");
    f.render_stateful_widget(list, area, &mut shifted);
}

fn draw_sakurafrp_actions_hint(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    let mut spans: Vec<Span> = vec![
        Span::raw(" "),
        Span::styled(s.sf_action_refresh, Style::default().fg(Color::White)),
        Span::raw(" (r)   "),
        Span::styled(s.sf_action_set_token, Style::default().fg(Color::White)),
        Span::raw(" (t)   "),
        Span::styled(
            s.sf_action_copy_address,
            Style::default().fg(Color::White),
        ),
        Span::raw(" (Enter)"),
    ];
    if let Some(err) = &app.natfrp_last_error {
        spans.push(Span::raw("   "));
        spans.push(Span::styled(
            format!("✗ {}", err),
            Style::default().fg(Color::Red),
        ));
    }
    let p = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(s.title_sakurafrp_actions),
    );
    f.render_widget(p, area);
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
    let hint = hint_for(app.lang, app.tab, &app.yaml_view);
    let line = Line::from(vec![
        Span::styled(format!(" {} ", hint), Style::default().fg(Color::DarkGray)),
        Span::raw("  │  "),
        Span::styled(&app.status, Style::default().fg(Color::Yellow)),
    ]);
    let p = Paragraph::new(line).block(Block::default().borders(Borders::ALL));
    f.render_widget(p, area);
}

fn draw_prompt(f: &mut Frame, prompt: &InputPrompt, lang: Lang) {
    let area = centered_rect(60, 5, f.area());
    f.render_widget(ratatui::widgets::Clear, area);
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
            Style::default().fg(Color::DarkGray),
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
