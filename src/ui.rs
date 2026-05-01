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
        TabId::Players => draw_players(f, chunks[2], app),
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

pub fn op_level_meaning(s: &Strings, level: u8) -> &'static str {
    match level {
        1 => s.detail_op_level_1,
        2 => s.detail_op_level_2,
        3 => s.detail_op_level_3,
        4 => s.detail_op_level_4,
        _ => "?",
    }
}

fn draw_players(f: &mut Frame, area: Rect, app: &mut App) {
    // Top single-line legend showing whitelist on/off + how to toggle.
    // Then list (left) + detail (right) for the rest.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3)])
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
        Color::DarkGray
    };
    let legend = Paragraph::new(Line::from(vec![
        Span::raw(" "),
        Span::styled(legend_text, Style::default().fg(legend_color).add_modifier(Modifier::BOLD)),
    ]));
    f.render_widget(legend, chunks[0]);

    let (list_area, detail_area) = split_list_detail(chunks[1]);
    let wl_enabled = app.whitelist_enabled;

    let items: Vec<ListItem> = if app.players.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            s.players_none,
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        app.players
            .iter()
            .map(|p| player_row(p, wl_enabled, app.lang))
            .collect()
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(s.title_players))
        .highlight_style(Style::default().bg(Color::Blue).add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(list, list_area, &mut app.players_state);
    if let Some(da) = detail_area {
        draw_players_detail(f, da, app);
    }
}

fn player_row(p: &crate::data::PlayerEntry, wl_enabled: bool, lang: Lang) -> ListItem<'static> {
    let s = lang.s();

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
            Span::styled(" ○ ", Style::default().fg(Color::DarkGray))
        }
    } else {
        Span::raw("   ")
    };

    let name_color = if p.historical_only { Color::DarkGray } else { Color::White };
    let name_label = if p.historical_only {
        format!(" {} {} ", p.name, s.players_historical_marker)
    } else {
        format!(" {} ", p.name)
    };

    let mut spans: Vec<Span> = vec![
        wl_span,
        op_span,
        Span::styled(format!("{:24}", truncate_display(&name_label, 24)), Style::default().fg(name_color)),
        Span::styled(
            format!(" {:36} ", &p.uuid),
            Style::default().fg(Color::DarkGray),
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
    let block = Block::default().borders(Borders::ALL).title(s.detail_title);
    let lines: Vec<Line> = match app.players_state.selected().and_then(|i| app.players.get(i)) {
        None => vec![Line::from(Span::styled(
            s.detail_no_selection,
            Style::default().fg(Color::DarkGray),
        ))],
        Some(p) => {
            let yn = |b: bool| if b { s.detail_yes } else { s.detail_no };
            let mut out = vec![
                kv_line_bold(&p.name, Color::Cyan),
                Line::raw(""),
                Line::from(Span::styled(
                    format!("{}:", s.detail_uuid),
                    Style::default().fg(Color::DarkGray),
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
                    .fg(Color::DarkGray)
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

    // v0.12 — variable layout:
    //   * mihomo warning line (0 or 1) — only shown when Sparkle/mihomo is up
    //   * user panel: 5 lines for the 3-step onboarding when tokenless,
    //     4 lines once we have a token (so the row layout matches v0.10).
    //   * tunnel list: takes remaining vertical space.
    //   * actions hint: 3 lines with the new `o` action surfaced.
    let mihomo_h: u16 = if app.mihomo_running { 1 } else { 0 };
    let user_h: u16 = if app.natfrp_token.is_none() { 5 } else { 4 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(mihomo_h),
            Constraint::Length(user_h),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(area);

    if mihomo_h > 0 {
        draw_sakurafrp_mihomo_warning(f, chunks[0], app);
    }
    draw_sakurafrp_user(f, chunks[1], app);
    draw_sakurafrp_tunnels(f, chunks[2], app);
    draw_sakurafrp_actions_hint(f, chunks[3], app);
    let _ = s; // referenced through nested fns
}

/// One-line dim warning when Sparkle/mihomo is running. Doesn't block anything;
/// just primes the user before friends connect.
fn draw_sakurafrp_mihomo_warning(f: &mut Frame, area: Rect, app: &App) {
    let s = app.lang.s();
    let p = Paragraph::new(Line::from(Span::styled(
        format!(" {}", s.sf_mihomo_warning),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::DIM),
    )));
    f.render_widget(p, area);
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

    // No token: tunnels panel mirrors the user-panel onboarding so the user's
    // eye doesn't have to scan two contradicting empty messages.
    if app.natfrp_token.is_none() {
        let p = Paragraph::new(vec![Line::from(Span::styled(
            format!(" {}", s.sf_user_no_token),
            Style::default().fg(Color::DarkGray),
        ))])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(s.title_sakurafrp_tunnels),
        );
        f.render_widget(p, area);
        return;
    }

    // Token set, refresh hasn't fired yet.
    if app.natfrp_tunnels.is_empty() && !app.natfrp_loaded {
        let p = Paragraph::new(Line::from(Span::styled(
            s.sf_tunnels_loading,
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

    // If the API call errored before we got any user info, don't claim "no
    // tunnels" — we genuinely don't know. Show a neutral message and let the
    // error in the user panel above carry the actionable diagnosis.
    if app.natfrp_tunnels.is_empty()
        && app.natfrp_last_error.is_some()
        && app.natfrp_user.is_none()
    {
        let p = Paragraph::new(Line::from(Span::styled(
            format!(" {}", s.sf_tunnels_loading),
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

    // Refresh succeeded, account exists, but the user hasn't created any
    // tunnels yet. v0.12 swaps the curt one-liner for a 3-option fork that
    // forwards-references v0.13's `c` command without depending on it.
    if app.natfrp_tunnels.is_empty() {
        let lines = vec![
            Line::from(Span::styled(
                format!(" {}", s.sf_tunnels_empty_header),
                Style::default().fg(Color::DarkGray),
            )),
            Line::raw(""),
            Line::from(Span::styled(
                s.sf_tunnels_empty_option_v013,
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM),
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
            s.sf_action_open_dashboard,
            Style::default().fg(Color::White),
        ),
        Span::raw(" (o)   "),
        Span::styled(
            s.sf_action_copy_address,
            Style::default().fg(Color::White),
        ),
        Span::raw(" (Enter)"),
    ];
    // Surface the launcher-down hint inline so the user sees it in their
    // primary visual focus area (not buried two tabs over). Errors (if any)
    // already render in the user panel so we don't duplicate them here.
    if app.launcher_hint_applicable() {
        spans.push(Span::raw("   "));
        spans.push(Span::styled(
            s.sf_launcher_hint,
            Style::default().fg(Color::Yellow),
        ));
    }
    let p = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(s.title_sakurafrp_actions),
    );
    f.render_widget(p, area);
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

#[cfg(test)]
mod tests {
    use super::*;

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
