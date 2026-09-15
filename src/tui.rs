//! A keyboard-first terminal trace reader.
//!
//! The TUI deliberately shares the parser and format description with the web
//! viewer. It is a fast way to inspect a run over ssh or from a small terminal:
//! `j/k` move, `h/l` switch panes, `/` searches, and `q` exits.

use crate::collect_jsonl;
use crate::format::FormatSpec;
use crate::trace::{get_path, parse_ts, Run, TraceFile};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event as CEvent, KeyCode, KeyEvent,
    KeyEventKind, KeyModifiers, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde_json::Value;
use std::collections::HashSet;
use std::error::Error;
use std::io;
use std::path::Path;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Runs,
    Events,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchScope {
    Runs,
    Events,
    Detail,
}

const DETAIL_BODY_OFFSET: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
struct RunRef {
    file: usize,
    run: usize,
}

enum RunRow {
    Folder {
        key: String,
        label: String,
        count: usize,
    },
    Run(RunRef),
}

struct App<'a> {
    spec: &'a FormatSpec,
    files: Vec<TraceFile>,
    format_path: &'a Path,
    source_path: &'a Path,
    runs: Vec<RunRef>,
    run_index: usize,
    event_index: usize,
    focus: Focus,
    detail_scroll: u16,
    detail_expanded: bool,
    raw_detail: bool,
    sidebar_collapsed: bool,
    query: String,
    search_input: Option<String>,
    search_scope: Option<SearchScope>,
    detail_match_line: Option<usize>,
    command_input: Option<String>,
    show_help: bool,
    pending_g: bool,
    collapsed_folders: HashSet<String>,
    notice: Option<String>,
}

impl<'a> App<'a> {
    fn new(
        spec: &'a FormatSpec,
        files: Vec<TraceFile>,
        format_path: &'a Path,
        source_path: &'a Path,
    ) -> Self {
        let runs = run_refs(&files);
        Self {
            spec,
            files,
            format_path,
            source_path,
            runs,
            run_index: 0,
            event_index: 0,
            focus: Focus::Events,
            detail_scroll: 0,
            detail_expanded: false,
            raw_detail: false,
            sidebar_collapsed: false,
            query: String::new(),
            search_input: None,
            search_scope: None,
            detail_match_line: None,
            command_input: None,
            show_help: false,
            pending_g: false,
            collapsed_folders: HashSet::new(),
            notice: None,
        }
    }

    fn rebuild_run_refs(&mut self) {
        self.runs = run_refs(&self.files);
    }

    fn refresh(&mut self) {
        let old_path = self.current_file().map(|file| file.path.clone());
        let old_run_id = self.current_run().map(|run| run.id.clone());
        let old_event_index = self.event_index;
        let paths = match collect_jsonl(self.source_path) {
            Ok(paths) => paths,
            Err(error) => {
                self.notice = Some(format!("refresh failed: {error}"));
                return;
            }
        };
        let mut refreshed = Vec::new();
        let mut skipped = 0usize;
        for path in paths {
            match crate::trace::load_file(&path.to_string_lossy(), refreshed.len(), self.spec) {
                Ok(mut file) => {
                    file.runs.retain(|run| run.stats.events > 0);
                    if file.runs.is_empty() {
                        skipped += 1;
                    } else {
                        refreshed.push(file);
                    }
                }
                Err(_) => skipped += 1,
            }
        }
        if refreshed.is_empty() {
            self.notice =
                Some("refresh found no parseable trace events; keeping current view".into());
            return;
        }

        self.files = refreshed;
        self.rebuild_run_refs();
        let restored = old_path.as_deref().and_then(|path| {
            old_run_id.as_deref().and_then(|run_id| {
                self.runs.iter().position(|reference| {
                    self.files[reference.file].path == path
                        && self.files[reference.file].runs[reference.run].id == run_id
                })
            })
        });
        self.run_index = restored.unwrap_or(0).min(self.runs.len().saturating_sub(1));
        self.event_index = self
            .current_run()
            .map(|run| old_event_index.min(run.events.len().saturating_sub(1)))
            .unwrap_or(0);
        self.detail_scroll = 0;
        self.detail_match_line = None;
        let folder_keys: HashSet<String> = self.files.iter().map(folder_key).collect();
        self.collapsed_folders
            .retain(|key| folder_keys.contains(key));
        self.notice = Some(if skipped == 0 {
            format!("refreshed {} trace file(s)", self.files.len())
        } else {
            format!(
                "refreshed {} trace file(s); skipped {}",
                self.files.len(),
                skipped
            )
        });
    }

    fn folder_label(&self, file: &TraceFile) -> String {
        let parent = Path::new(&file.path).parent();
        if let Some(parent) = parent {
            if let Ok(relative) = parent.strip_prefix(self.source_path) {
                if !relative.as_os_str().is_empty() {
                    return relative.display().to_string();
                }
                if let Some(name) = parent.file_name() {
                    return name.to_string_lossy().into_owned();
                }
            }
        }
        if file.dir.is_empty() {
            ".".into()
        } else {
            file.dir.clone()
        }
    }

    fn current_folder_key(&self) -> Option<String> {
        self.current_file().map(folder_key)
    }

    fn current_folder_collapsed(&self) -> bool {
        self.current_folder_key()
            .map(|key| self.collapsed_folders.contains(&key))
            .unwrap_or(false)
    }

    fn toggle_current_folder(&mut self) {
        if let Some(key) = self.current_folder_key() {
            if !self.collapsed_folders.remove(&key) {
                self.collapsed_folders.insert(key);
            }
        }
    }

    fn visible_run_indices(&self) -> Vec<usize> {
        self.runs
            .iter()
            .enumerate()
            .filter(|(_, reference)| {
                !self
                    .collapsed_folders
                    .contains(&folder_key(&self.files[reference.file]))
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn run_rows(&self) -> Vec<RunRow> {
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        for reference in &self.runs {
            let file = &self.files[reference.file];
            let key = folder_key(file);
            if seen.insert(key.clone()) {
                let count = self
                    .runs
                    .iter()
                    .filter(|candidate| folder_key(&self.files[candidate.file]) == key)
                    .count();
                rows.push(RunRow::Folder {
                    key: key.clone(),
                    label: self.folder_label(file),
                    count,
                });
            }
            if !self.collapsed_folders.contains(&key) {
                rows.push(RunRow::Run(*reference));
            }
        }
        rows
    }

    fn selected_run_row(&self, rows: &[RunRow]) -> Option<usize> {
        let current_folder = self.current_folder_key();
        rows.iter().position(|row| match row {
            RunRow::Run(reference) => *reference == self.runs[self.run_index],
            RunRow::Folder { key, .. } => {
                self.current_folder_collapsed() && current_folder.as_deref() == Some(key)
            }
        })
    }

    fn current_run(&self) -> Option<&Run> {
        let reference = self.runs.get(self.run_index)?;
        self.files.get(reference.file)?.runs.get(reference.run)
    }

    fn current_file(&self) -> Option<&TraceFile> {
        let reference = self.runs.get(self.run_index)?;
        self.files.get(reference.file)
    }

    fn select_run(&mut self, index: usize) {
        if self.runs.is_empty() {
            return;
        }
        self.run_index = index.min(self.runs.len() - 1);
        self.event_index = 0;
        self.detail_scroll = 0;
        self.detail_match_line = None;
        self.detail_expanded = false;
    }

    fn move_run(&mut self, delta: isize) {
        let visible = self.visible_run_indices();
        if visible.is_empty() {
            return;
        }
        let position = visible
            .iter()
            .position(|index| *index == self.run_index)
            .unwrap_or(if delta < 0 { visible.len() - 1 } else { 0 });
        let next =
            (position as isize + delta).clamp(0, visible.len().saturating_sub(1) as isize) as usize;
        self.select_run(visible[next]);
    }

    fn first_visible_run(&mut self) {
        if let Some(index) = self.visible_run_indices().first().copied() {
            self.select_run(index);
        }
    }

    fn last_visible_run(&mut self) {
        if let Some(index) = self.visible_run_indices().last().copied() {
            self.select_run(index);
        }
    }

    fn move_event(&mut self, delta: isize) {
        let Some(run) = self.current_run() else {
            return;
        };
        if run.events.is_empty() {
            return;
        }
        let next = (self.event_index as isize + delta)
            .clamp(0, run.events.len().saturating_sub(1) as isize) as usize;
        self.event_index = next;
        self.detail_scroll = 0;
        self.detail_match_line = None;
    }

    fn move_event_page(&mut self, direction: isize) {
        let page = 10;
        self.move_event(direction * page);
    }

    fn scroll_detail(&mut self, delta: i32) {
        if delta < 0 {
            self.detail_scroll = self.detail_scroll.saturating_sub((-delta) as u16);
        } else {
            self.detail_scroll = self.detail_scroll.saturating_add(delta as u16);
        }
    }

    fn begin_search(&mut self) {
        self.search_scope = Some(if self.detail_expanded {
            SearchScope::Detail
        } else {
            match self.focus {
                Focus::Runs => SearchScope::Runs,
                Focus::Events => SearchScope::Events,
            }
        });
        self.search_input = Some(self.query.clone());
    }

    fn finish_search(&mut self) {
        let Some(input) = self.search_input.take() else {
            return;
        };
        self.query = input;
        match self.search_scope.unwrap_or(SearchScope::Events) {
            SearchScope::Runs => {
                if let Some(index) = self.find_run_match(self.run_index, 1) {
                    self.select_run(index);
                }
            }
            SearchScope::Events => {
                if let Some(index) = self.find_event_match(self.event_index, 1) {
                    self.event_index = index;
                    self.detail_scroll = 0;
                    self.detail_match_line = None;
                }
            }
            SearchScope::Detail => {
                let from = self
                    .detail_match_line
                    .unwrap_or(self.detail_scroll as usize)
                    .saturating_sub(1);
                if let Some(line) = self.find_detail_match(from, 1) {
                    self.detail_match_line = Some(line);
                    self.detail_scroll = line
                        .saturating_add(DETAIL_BODY_OFFSET)
                        .min(u16::MAX as usize) as u16;
                }
            }
        }
    }

    fn finish_command(&mut self) {
        let Some(input) = self.command_input.take() else {
            return;
        };
        let Ok(line) = input.trim().parse::<usize>() else {
            return;
        };

        // In an expanded detail pane, :N refers to the displayed content line
        // number. Outside it, it keeps the original event-jump behaviour.
        if self.detail_expanded {
            if let Some(body_line) = self.find_detail_line_number(line) {
                self.detail_scroll = body_line
                    .saturating_add(DETAIL_BODY_OFFSET)
                    .min(u16::MAX as usize) as u16;
                self.detail_match_line = None;
                return;
            }
            return;
        }

        let Some(run) = self.current_run() else {
            return;
        };
        if (1..=run.events.len()).contains(&line) {
            self.event_index = line - 1;
            self.focus = Focus::Events;
            self.detail_scroll = 0;
            self.detail_match_line = None;
        }
    }

    fn find_detail_line_number(&self, line_number: usize) -> Option<usize> {
        if line_number == 0 {
            return None;
        }
        let lines = self.detail_lines_for_search()?;
        let marker = format!("{line_number:>4} │ ");
        lines.iter().position(|line| {
            line.spans
                .first()
                .map(|span| span.content.as_ref() == marker)
                .unwrap_or(false)
        })
    }

    fn find_event_match(&self, from: usize, direction: isize) -> Option<usize> {
        let run = self.current_run()?;
        if self.query.trim().is_empty() || run.events.is_empty() {
            return None;
        }
        let len = run.events.len() as isize;
        let query = self.query.to_lowercase();
        let mut index = from as isize;
        for _ in 0..run.events.len() {
            index = (index + direction).rem_euclid(len);
            let event = &run.events[index as usize];
            let ty = event_type(self.spec, event);
            let title = event_title(self.spec, event, &ty);
            let text = format!(
                "{} {} {}",
                ty,
                title,
                serde_json::to_string(event).unwrap_or_default()
            );
            if text.to_lowercase().contains(&query) {
                return Some(index as usize);
            }
        }
        None
    }

    fn find_run_match(&self, from: usize, direction: isize) -> Option<usize> {
        if self.query.trim().is_empty() || self.runs.is_empty() {
            return None;
        }
        let len = self.runs.len() as isize;
        let query = self.query.to_lowercase();
        let mut index = from as isize;
        for _ in 0..self.runs.len() {
            index = (index + direction).rem_euclid(len);
            let reference = self.runs[index as usize];
            let file = &self.files[reference.file];
            let run = &file.runs[reference.run];
            let text = format!(
                "{} {} {} {} {}",
                file.name,
                file.path,
                run.id,
                run.label,
                run_summary(&run.stats)
            );
            if text.to_lowercase().contains(&query) {
                return Some(index as usize);
            }
        }
        None
    }

    fn detail_lines_for_search(&self) -> Option<Vec<Line<'static>>> {
        let run = self.current_run()?;
        let event = run.events.get(self.event_index)?;
        let ty = event_type(self.spec, event);
        Some(detail_body_lines(self.spec, event, &ty, self.raw_detail))
    }

    fn find_detail_match(&self, from: usize, direction: isize) -> Option<usize> {
        let lines = self.detail_lines_for_search()?;
        if self.query.trim().is_empty() || lines.is_empty() {
            return None;
        }
        let len = lines.len() as isize;
        let query = self.query.to_lowercase();
        let mut index = from as isize;
        for _ in 0..lines.len() {
            index = (index + direction).rem_euclid(len);
            let text: String = lines[index as usize]
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            if text.to_lowercase().contains(&query) {
                return Some(index as usize);
            }
        }
        None
    }

    fn next_match(&mut self, direction: isize) {
        match self.search_scope.unwrap_or(SearchScope::Events) {
            SearchScope::Runs => {
                if let Some(index) = self.find_run_match(self.run_index, direction) {
                    self.select_run(index);
                }
            }
            SearchScope::Events => {
                if let Some(index) = self.find_event_match(self.event_index, direction) {
                    self.event_index = index;
                    self.detail_scroll = 0;
                    self.detail_match_line = None;
                }
            }
            SearchScope::Detail => {
                let from = self
                    .detail_match_line
                    .unwrap_or(self.detail_scroll as usize);
                if let Some(line) = self.find_detail_match(from, direction) {
                    self.detail_match_line = Some(line);
                    self.detail_scroll = line
                        .saturating_add(DETAIL_BODY_OFFSET)
                        .min(u16::MAX as usize) as u16;
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind != KeyEventKind::Press {
            return false;
        }

        if let Some(input) = self.search_input.as_mut() {
            match key.code {
                KeyCode::Esc => self.search_input = None,
                KeyCode::Enter => self.finish_search(),
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.push(c);
                }
                _ => {}
            }
            return false;
        }

        if let Some(input) = self.command_input.as_mut() {
            match key.code {
                KeyCode::Esc => self.command_input = None,
                KeyCode::Enter => self.finish_command(),
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    input.push(c);
                }
                _ => {}
            }
            return false;
        }

        if self.show_help {
            if matches!(
                key.code,
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
            ) {
                self.show_help = false;
            }
            return false;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => return true,
                KeyCode::Char('d') => {
                    if self.detail_expanded {
                        self.scroll_detail(12);
                    } else if self.focus == Focus::Events {
                        self.move_event_page(1);
                    } else {
                        self.move_run(10);
                    }
                }
                KeyCode::Char('[') => self.move_run(-1),
                KeyCode::Char(']') => self.move_run(1),
                KeyCode::Char('u') => {
                    if self.detail_expanded {
                        self.scroll_detail(-12);
                    } else if self.focus == Focus::Events {
                        self.move_event_page(-1);
                    } else {
                        self.move_run(-10);
                    }
                }
                KeyCode::Char('f') => self.scroll_detail(12),
                KeyCode::Char('b') => self.scroll_detail(-12),
                _ => {}
            }
            return false;
        }

        if self.pending_g {
            self.pending_g = false;
            if key.code == KeyCode::Char('g') {
                match self.focus {
                    Focus::Runs => self.first_visible_run(),
                    Focus::Events => self.move_event(isize::MIN),
                }
                return false;
            }
        }

        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char('b') => {
                self.sidebar_collapsed = !self.sidebar_collapsed;
                if self.sidebar_collapsed {
                    self.focus = Focus::Events;
                }
            }
            KeyCode::Char('c') if self.focus == Focus::Runs => self.toggle_current_folder(),
            KeyCode::Char('C') if self.focus == Focus::Runs => self.collapsed_folders.clear(),
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('R') => {
                self.raw_detail = !self.raw_detail;
                self.detail_scroll = 0;
                self.detail_match_line = None;
            }
            KeyCode::Esc => {
                if self.detail_expanded {
                    self.detail_expanded = false;
                } else {
                    self.query.clear();
                    self.search_scope = None;
                    self.detail_match_line = None;
                }
            }
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('/') => self.begin_search(),
            KeyCode::Char(':') => self.command_input = Some(String::new()),
            KeyCode::Char('n') if !self.query.is_empty() => self.next_match(1),
            KeyCode::Char('N') if !self.query.is_empty() => self.next_match(-1),
            KeyCode::Tab => {
                if self.detail_expanded || self.focus == Focus::Events {
                    // Formatted detail and raw JSON are alternate tabs, not
                    // two copies rendered at once.
                    self.raw_detail = !self.raw_detail;
                    self.detail_scroll = 0;
                    self.detail_match_line = None;
                } else {
                    self.focus = Focus::Events;
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if self.sidebar_collapsed {
                    self.sidebar_collapsed = false;
                }
                self.focus = Focus::Runs;
            }
            KeyCode::Char('l') | KeyCode::Right => self.focus = Focus::Events,
            KeyCode::Char('j') | KeyCode::Down => {
                if self.detail_expanded && self.focus == Focus::Events {
                    self.scroll_detail(1);
                } else {
                    match self.focus {
                        Focus::Runs => self.move_run(1),
                        Focus::Events => self.move_event(1),
                    }
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.detail_expanded && self.focus == Focus::Events {
                    self.scroll_detail(-1);
                } else {
                    match self.focus {
                        Focus::Runs => self.move_run(-1),
                        Focus::Events => self.move_event(-1),
                    }
                }
            }
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::Char('G') | KeyCode::End => match self.focus {
                Focus::Runs => self.last_visible_run(),
                Focus::Events => {
                    if let Some(run) = self.current_run() {
                        self.event_index = run.events.len().saturating_sub(1);
                        self.detail_scroll = 0;
                    }
                }
            },
            KeyCode::Home => match self.focus {
                Focus::Runs => self.first_visible_run(),
                Focus::Events => self.move_event(isize::MIN),
            },
            KeyCode::PageDown => {
                if self.detail_expanded {
                    self.scroll_detail(16);
                } else if self.focus == Focus::Events {
                    self.move_event_page(1);
                } else {
                    self.move_run(10);
                }
            }
            KeyCode::PageUp => {
                if self.detail_expanded {
                    self.scroll_detail(-16);
                } else if self.focus == Focus::Events {
                    self.move_event_page(-1);
                } else {
                    self.move_run(-10);
                }
            }
            KeyCode::Enter => {
                if self.focus == Focus::Runs {
                    if self.current_folder_collapsed() {
                        self.toggle_current_folder();
                    } else {
                        self.focus = Focus::Events;
                    }
                } else {
                    self.detail_expanded = !self.detail_expanded;
                    self.detail_scroll = 0;
                }
            }
            _ => {}
        }
        false
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);
        self.draw_header(frame, outer[0]);
        self.draw_body(frame, outer[1]);
        self.draw_status(frame, outer[2]);
        if self.show_help {
            self.draw_help(frame, area);
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let file_name = self
            .current_file()
            .map(|f| f.name.as_str())
            .unwrap_or("no trace");
        let run = self.current_run();
        let run_name = run.map(|r| r.id.as_str()).unwrap_or("-");
        let summary = run
            .map(|r| run_summary(&r.stats))
            .unwrap_or_else(|| "no events".into());
        let title = Line::from(vec![
            Span::styled(" traciium ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled("/ ", Style::default().fg(Color::DarkGray)),
            Span::styled(file_name.to_string(), Style::default().fg(Color::Cyan)),
            Span::styled(" / ", Style::default().fg(Color::DarkGray)),
            Span::styled(run_name.to_string(), Style::default().fg(Color::White)),
            Span::styled("  ", Style::default()),
            Span::styled(summary, Style::default().fg(Color::DarkGray)),
        ]);
        let subtitle = Line::from(vec![
            Span::styled(" format ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                self.format_path.display().to_string(),
                Style::default().fg(Color::Gray),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(Text::from(vec![title, subtitle]))
                .block(Block::default().borders(Borders::BOTTOM)),
            area,
        );
    }

    fn draw_body(&mut self, frame: &mut Frame, area: Rect) {
        let content_area = if self.sidebar_collapsed {
            area
        } else {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(34), Constraint::Min(0)])
                .split(area);
            self.draw_runs(frame, columns[0]);
            columns[1]
        };

        if self.detail_expanded {
            self.draw_details(frame, content_area, true);
            return;
        }

        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(52), Constraint::Min(0)])
            .split(content_area);
        self.draw_events(frame, right[0]);
        self.draw_details(frame, right[1], false);
    }

    fn draw_runs(&self, frame: &mut Frame, area: Rect) {
        let rows = self.run_rows();
        let selected = self.selected_run_row(&rows);
        let items: Vec<ListItem> = rows
            .iter()
            .map(|row| match row {
                RunRow::Folder { key, label, count } => {
                    let marker = if self.collapsed_folders.contains(key) {
                        "›"
                    } else {
                        "⌄"
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!(" {marker} "), Style::default().fg(Color::Cyan)),
                        Span::styled(label.clone(), Style::default().fg(Color::White)),
                        Span::styled(
                            format!("  {count} run{}", if *count == 1 { "" } else { "s" }),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]))
                }
                RunRow::Run(reference) => {
                    let file = &self.files[reference.file];
                    let run = &file.runs[reference.run];
                    let outcome = match run.stats.success {
                        Some(true) => ("PASS", Color::Green),
                        Some(false) => ("FAIL", Color::Red),
                        None => ("----", Color::DarkGray),
                    };
                    let first = Line::from(vec![
                        Span::raw("   "),
                        Span::styled(run.label.clone(), Style::default().fg(Color::White)),
                    ]);
                    let second = Line::from(vec![
                        Span::raw("      "),
                        Span::styled(file.name.clone(), Style::default().fg(Color::DarkGray)),
                        Span::raw("  "),
                        Span::styled(outcome.0, Style::default().fg(outcome.1)),
                        Span::styled(
                            format!(
                                "  {}  {} events",
                                fmt_duration(run.stats.duration_s),
                                run.stats.events
                            ),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]);
                    ListItem::new(Text::from(vec![first, second]))
                }
            })
            .collect();
        let mut state = ListState::default();
        state.select(selected);
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::RIGHT)
                    .title(" Runs / folders "),
            )
            .highlight_style(Style::default().bg(Color::Rgb(31, 38, 45)))
            .highlight_symbol("");
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn draw_events(&self, frame: &mut Frame, area: Rect) {
        let Some(run) = self.current_run() else {
            frame.render_widget(
                Block::default().borders(Borders::LEFT).title(" Events "),
                area,
            );
            return;
        };
        let items: Vec<ListItem> = run
            .events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                let ty = event_type(self.spec, event);
                let tone = event_tone(self.spec, event, &ty);
                let label = event_label(self.spec, event, &ty);
                let title = event_title(self.spec, event, &ty);
                let relative = parse_ts(get_path(event, &self.spec.trace.timestamp_field))
                    .map(|ts| fmt_relative(ts, run.t_min))
                    .unwrap_or_default();
                let mut spans = vec![
                    Span::styled(
                        format!("{:>4} ", index + 1),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(format!("{:<16} ", truncate(&label, 16)), tone_style(&tone)),
                ];
                if !title.is_empty() {
                    spans.push(Span::styled(
                        truncate(&title, 64),
                        Style::default().fg(Color::White),
                    ));
                }
                spans.push(Span::styled(
                    format!("  {:>8}", relative),
                    Style::default().fg(Color::DarkGray),
                ));
                ListItem::new(Line::from(spans))
            })
            .collect();
        let mut state = ListState::default();
        state.select((!run.events.is_empty()).then_some(self.event_index));
        let title = format!(" Events  {} ", run.events.len());
        let list = List::new(items)
            .block(Block::default().borders(Borders::LEFT).title(title))
            .highlight_style(Style::default().bg(Color::Rgb(31, 38, 45)))
            .highlight_symbol("› ");
        frame.render_stateful_widget(list, area, &mut state);
    }

    fn draw_details(&self, frame: &mut Frame, area: Rect, expanded: bool) {
        let Some(run) = self.current_run() else {
            frame.render_widget(
                Block::default().title(" Detail ").borders(Borders::ALL),
                area,
            );
            return;
        };
        let Some(event) = run.events.get(self.event_index) else {
            frame.render_widget(
                Block::default().title(" Detail ").borders(Borders::ALL),
                area,
            );
            return;
        };
        let ty = event_type(self.spec, event);
        let tone = event_tone(self.spec, event, &ty);
        let label = event_label(self.spec, event, &ty);
        let title = event_title(self.spec, event, &ty);
        let ts = parse_ts(get_path(event, &self.spec.trace.timestamp_field));
        let seq = self
            .spec
            .trace
            .sequence_field
            .as_deref()
            .and_then(|field| get_path(event, field))
            .map(display_value)
            .unwrap_or_else(|| "-".into());

        let mut lines = vec![Line::from(vec![
            Span::styled(label, tone_style(&tone).add_modifier(Modifier::BOLD)),
            Span::styled(
                if title.is_empty() {
                    "".into()
                } else {
                    format!("  {title}")
                },
                Style::default().fg(Color::White),
            ),
        ])];
        lines.push(Line::from(vec![
            Span::styled("type ", Style::default().fg(Color::DarkGray)),
            Span::raw(ty.clone()),
            Span::styled("   seq ", Style::default().fg(Color::DarkGray)),
            Span::raw(seq),
            Span::styled("   time ", Style::default().fg(Color::DarkGray)),
            Span::raw(ts.map(fmt_absolute).unwrap_or_else(|| "-".into())),
        ]));
        lines.push(Line::from(""));
        lines.extend(detail_body_lines(self.spec, event, &ty, self.raw_detail));

        let title = if expanded {
            if self.raw_detail {
                " JSON · raw  (Tab/R formatted, :N line, Esc return) "
            } else {
                " Detail · formatted  (Tab/R JSON, :N line, Esc return) "
            }
        } else if self.raw_detail {
            " JSON · raw  (Tab/R formatted, Enter expand) "
        } else {
            " Detail · formatted  (Enter expand, Tab/R JSON) "
        };
        let paragraph = Paragraph::new(Text::from(lines))
            .block(Block::default().borders(Borders::ALL).title(title))
            .scroll((self.detail_scroll, 0))
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let text = if let Some(input) = &self.search_input {
            let scope = match self.search_scope.unwrap_or(SearchScope::Events) {
                SearchScope::Runs => "runs",
                SearchScope::Events => "events",
                SearchScope::Detail => "detail",
            };
            format!(" /{}▌  search {scope} · Enter apply · Esc cancel", input)
        } else if let Some(input) = &self.command_input {
            let target = if self.detail_expanded {
                "detail line"
            } else {
                "event"
            };
            format!(" :{}▌  Enter jump to {target} · Esc cancel", input)
        } else {
            let focus = match self.focus {
                Focus::Runs => "runs",
                Focus::Events => "events",
            };
            let count = self
                .current_run()
                .map(|run| {
                    format!(
                        "{}/{}",
                        self.event_index.saturating_add(1),
                        run.events.len()
                    )
                })
                .unwrap_or_else(|| "0/0".into());
            let query = if self.query.is_empty() {
                String::new()
            } else {
                let scope = match self.search_scope.unwrap_or(SearchScope::Events) {
                    SearchScope::Runs => "runs",
                    SearchScope::Events => "events",
                    SearchScope::Detail => "detail",
                };
                format!("  search {scope}: {}", self.query)
            };
            let notice = self
                .notice
                .as_deref()
                .map(|message| format!("  [{message}]"))
                .unwrap_or_default();
            format!(
                " {}  focus:{}  event:{}  sidebar:{}{}{}   j/k move · b sidebar · / search · ? help · q quit",
                if self.pending_g { "g_" } else { "normal" },
                focus,
                count,
                if self.sidebar_collapsed { "closed" } else { "open" },
                query,
                notice
            )
        };
        frame.render_widget(
            Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
            area,
        );
    }

    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        let popup = centered_rect(72, 72, area);
        let lines = vec![
            Line::from(Span::styled(
                "traciium / keyboard help",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("j / k       next / previous run or scroll expanded detail"),
            Line::from("h / l       focus runs / events"),
            Line::from("Ctrl-[ / ]  previous / next run"),
            Line::from("Enter       choose run; expand event detail"),
            Line::from("b           collapse / restore the Runs pane"),
            Line::from("c / C       collapse current folder / expand all"),
            Line::from("r           refresh traces"),
            Line::from("Tab         switch formatted / JSON detail (from events)"),
            Line::from("R           toggle formatted / JSON detail"),
            Line::from("g g / G     first / last item"),
            Line::from("Ctrl-d/u    half-page down / up"),
            Line::from("PageDown/Up page or detail scroll; mouse wheel works too"),
            Line::from("/           search the focused pane"),
            Line::from(":N          jump to detail line N, or event N normally"),
            Line::from("n / N       next / previous search match"),
            Line::from("Esc         close detail/search"),
            Line::from("q           quit"),
            Line::from(""),
            Line::from(Span::styled(
                "Press ? or Esc to close",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .block(Block::default().borders(Borders::ALL).title(" Help "))
                .style(Style::default().bg(Color::Rgb(20, 24, 28))),
            popup,
        );
    }
}

/// Start the terminal UI. The alternate screen is always restored before the
/// result is returned, including when the event loop encounters an error.
pub fn run(
    spec: &FormatSpec,
    files: Vec<TraceFile>,
    format_path: &Path,
    source_path: &Path,
) -> Result<(), Box<dyn Error>> {
    let mut app = App::new(spec, files, format_path, source_path);
    if app.runs.is_empty() {
        return Err("no runs to display".into());
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = loop {
        terminal.draw(|frame| app.draw(frame))?;
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                CEvent::Key(key) => {
                    if app.handle_key(key) {
                        break Ok(());
                    }
                }
                CEvent::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::ScrollDown => {
                        if app.detail_expanded {
                            app.scroll_detail(4);
                        } else if app.focus == Focus::Events {
                            app.move_event(4);
                        } else {
                            app.move_run(4);
                        }
                    }
                    MouseEventKind::ScrollUp => {
                        if app.detail_expanded {
                            app.scroll_detail(-4);
                        } else if app.focus == Focus::Events {
                            app.move_event(-4);
                        } else {
                            app.move_run(-4);
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    };

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    result
}

fn run_refs(files: &[TraceFile]) -> Vec<RunRef> {
    files
        .iter()
        .enumerate()
        .flat_map(|(file, trace)| (0..trace.runs.len()).map(move |run| RunRef { file, run }))
        .collect()
}

fn folder_key(file: &TraceFile) -> String {
    Path::new(&file.path)
        .parent()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".into())
}

fn event_type(spec: &FormatSpec, event: &Value) -> String {
    get_path(event, &spec.trace.type_field)
        .and_then(Value::as_str)
        .unwrap_or("event")
        .to_string()
}

fn event_label(spec: &FormatSpec, event: &Value, ty: &str) -> String {
    let mut label = spec.label(ty);
    for conditional in &spec.conditional_styles {
        if conditional
            .when
            .iter()
            .all(|(key, expected)| get_path(event, key) == Some(expected))
        {
            if let Some(prefix) = &conditional.style.label_prefix {
                label = format!("{prefix}{label}");
            }
            break;
        }
    }
    label
}

fn event_tone(spec: &FormatSpec, event: &Value, ty: &str) -> String {
    for conditional in &spec.conditional_styles {
        if conditional
            .when
            .iter()
            .all(|(key, expected)| get_path(event, key) == Some(expected))
        {
            if let Some(tone) = &conditional.style.tone {
                return tone.clone();
            }
            break;
        }
    }
    spec.tone(ty)
}

fn event_title(spec: &FormatSpec, event: &Value, ty: &str) -> String {
    let field = spec.event(ty).title_from;
    field
        .as_deref()
        .and_then(|path| get_path(event, path))
        .map(display_value)
        .unwrap_or_default()
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".into(),
        _ => value.to_string(),
    }
}

/// Build the detail pane from the same body renderer names used by the web
/// viewer. The terminal version intentionally stays text-first, but it still
/// respects the format file instead of dumping an undifferentiated JSON blob.
fn formatted_event_lines(spec: &FormatSpec, event: &Value, ty: &str) -> Vec<Line<'static>> {
    let event_spec = spec.event(ty);
    let mut lines = Vec::new();
    let mut covered = HashSet::new();
    let mut rendered = 0usize;
    let mut omitted_json = false;

    for (field, kind) in &event_spec.body {
        covered.insert(field_root(field).to_string());
        if spec.defaults.ignore.iter().any(|ignored| ignored == field) {
            continue;
        }
        let Some(value) = get_path(event, field) else {
            continue;
        };
        if empty_value(value) {
            continue;
        }
        if kind == "json" {
            // JSON fields belong to the JSON tab. Rendering them here makes
            // large tool schemas (especially in llm_request) drown out the
            // actual prompt and duplicate the raw view.
            omitted_json = true;
            continue;
        }
        if rendered > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            format!("  {}", human_field(field)),
            Style::default().fg(Color::Cyan),
        )));
        append_formatted_value(&mut lines, kind, value);
        rendered += 1;
    }

    let type_root = field_root(&spec.trace.type_field);
    let timestamp_root = field_root(&spec.trace.timestamp_field);
    let sequence_root = spec.trace.sequence_field.as_deref().map(field_root);
    let title_root = event_spec.title_from.as_deref().map(field_root);
    let mut leftovers = Vec::new();
    if let Value::Object(map) = event {
        for (key, value) in map {
            if key == type_root
                || key == timestamp_root
                || sequence_root == Some(key.as_str())
                || title_root == Some(key.as_str())
                || covered.contains(key)
                || spec.defaults.ignore.iter().any(|ignored| ignored == key)
                || empty_value(value)
                || spec
                    .defaults
                    .header_fields
                    .iter()
                    .any(|header| header == key)
            {
                continue;
            }
            leftovers.push((key, value));
        }
    }
    if !leftovers.is_empty() {
        if rendered > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            "  other fields",
            Style::default().fg(Color::Cyan),
        )));
        for (key, value) in leftovers {
            append_pair(&mut lines, key, value);
        }
        rendered += 1;
    }

    if omitted_json {
        if rendered > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            "  JSON fields hidden · press Tab for the JSON panel",
            Style::default().fg(Color::DarkGray),
        )));
        rendered += 1;
    }
    if rendered == 0 {
        lines.push(Line::from(Span::styled(
            "  JSON available · press Tab for the JSON panel",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines
}

fn detail_body_lines(
    spec: &FormatSpec,
    event: &Value,
    ty: &str,
    raw_detail: bool,
) -> Vec<Line<'static>> {
    if raw_detail {
        let mut lines = Vec::new();
        append_json_lines(&mut lines, event);
        lines
    } else {
        formatted_event_lines(spec, event, ty)
    }
}

fn append_formatted_value(lines: &mut Vec<Line<'static>>, kind: &str, value: &Value) {
    match kind {
        "markdown" => {
            append_numbered_text_lines(lines, &display_value(value), "  ", true, Color::Gray)
        }
        "code" => append_code_lines(lines, &display_value(value), "  "),
        "output" => {
            append_numbered_text_lines(lines, &display_value(value), "  ", false, Color::Gray)
        }
        "text" => {
            append_numbered_text_lines(lines, &display_value(value), "  ", false, Color::White)
        }
        "json" => append_json_lines(lines, value),
        "kv" => append_kv(lines, value),
        "tokens" => append_tokens(lines, value),
        "messages" => append_messages(lines, value),
        "tool_args" => append_tool_args(lines, value),
        "tool_calls" | "tool_calls_brief" => append_tool_calls(lines, value),
        "list" => append_list(lines, value),
        _ => append_value_lines(lines, value),
    }
}

fn append_value_lines(lines: &mut Vec<Line<'static>>, value: &Value) {
    if let Some(text) = value.as_str() {
        append_numbered_text_lines(lines, text, "  ", false, Color::Gray);
    } else if value.is_object() || value.is_array() {
        append_json_lines(lines, value);
    } else {
        append_numbered_text_lines(lines, &display_value(value), "  ", false, Color::Gray);
    }
}

fn append_numbered_text_lines(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    indent: &str,
    markdown: bool,
    color: Color,
) {
    for (number, raw) in text.lines().enumerate() {
        let value = if markdown {
            clean_markdown_line(raw)
        } else {
            raw.to_string()
        };
        lines.push(Line::from(vec![
            line_number_span(number + 1),
            Span::styled(format!("{indent}{value}"), Style::default().fg(color)),
        ]));
    }
    if text.is_empty() {
        lines.push(Line::from(vec![
            line_number_span(1),
            Span::raw(indent.to_string()),
        ]));
    }
}

fn append_json_lines(lines: &mut Vec<Line<'static>>, value: &Value) {
    let rendered = if let Some(text) = value.as_str() {
        text.to_string()
    } else {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "<cannot render value>".into())
    };
    append_highlighted_lines(lines, &rendered, "  ", "json");
}

fn append_code_lines(lines: &mut Vec<Line<'static>>, text: &str, indent: &str) {
    append_highlighted_lines(lines, text, indent, guess_language(text));
}

fn append_highlighted_lines(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    indent: &str,
    language: &str,
) {
    for (number, raw) in text.lines().enumerate() {
        lines.push(highlight_line_numbered(raw, number + 1, indent, language));
    }
    if text.is_empty() {
        lines.push(Line::from(vec![
            line_number_span(1),
            Span::raw(indent.to_string()),
        ]));
    }
}

fn line_number_span(number: usize) -> Span<'static> {
    Span::styled(
        format!("{number:>4} │ "),
        Style::default().fg(Color::DarkGray),
    )
}

fn append_kv(lines: &mut Vec<Line<'static>>, value: &Value) {
    if let Value::Object(map) = value {
        for (key, value) in map {
            append_pair(lines, key, value);
        }
    } else {
        append_value_lines(lines, value);
    }
}

fn append_pair(lines: &mut Vec<Line<'static>>, key: &str, value: &Value) {
    let rendered = truncate(&display_value(value).replace('\n', " "), 180);
    lines.push(Line::from(vec![
        Span::styled(format!("    {key}: "), Style::default().fg(Color::DarkGray)),
        Span::styled(rendered, Style::default().fg(Color::Gray)),
    ]));
}

fn append_tokens(lines: &mut Vec<Line<'static>>, value: &Value) {
    if let Value::Object(map) = value {
        for key in [
            "prompt",
            "completion",
            "reasoning",
            "cached_prompt",
            "total",
        ] {
            if let Some(token_count) = map.get(key) {
                append_pair(lines, key, token_count);
            }
        }
        for (key, value) in map {
            if ![
                "prompt",
                "completion",
                "reasoning",
                "cached_prompt",
                "total",
            ]
            .contains(&key.as_str())
            {
                append_pair(lines, key, value);
            }
        }
    } else {
        append_value_lines(lines, value);
    }
}

fn append_list(lines: &mut Vec<Line<'static>>, value: &Value) {
    if let Value::Array(items) = value {
        for item in items {
            lines.push(Line::from(Span::styled(
                format!("    - {}", truncate(&display_value(item), 180)),
                Style::default().fg(Color::Gray),
            )));
        }
    } else {
        append_value_lines(lines, value);
    }
}

fn append_messages(lines: &mut Vec<Line<'static>>, value: &Value) {
    let Value::Array(messages) = value else {
        append_value_lines(lines, value);
        return;
    };
    for (index, message) in messages.iter().enumerate() {
        if index > 0 {
            lines.push(Line::from(""));
        }
        let role = message
            .get("role")
            .map(display_value)
            .unwrap_or_else(|| format!("message {}", index + 1));
        lines.push(Line::from(Span::styled(
            format!("    {role}"),
            Style::default().fg(Color::LightGreen),
        )));
        if let Some(content) = message.get("content") {
            if let Some(text) = content.as_str() {
                append_numbered_text_lines(lines, text, "      ", true, Color::Gray);
            } else {
                append_json_lines_indented(lines, content, "      ");
            }
        }
    }
}

fn append_tool_args(lines: &mut Vec<Line<'static>>, value: &Value) {
    let parsed = if let Some(text) = value.as_str() {
        serde_json::from_str::<Value>(text).unwrap_or_else(|_| Value::String(text.to_string()))
    } else {
        value.clone()
    };
    let Value::Object(map) = parsed else {
        append_value_lines(lines, &parsed);
        return;
    };
    const PRIMARY: [&str; 10] = [
        "command", "cmd", "code", "script", "query", "pattern", "path", "file", "content", "text",
    ];
    for (key, value) in &map {
        if PRIMARY.contains(&key.as_str()) {
            lines.push(Line::from(Span::styled(
                format!("    {key}:"),
                Style::default().fg(Color::LightCyan),
            )));
            if let Some(text) = value.as_str() {
                append_code_lines(lines, text, "      ");
            } else {
                append_json_lines_indented(lines, value, "      ");
            }
        } else {
            append_pair(lines, key, value);
        }
    }
}

fn append_tool_calls(lines: &mut Vec<Line<'static>>, value: &Value) {
    let Value::Array(calls) = value else {
        append_value_lines(lines, value);
        return;
    };
    for call in calls {
        let name = call
            .get("name")
            .map(display_value)
            .unwrap_or_else(|| "?".into());
        let id = call
            .get("id")
            .or_else(|| call.get("call_id"))
            .map(display_value)
            .unwrap_or_default();
        lines.push(Line::from(Span::styled(
            format!(
                "    {name}{}",
                if id.is_empty() {
                    String::new()
                } else {
                    format!("  · {id}")
                }
            ),
            Style::default().fg(Color::LightCyan),
        )));
        if let Some(arguments) = call.get("arguments") {
            // The web viewer uses a brief row that links to the full tool
            // call. In a terminal there is no second card to click, so show
            // the formatted arguments here even for tool_calls_brief.
            append_tool_args(lines, arguments);
        }
    }
}

fn append_json_lines_indented(lines: &mut Vec<Line<'static>>, value: &Value, indent: &str) {
    let rendered =
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "<cannot render value>".into());
    append_highlighted_lines(lines, &rendered, indent, "json");
}

fn guess_language(text: &str) -> &'static str {
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return "json";
    }
    if trimmed.starts_with("def ")
        || trimmed.starts_with("class ")
        || trimmed.starts_with("import ")
        || trimmed.starts_with("from ")
        || trimmed.contains("python")
    {
        return "python";
    }
    if trimmed.lines().any(|line| {
        let line = line.trim_start();
        line.contains(':') && line.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
    }) && !trimmed.contains(';')
    {
        return "yaml";
    }
    "bash"
}

fn highlight_line_numbered(
    line: &str,
    number: usize,
    indent: &str,
    language: &str,
) -> Line<'static> {
    let mut spans = vec![
        line_number_span(number),
        Span::styled(indent.to_string(), Style::default().fg(Color::DarkGray)),
    ];
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if (c == '#' && language != "json")
            || (c == '/' && i + 1 < chars.len() && chars[i + 1] == '/')
        {
            spans.push(Span::styled(
                chars[i..].iter().collect::<String>(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ));
            break;
        }
        if c == '"' || c == '\'' || c == '`' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i = (i + 2).min(chars.len());
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            let mut style = Style::default().fg(Color::LightGreen);
            if language == "json"
                && chars
                    .get(i..)
                    .is_some_and(|rest| rest.iter().find(|c| !c.is_whitespace()) == Some(&':'))
            {
                style = Style::default().fg(Color::LightBlue);
            }
            spans.push(Span::styled(token, style));
            continue;
        }
        if c.is_ascii_digit() && (i == 0 || !chars[i - 1].is_ascii_alphanumeric()) {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || ".-_".contains(chars[i])) {
                i += 1;
            }
            spans.push(Span::styled(
                chars[start..i].iter().collect::<String>(),
                Style::default().fg(Color::LightYellow),
            ));
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' || c == '-' {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '-')
            {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            spans.push(Span::styled(token.clone(), word_style(&token, language)));
            continue;
        }
        if "{}[]():,=<>|&;+$".contains(c) {
            spans.push(Span::styled(
                c.to_string(),
                Style::default().fg(Color::LightCyan),
            ));
        } else {
            spans.push(Span::raw(c.to_string()));
        }
        i += 1;
    }
    Line::from(spans)
}

fn word_style(word: &str, language: &str) -> Style {
    const PYTHON_KEYWORDS: [&str; 16] = [
        "def", "class", "return", "if", "elif", "else", "for", "while", "import", "from", "as",
        "with", "try", "except", "raise", "in",
    ];
    const BASH_COMMANDS: [&str; 20] = [
        "ls", "cd", "pwd", "cat", "head", "tail", "grep", "find", "sed", "awk", "echo", "python",
        "python3", "pytest", "cargo", "git", "mkdir", "rm", "cp", "mv",
    ];
    if language == "python" && PYTHON_KEYWORDS.contains(&word) {
        Style::default().fg(Color::LightMagenta)
    } else if language == "bash" && BASH_COMMANDS.contains(&word) {
        Style::default().fg(Color::LightBlue)
    } else if matches!(word, "true" | "false" | "null" | "None" | "True" | "False") {
        Style::default().fg(Color::LightRed)
    } else if word.starts_with('-') {
        Style::default().fg(Color::LightCyan)
    } else {
        Style::default().fg(Color::Gray)
    }
}

fn clean_markdown_line(line: &str) -> String {
    let trimmed = line.trim_start();
    if let Some(heading) = trimmed.strip_prefix("#### ") {
        return heading.to_string();
    }
    if let Some(heading) = trimmed.strip_prefix("### ") {
        return heading.to_string();
    }
    if let Some(heading) = trimmed.strip_prefix("## ") {
        return heading.to_string();
    }
    if let Some(heading) = trimmed.strip_prefix("# ") {
        return heading.to_string();
    }
    if let Some(item) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        return format!("• {item}");
    }
    trimmed.to_string()
}

fn empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn field_root(field: &str) -> &str {
    field.split('.').next().unwrap_or(field)
}

fn human_field(field: &str) -> String {
    field.replace('_', " ").to_uppercase()
}

fn run_summary(stats: &crate::trace::RunStats) -> String {
    let outcome = match stats.success {
        Some(true) => "PASS",
        Some(false) => "FAIL",
        None => "----",
    };
    format!(
        "{outcome}  {}  {} events  {} tools",
        fmt_duration(stats.duration_s),
        stats.events,
        stats.tool_calls
    )
}

fn fmt_duration(seconds: f64) -> String {
    if seconds < 0.001 {
        format!("{}µs", (seconds.max(0.0) * 1_000_000.0) as u64)
    } else if seconds < 1.0 {
        format!("{:.0}ms", seconds * 1_000.0)
    } else if seconds < 60.0 {
        format!("{seconds:.2}s")
    } else if seconds < 3600.0 {
        format!("{}m {:.0}s", (seconds / 60.0).floor(), seconds % 60.0)
    } else {
        format!(
            "{}h {}m",
            (seconds / 3600.0).floor(),
            ((seconds % 3600.0) / 60.0).round()
        )
    }
}

fn fmt_relative(timestamp: f64, start: f64) -> String {
    let delta = timestamp - start;
    let sign = if delta < 0.0 { '-' } else { '+' };
    format!("{sign}{}", fmt_duration(delta.abs()))
}

fn fmt_absolute(timestamp: f64) -> String {
    // Keep the TUI dependency-light. Unix seconds are more useful than a
    // locale-dependent wall-clock conversion in an audit view.
    format!("unix {timestamp:.3}")
}

fn truncate(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let shortened: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{shortened}…")
    } else {
        shortened
    }
}

fn tone_style(tone: &str) -> Style {
    Style::default().fg(tone_color(tone))
}

fn tone_color(tone: &str) -> Color {
    if let Some(hex) = tone.strip_prefix('#') {
        if hex.len() == 6 {
            let parse = |part: &str| u8::from_str_radix(part, 16).ok();
            if let (Some(r), Some(g), Some(b)) =
                (parse(&hex[0..2]), parse(&hex[2..4]), parse(&hex[4..6]))
            {
                return Color::Rgb(r, g, b);
            }
        }
    }
    match tone {
        "violet" => Color::LightMagenta,
        "sky" | "blue" => Color::LightBlue,
        "emerald" | "green" => Color::LightGreen,
        "amber" | "orange" => Color::LightYellow,
        "cyan" => Color::LightCyan,
        "pink" | "rose" => Color::LightRed,
        "lime" => Color::Yellow,
        "red" => Color::Red,
        "slate" | "zinc" | "gap" => Color::Gray,
        _ => Color::White,
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{RunStats, Span};

    #[test]
    fn colon_command_jumps_to_one_based_event_number() {
        let spec = FormatSpec::default();
        let run = Run {
            id: "run".into(),
            label: "run".into(),
            file: 0,
            stats: RunStats {
                events: 3,
                ..Default::default()
            },
            t_min: 0.0,
            t_max: 0.0,
            spans: Vec::<Span>::new(),
            events: vec![
                serde_json::json!({"t": "one"}),
                serde_json::json!({"t": "two"}),
                serde_json::json!({"t": "three"}),
            ],
        };
        let file = TraceFile {
            path: "trace.jsonl".into(),
            name: "trace.jsonl".into(),
            dir: String::new(),
            bytes: 0,
            runs: vec![run],
        };
        let files = vec![file];
        let mut app = App::new(
            &spec,
            files,
            Path::new("format.yaml"),
            Path::new("trace.jsonl"),
        );
        for code in [KeyCode::Char(':'), KeyCode::Char('2'), KeyCode::Enter] {
            assert!(!app.handle_key(KeyEvent::new(code, KeyModifiers::NONE)));
        }
        assert_eq!(app.event_index, 1);
        assert_eq!(app.focus, Focus::Events);
    }

    #[test]
    fn colon_command_jumps_to_numbered_detail_line_when_expanded() {
        let spec: FormatSpec = serde_yaml::from_str(
            "trace:\n  type_field: t\nevents:\n  tool_call:\n    body:\n      arguments: tool_args\n",
        )
        .unwrap();
        let run = Run {
            id: "run".into(),
            label: "run".into(),
            file: 0,
            stats: RunStats {
                events: 1,
                ..Default::default()
            },
            t_min: 0.0,
            t_max: 0.0,
            spans: Vec::<Span>::new(),
            events: vec![serde_json::json!({
                "t": "tool_call",
                "arguments": {"command": "first\nsecond"}
            })],
        };
        let file = TraceFile {
            path: "trace.jsonl".into(),
            name: "trace.jsonl".into(),
            dir: String::new(),
            bytes: 0,
            runs: vec![run],
        };
        let files = vec![file];
        let mut app = App::new(
            &spec,
            files,
            Path::new("format.yaml"),
            Path::new("trace.jsonl"),
        );
        app.detail_expanded = true;
        app.command_input = Some("2".into());
        app.finish_command();
        assert_eq!(app.detail_scroll, (DETAIL_BODY_OFFSET + 3) as u16);
    }
}
