use std::path::PathBuf;
use std::time::{Duration, Instant};
use ratatui::layout::Rect;
use ratatui::text::Line;
use crate::execution::ExecResult;
use crate::persistence::{self, HISTORY_LIMIT};
use crate::parser::{next_grapheme_boundary, prev_grapheme_boundary};

pub struct App {
    pub input: String,
    pub cursor: usize,
    pub history: Vec<String>,
    pub hist_pos: Option<usize>,
    pub output_lines: Vec<String>,
    pub error_lines: Vec<String>,
    pub status_line: String,
    pub history_path: Option<PathBuf>,
    pub stdout_partial: String,
    pub stderr_partial: String,
    pub is_running: bool,
    pub last_run_cmd: Option<String>,
    pub last_edit_at: Option<Instant>,
    pub append_history_on_finish: bool,
}

impl App {
    pub fn new() -> Self {
        let history_path = persistence::history_file().ok();
        let history = history_path
            .as_ref()
            .and_then(|path| persistence::load_history(path).ok())
            .unwrap_or_default();

        Self {
            input: String::new(),
            cursor: 0,
            history,
            hist_pos: None,
            output_lines: vec!["(output will appear here)".into()],
            error_lines: Vec::new(),
            status_line: "Ready".into(),
            history_path,
            stdout_partial: String::new(),
            stderr_partial: String::new(),
            is_running: false,
            last_run_cmd: None,
            last_edit_at: None,
            append_history_on_finish: false,
        }
    }

    pub fn begin_run(&mut self, _cmd: String) {
        self.status_line = "running...".into();
        self.output_lines.clear();
        self.error_lines.clear();
        self.stdout_partial.clear();
        self.stderr_partial.clear();
        self.is_running = true;
    }

    pub fn append_stdout_chunk(&mut self, chunk: String) {
        Self::append_chunk(chunk, &mut self.stdout_partial, &mut self.output_lines);
    }

    pub fn append_stderr_chunk(&mut self, chunk: String) {
        Self::append_chunk(chunk, &mut self.stderr_partial, &mut self.error_lines);
    }

    fn append_chunk(chunk: String, partial: &mut String, lines: &mut Vec<String>) {
        partial.push_str(&chunk);
        while let Some(pos) = partial.find('\n') {
            let mut line = partial[..pos].to_string();
            if line.ends_with('\r') {
                line.pop();
            }
            lines.push(line);
            partial.drain(..=pos);
        }
    }

    fn flush_partials(&mut self) {
        if !self.stdout_partial.is_empty() {
            let line = self.stdout_partial.trim_end_matches('\r').to_string();
            self.output_lines.push(line);
            self.stdout_partial.clear();
        }
        if !self.stderr_partial.is_empty() {
            let line = self.stderr_partial.trim_end_matches('\r').to_string();
            self.error_lines.push(line);
            self.stderr_partial.clear();
        }
    }

    pub fn finish_run(&mut self, res: ExecResult) {
        self.flush_partials();
        if self.output_lines.is_empty() {
            if res.stdout.is_empty() {
                self.output_lines.push("<no stdout>".into());
            } else {
                self.output_lines = res.stdout.lines().map(|s| s.to_string()).collect();
            }
        }
        if self.error_lines.is_empty() && !res.stderr.is_empty() {
            self.error_lines = res.stderr.lines().map(|s| s.to_string()).collect();
        }
        self.status_line = format!("exit {}", res.status);
        self.is_running = false;
        if self.append_history_on_finish && !res.cmd.trim().is_empty() {
            self.append_history(res.cmd);
        }
        self.append_history_on_finish = false;
    }

    pub fn stdout_view<'a>(&'a self, area: Rect) -> Vec<Line<'a>> {
        Self::visible_chunk(
            &self.output_lines,
            (!self.stdout_partial.is_empty()).then_some(self.stdout_partial.as_str()),
            area,
        )
    }

    pub fn stderr_view<'a>(&'a self, area: Rect) -> Vec<Line<'a>> {
        Self::visible_chunk(
            &self.error_lines,
            (!self.stderr_partial.is_empty()).then_some(self.stderr_partial.as_str()),
            area,
        )
    }

    fn visible_chunk<'a>(
        lines: &'a [String],
        tail: Option<&'a str>,
        area: Rect,
    ) -> Vec<Line<'a>> {
        let height = area.height.saturating_sub(2) as usize; // minus borders
        let mut display: Vec<&'a str> = lines.iter().map(|s| s.as_str()).collect();
        if let Some(extra) = tail {
            if !extra.is_empty() {
                display.push(extra);
            }
        }
        if display.is_empty() {
            return Vec::new();
        }
        let total = display.len();
        let start = total.saturating_sub(height);
        display[start..]
            .iter()
            .map(|s| Line::from(*s))
            .collect()
    }

    fn append_history(&mut self, entry: String) {
        if let Some(last) = self.history.last() {
            if last == &entry {
                return;
            }
        }
        self.history.push(entry);
        if self.history.len() > HISTORY_LIMIT {
            let remove_count = self.history.len() - HISTORY_LIMIT;
            self.history.drain(0..remove_count);
        }
        persistence::save_history(self);
        self.hist_pos = None;
    }

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next_idx = match self.hist_pos {
            None => self.history.len().saturating_sub(1),
            Some(0) => 0,
            Some(idx) => idx - 1,
        };
        self.hist_pos = Some(next_idx);
        self.input = self.history[next_idx].clone();
        self.cursor = self.input.len();
        self.last_run_cmd = None;
        self.mark_edited();
    }

    pub fn history_next(&mut self) {
        if self.history.is_empty() {
            return;
        }
        if let Some(idx) = self.hist_pos {
            if idx >= self.history.len().saturating_sub(1) {
                self.hist_pos = None;
                self.input.clear();
                self.cursor = 0;
                self.last_run_cmd = None;
                self.mark_edited();
            } else {
                let next = idx + 1;
                self.hist_pos = Some(next);
                self.input = self.history[next].clone();
                self.cursor = self.input.len();
                self.last_run_cmd = None;
                self.mark_edited();
            }
        } else {
            self.input.clear();
            self.cursor = 0;
            self.last_run_cmd = None;
            self.mark_edited();
        }
    }

    pub fn insert_char(&mut self, ch: char) {
        self.input.insert(self.cursor, ch);
        self.cursor = next_grapheme_boundary(&self.input, self.cursor);
        self.hist_pos = None;
        self.mark_edited();
    }

    pub fn delete_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = prev_grapheme_boundary(&self.input, self.cursor);
        self.input.drain(prev..self.cursor);
        self.cursor = prev;
        self.hist_pos = None;
        self.mark_edited();
    }

    pub fn delete_forward(&mut self) {
        if self.cursor >= self.input.len() {
            return;
        }
        let next = next_grapheme_boundary(&self.input, self.cursor);
        self.input.drain(self.cursor..next);
        self.hist_pos = None;
        self.mark_edited();
    }

    pub fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.hist_pos = None;
        self.mark_edited();
    }

    pub fn move_cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_cursor_end(&mut self) {
        self.cursor = self.input.len();
    }

    pub fn move_cursor_left(&mut self) {
        self.cursor = prev_grapheme_boundary(&self.input, self.cursor);
    }

    pub fn move_cursor_right(&mut self) {
        self.cursor = next_grapheme_boundary(&self.input, self.cursor);
    }

    pub fn mark_edited(&mut self) {
        self.last_edit_at = Some(Instant::now());
    }

    pub fn should_auto_run(&self) -> bool {
        if self.is_running {
            return false;
        }
        let Some(edit_at) = self.last_edit_at else {
            return false;
        };
        if edit_at.elapsed() < Duration::from_millis(250) {
            return false;
        }
        if self.input.trim().is_empty() {
            return false;
        }
        if let Some(last) = &self.last_run_cmd {
            if last == &self.input {
                return false;
            }
        }
        true
    }

    pub fn prepare_run(&mut self, cmd: &str, manual: bool) -> bool {
        if cmd.trim().is_empty() {
            return false;
        }
        self.hist_pos = None;
        self.last_run_cmd = Some(cmd.to_string());
        self.last_edit_at = None;
        self.append_history_on_finish = manual;
        true
    }
}
