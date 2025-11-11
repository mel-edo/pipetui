use std::fs;
use std::path::{Path, PathBuf};
use anyhow::Result;
use crate::history::App;

pub const HISTORY_LIMIT: usize = 500;

pub fn history_file() -> Result<PathBuf> {
    let proj = dirs::cache_dir()
        .or_else(|| dirs::data_dir())
        .ok_or_else(|| anyhow::anyhow!("no cache or data dir"))?
        .join("pipetui");
    fs::create_dir_all(&proj)?;
    Ok(proj.join("history.json"))
}

pub fn load_history(path: &Path) -> Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = fs::File::open(path)?;
    let hist: Vec<String> = serde_json::from_reader(file)?;
    Ok(hist)
}

pub fn save_history(app: &App) {
    if let Some(path) = &app.history_path {
        if let Ok(file) = fs::File::create(path) {
            let _ = serde_json::to_writer_pretty(file, &app.history);
        }
    }
}
