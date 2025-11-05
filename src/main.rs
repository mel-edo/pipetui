use std::fs;
use std::io;
use std::path::{Path, PathBuf};
// use std::sync::Arc;
use std::thread;

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
    Result(ExecResult),
}

const HISTORY_LIMIT: usize = 500;

fn spawn_worker(rx: Receiver<WorkerMsg>, tx_ui: Sender<UiMsg>) {
    thread::spawn(move || {
        while let Ok(msg) = rx.recv() {
            match msg {
                WorkerMsg::Run(cmd) => {
                    #[cfg(target_os = "windows")]
                    let output = std::process::Command::new("cmd")
                        .args(&["/C", &cmd])
                        .output();
                    #[cfg(not(target_os = "windows"))]
                    let output = std::process::Command::new("sh")
                        .arg("-c")
                        .arg(&cmd)
                        .output();

                    match output {
                        Ok(out) => {
                            let status = out.status.code().unwrap_or(-1);
                            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                            let _ = tx_ui.send(UiMsg::Result(ExecResult {
                                cmd,
                                status,
                                stdout,
                                stderr,
                            }));
                        }
                        Err(e) => {
                            let _ = tx_ui.send(UiMsg::Result(ExecResult {
                                cmd,
                                status: -1,
                                stdout: String::new(),
                                stderr: format!("Failed to execute: {e}"),
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
        }
    }

    fn push_result(&mut self, res: ExecResult) {
        self.status_line = format!("exit {}", res.status);
        self.output_lines = if res.stdout.is_empty() {
            vec!["<no stdout>".into()]
        } else {
            res.stdout.lines().map(|s| s.to_string()).collect()
        };
        self.error_lines = if res.stderr.is_empty() {
            Vec::new()
        } else {
            res.stderr.lines().map(|s| s.to_string()).collect()
        };
        if !res.cmd.trim().is_empty() {
            self.append_history(res.cmd);
        }
    }

    fn visible_chunk<'a>(lines: &'a [String], area: Rect) -> Vec<Line<'a>> {
        let height = area.height.saturating_sub(2) as usize; // minus borders
        let total = lines.len();
        // show last `height` lines
        let start = total.saturating_sub(height);
        lines[start..]
            .iter()
            .map(|s| Line::from(s.as_str()))
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
            } else {
                let next = idx + 1;
                self.hist_pos = Some(next);
                self.input = self.history[next].clone();
                self.cursor = self.input.len();
            }
        } else {
            self.input.clear();
            self.cursor = 0;
        }
    }

    fn move_cursor_left(&mut self) {
        self.cursor = prev_grapheme_boundary(&self.input, self.cursor);
    }

    fn move_cursor_right(&mut self) {
        self.cursor = next_grapheme_boundary(&self.input, self.cursor);
    }

    fn move_cursor_home(&mut self) {
        self.cursor = 0;
    }

    fn move_cursor_end(&mut self) {
        self.cursor = self.input.len();
    }

    fn insert_char(&mut self, ch: char) {
        self.input.insert(self.cursor, ch);
        self.cursor = next_grapheme_boundary(&self.input, self.cursor);
        self.hist_pos = None;
    }

    fn delete_backward(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = prev_grapheme_boundary(&self.input, self.cursor);
        self.input.drain(prev..self.cursor);
        self.cursor = prev;
        self.hist_pos = None;
    }

    fn delete_forward(&mut self) {
        if self.cursor >= self.input.len() {
            return;
        }
        let next = next_grapheme_boundary(&self.input, self.cursor);
        self.input.drain(self.cursor..next);
        self.hist_pos = None;
    }

    fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.hist_pos = None;
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
    let out = Paragraph::new(App::visible_chunk(&app.output_lines, out_area))
        .block(out_block)
        .wrap(Wrap { trim: false });
    f.render_widget(out, out_area);

    // Stderr + Status
    let bottom_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(1)].as_ref())
        .split(chunks[2]);

    let err_block = Block::default().title("stderr").borders(Borders::ALL);
    let err = if app.error_lines.is_empty() {
        Paragraph::new(Line::from("<no stderr>")).block(err_block)
    } else {
        Paragraph::new(
            app.error_lines
                .iter()
                .map(|s| Line::from(s.as_str()))
                .collect::<Vec<_>>(),
        )
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
            let UiMsg::Result(res) = msg;
            app.push_result(res);
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
                        if !cmd.trim().is_empty() {
                            app.status_line = "running...".into();
                            app.hist_pos = None;
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
