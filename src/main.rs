use std::fs;
use std::io::BufRead;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{unbounded, Receiver, Sender};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Terminal;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Debug)]
struct ExecResult {
    cmd: String,
    status: i32,
    stdout: String,
    stderr: String,
}

enum WorkerMsg {
    Run(String),
}

enum UiMsg {
    Started(String),
    StdoutChunk(String),
    StderrChunk(String),
    Finished(ExecResult),
}

const HISTORY_LIMIT: usize = 500;

fn spawn_worker(rx: Receiver<WorkerMsg>, tx_ui: Sender<UiMsg>) {
    thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            match msg {
                WorkerMsg::Run(cmd) => {
                    let _ = tx_ui.send(UiMsg::Started(cmd.clone()));

                    #[cfg(target_os = "windows")]
                    let mut command = Command::new("cmd");
                    #[cfg(target_os = "windows")]
                    command.args(["/C", &cmd]);
                    #[cfg(not(target_os = "windows"))]
                    let mut command = Command::new("sh");
                    #[cfg(not(target_os = "windows"))]
                    command.args(["-c", &cmd]);

                    command.stdout(Stdio::piped()).stderr(Stdio::piped());

                    match command.spawn() {
                        Ok(mut child) => {
                            let stdout_log = Arc::new(Mutex::new(String::new()));
                            let stderr_log = Arc::new(Mutex::new(String::new()));

                            let (tx_stdout_chunk, rx_stdout_chunk) = unbounded::<String>();
                            let (tx_stderr_chunk, rx_stderr_chunk) = unbounded::<String>();

                            let stdout_handle = child.stdout.take().map(|stdout| {
                                let tx_chunk = tx_stdout_chunk.clone();
                                let log = Arc::clone(&stdout_log);
                                thread::spawn(move || stream_pipe(stdout, tx_chunk, log))
                            });

                            let stderr_handle = child.stderr.take().map(|stderr| {
                                let tx_chunk = tx_stderr_chunk.clone();
                                let log = Arc::clone(&stderr_log);
                                thread::spawn(move || stream_pipe(stderr, tx_chunk, log))
                            });

                            drop(tx_stdout_chunk);
                            drop(tx_stderr_chunk);

                            let agg_tx = tx_ui.clone();
                            let aggregator = thread::spawn(move || {
                                aggregate_streams(rx_stdout_chunk, rx_stderr_chunk, agg_tx);
                            });

                            let status = child.wait();

                            if let Some(handle) = stdout_handle {
                                let _ = handle.join();
                            }
                            if let Some(handle) = stderr_handle {
                                let _ = handle.join();
                            }
                            let _ = aggregator.join();

                            let status_code = status
                                .as_ref()
                                .ok()
                                .and_then(|s| s.code())
                                .unwrap_or(-1);

                            let stdout = stdout_log
                                .lock()
                                .map(|buf| buf.clone())
                                .unwrap_or_default();
                            let stderr = stderr_log
                                .lock()
                                .map(|buf| buf.clone())
                                .unwrap_or_default();

                            let _ = tx_ui.send(UiMsg::Finished(ExecResult {
                                cmd,
                                status: status_code,
                                stdout,
                                stderr,
                            }));
                        }
                        Err(e) => {
                            let _ = tx_ui.send(UiMsg::Finished(ExecResult {
                                cmd,
                                status: -1,
                                stdout: String::new(),
                                stderr: format!("Failed to spawn: {e}"),
                            }));
                        }
                    }
                }
            }
        }
    });
}

struct App {
    input: String,
    cursor: usize,
    history: Vec<String>,
    hist_pos: Option<usize>,
    output_lines: Vec<String>,
    error_lines: Vec<String>,
    status_line: String,
    history_path: Option<PathBuf>,
    stdout_partial: String,
    stderr_partial: String,
    is_running: bool,
    last_run_cmd: Option<String>,
    last_edit_at: Option<Instant>,
    append_history_on_finish: bool,
}

impl App {
    fn new() -> Self {
        let history_path = App::history_file().ok();
        let history = history_path
            .as_ref()
            .and_then(|path| App::load_history(path).ok())
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

    fn begin_run(&mut self, _cmd: String) {
        self.status_line = "running...".into();
        self.output_lines.clear();
        self.error_lines.clear();
        self.stdout_partial.clear();
        self.stderr_partial.clear();
        self.is_running = true;
    }

    fn append_stdout_chunk(&mut self, chunk: String) {
        Self::append_chunk(chunk, &mut self.stdout_partial, &mut self.output_lines);
    }

    fn append_stderr_chunk(&mut self, chunk: String) {
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

    fn finish_run(&mut self, res: ExecResult) {
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

    fn stdout_view<'a>(&'a self, area: Rect) -> Vec<Line<'a>> {
        Self::visible_chunk(
            &self.output_lines,
            (!self.stdout_partial.is_empty()).then_some(self.stdout_partial.as_str()),
            area,
        )
    }

    fn stderr_view<'a>(&'a self, area: Rect) -> Vec<Line<'a>> {
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

    fn history_file() -> Result<PathBuf> {
        let proj = dirs::cache_dir()
            .or_else(|| dirs::data_dir())
            .ok_or_else(|| anyhow::anyhow!("no cache or data dir"))?
            .join("pipetui");
        fs::create_dir_all(&proj)?;
        Ok(proj.join("history.json"))
    }

    fn load_history(path: &Path) -> Result<Vec<String>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let file = fs::File::open(path)?;
        let hist: Vec<String> = serde_json::from_reader(file)?;
        Ok(hist)
    }

    fn save_history(&self) {
        if let Some(path) = &self.history_path {
            if let Ok(file) = fs::File::create(path) {
                let _ = serde_json::to_writer_pretty(file, &self.history);
            }
        }
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
        self.save_history();
        self.hist_pos = None;
    }

    fn history_prev(&mut self) {
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

    fn history_next(&mut self) {
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

    fn insert_char(&mut self, ch: char) {
        self.input.insert(self.cursor, ch);
        self.cursor = next_grapheme_boundary(&self.input, self.cursor);
        self.hist_pos = None;
        self.mark_edited();
    }

    fn delete_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = prev_grapheme_boundary(&self.input, self.cursor);
        self.input.drain(prev..self.cursor);
        self.cursor = prev;
        self.hist_pos = None;
        self.mark_edited();
    }

    fn delete_forward(&mut self) {
        if self.cursor >= self.input.len() {
            return;
        }
        let next = next_grapheme_boundary(&self.input, self.cursor);
        self.input.drain(self.cursor..next);
        self.hist_pos = None;
        self.mark_edited();
    }

    fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.hist_pos = None;
        self.mark_edited();
    }

    fn move_cursor_home(&mut self) {
        self.cursor = 0;
    }

    fn move_cursor_end(&mut self) {
        self.cursor = self.input.len();
    }

    fn move_cursor_left(&mut self) {
        self.cursor = prev_grapheme_boundary(&self.input, self.cursor);
    }

    fn move_cursor_right(&mut self) {
        self.cursor = next_grapheme_boundary(&self.input, self.cursor);
    }

    fn mark_edited(&mut self) {
        self.last_edit_at = Some(Instant::now());
    }

    fn should_auto_run(&self) -> bool {
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

    fn prepare_run(&mut self, cmd: &str, manual: bool) -> bool {
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

fn ui<B: ratatui::backend::Backend>(f: &mut ratatui::Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Length(3),
                Constraint::Min(6),
                Constraint::Length(6),
            ]
            .as_ref(),
        )
        .split(f.size());

    // Input
    let input = Paragraph::new(app.input.as_str())
        .block(Block::default().title("pipeline").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    f.render_widget(input, chunks[0]);

    // Output
    let out_block = Block::default().title("stdout").borders(Borders::ALL);
    let out_area = chunks[1];
    let stdout_lines = app.stdout_view(out_area);
    let out = if stdout_lines.is_empty() {
        Paragraph::new(Line::from("(waiting for output...)"))
            .block(out_block)
            .wrap(Wrap { trim: false })
    } else {
        Paragraph::new(stdout_lines)
            .block(out_block)
            .wrap(Wrap { trim: false })
    };
    f.render_widget(out, out_area);

    // Stderr + Status
    let bottom_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(1)].as_ref())
        .split(chunks[2]);

    let err_block = Block::default().title("stderr").borders(Borders::ALL);
    let stderr_lines = app.stderr_view(bottom_chunks[0]);
    let err = if stderr_lines.is_empty() {
        Paragraph::new(Line::from("<no stderr>")).block(err_block)
    } else {
        Paragraph::new(stderr_lines)
            .block(err_block)
            .wrap(Wrap { trim: false })
    };
    f.render_widget(err, bottom_chunks[0]);

    let status = Paragraph::new(Line::from(vec![
        Span::styled("Status: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(&app.status_line),
        Span::raw("   "),
        Span::raw("Keys: Enter=run  Esc=quit  Ctrl+u=clear  ↑/↓=history  ←/→=move  Home/End"),
    ]));
    f.render_widget(status, bottom_chunks[1]);

    // Set cursor to input box
    let cursor_x =
        chunks[0].x + 1 + unicode_width::UnicodeWidthStr::width(&app.input[..app.cursor]) as u16;
    let y = chunks[0].y + 1;
    f.set_cursor(cursor_x, y);
}

fn stream_pipe(pipe: impl Read, tx: Sender<String>, log: Arc<Mutex<String>>) {
    let mut reader = BufReader::new(pipe);
    let mut buf = Vec::with_capacity(4096);
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {
                let chunk = String::from_utf8_lossy(&buf).to_string();
                if let Ok(mut guard) = log.lock() {
                    guard.push_str(&chunk);
                }
                if tx.send(chunk).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

fn aggregate_streams(
    rx_stdout: Receiver<String>,
    rx_stderr: Receiver<String>,
    tx_ui: Sender<UiMsg>,
) {
    let ticker = crossbeam_channel::tick(Duration::from_millis(250));
    let mut pending_stdout = String::new();
    let mut pending_stderr = String::new();
    let mut stdout_open = true;
    let mut stderr_open = true;

    loop {
        crossbeam_channel::select! {
            recv(rx_stdout) -> msg => match msg {
                Ok(chunk) => {
                    pending_stdout.push_str(&chunk);
                    continue;
                }
                Err(_) => stdout_open = false,
            },
            recv(rx_stderr) -> msg => match msg {
                Ok(chunk) => {
                    pending_stderr.push_str(&chunk);
                    continue;
                }
                Err(_) => stderr_open = false,
            },
            recv(ticker) -> _ => {},
        }

        if !pending_stdout.is_empty() {
            let chunk = std::mem::take(&mut pending_stdout);
            let _ = tx_ui.send(UiMsg::StdoutChunk(chunk));
        }
        if !pending_stderr.is_empty() {
            let chunk = std::mem::take(&mut pending_stderr);
            let _ = tx_ui.send(UiMsg::StderrChunk(chunk));
        }

        if !stdout_open && !stderr_open {
            if pending_stdout.is_empty() && pending_stderr.is_empty() {
                break;
            }
        }
    }

    if !pending_stdout.is_empty() {
        let _ = tx_ui.send(UiMsg::StdoutChunk(pending_stdout));
    }
    if !pending_stderr.is_empty() {
        let _ = tx_ui.send(UiMsg::StderrChunk(pending_stderr));
    }
}

fn prev_grapheme_boundary(text: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }
    let mut prev = 0;
    for (idx, _) in text.grapheme_indices(true) {
        if idx >= cursor {
            break;
        }
        prev = idx;
    }
    prev
}

fn next_grapheme_boundary(text: &str, cursor: usize) -> usize {
    if cursor >= text.len() {
        return text.len();
    }
    for (idx, _) in text.grapheme_indices(true) {
        if idx > cursor {
            return idx;
        }
    }
    text.len()
}

fn main() -> Result<()> {
    // channels
    let (tx_worker, rx_worker) = unbounded::<WorkerMsg>();
    let (tx_ui, rx_ui) = unbounded::<UiMsg>();
    spawn_worker(rx_worker, tx_ui);

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        crossterm::cursor::Hide
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut app = App::new();

    loop {
        terminal.draw(|f| ui::<CrosstermBackend<std::io::Stdout>>(f, &app))?;

        // check for worker results without blocking UI
        while let Ok(msg) = rx_ui.try_recv() {
            match msg {
                UiMsg::Started(cmd) => app.begin_run(cmd),
                UiMsg::StdoutChunk(chunk) => app.append_stdout_chunk(chunk),
                UiMsg::StderrChunk(chunk) => app.append_stderr_chunk(chunk),
                UiMsg::Finished(res) => app.finish_run(res),
            }
        }

        if app.should_auto_run() {
            let cmd = app.input.clone();
            if app.prepare_run(&cmd, false) {
                tx_worker.send(WorkerMsg::Run(cmd)).ok();
            }
        }

        // handle input
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                // ignore repeats
                if key.kind == KeyEventKind::Repeat {
                    continue;
                }
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        break;
                    }
                    KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.move_cursor_home();
                    }
                    KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.move_cursor_end();
                    }
                    KeyCode::Esc => break,
                    KeyCode::Enter => {
                        let cmd = app.input.clone();
                        if app.prepare_run(&cmd, true) {
                            tx_worker.send(WorkerMsg::Run(cmd)).ok();
                        }
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        app.clear_input();
                    }
                    KeyCode::Backspace => {
                        app.delete_backward();
                    }
                    KeyCode::Delete => {
                        app.delete_forward();
                    }
                    KeyCode::Left => {
                        app.move_cursor_left();
                    }
                    KeyCode::Right => {
                        app.move_cursor_right();
                    }
                    KeyCode::Home => {
                        app.move_cursor_home();
                    }
                    KeyCode::End => {
                        app.move_cursor_end();
                    }
                    KeyCode::Up => {
                        app.history_prev();
                    }
                    KeyCode::Down => {
                        app.history_next();
                    }
                    KeyCode::Char(ch) => {
                        if key.modifiers.contains(KeyModifiers::ALT)
                            || key.modifiers.contains(KeyModifiers::CONTROL)
                        {
                            continue;
                        }
                        app.insert_char(ch);
                    }
                    _ => {}
                }
            }
        }
    }

    // restore terminal
    disable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        crossterm::cursor::Show,
        terminal::LeaveAlternateScreen
    )?;
    Ok(())
}
