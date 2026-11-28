//! Inbox capture for business workspaces: text, images and files the user drops in (or
//! grabs from the clipboard with Ctrl+Alt+I) are saved to `<workspace>/inbox/` as
//! `YYYYMMDD-HHMM-<slug>.md`, and the orchestrator is told so it can triage them.

use std::path::{Path, PathBuf};

/// A short file-name slug from the first words of some text.
pub fn slug_from(text: &str) -> String {
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("capture");
    let s = crate::queue::slug(first);
    let s: String = s.chars().take(40).collect();
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() { "capture".into() } else { s }
}

/// A free path in the inbox for `stem` + `ext` (adds -2, -3... if taken).
fn free_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{stem}-{n}.{ext}"));
        n += 1;
    }
    p
}

fn stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M").to_string()
}

fn inbox_dir(ws: &str) -> Result<PathBuf, String> {
    let dir = Path::new(ws).join("inbox");
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Save captured text as an inbox note. Returns its path.
pub fn save_text(ws: &str, text: &str, source: &str) -> Result<PathBuf, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("nothing to save: the text is empty".into());
    }
    let dir = inbox_dir(ws)?;
    let p = free_path(&dir, &format!("{}-{}", stamp(), slug_from(text)), "md");
    let body = format!("<!-- captured {} from {source} -->\n\n{text}\n", chrono::Local::now().format("%Y-%m-%d %H:%M"));
    std::fs::write(&p, body).map_err(|e| e.to_string())?;
    Ok(p)
}

/// Save a captured file (image, PDF, text...) next to an inbox note that links to it.
/// Returns the note's path.
pub fn save_file(ws: &str, name: &str, bytes: &[u8], source: &str) -> Result<PathBuf, String> {
    let dir = inbox_dir(ws)?;
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !e.is_empty() && e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()) => (s, e.to_lowercase()),
        _ => (name, "bin".to_string()),
    };
    let base = format!("{}-{}", stamp(), slug_from(stem));
    let file = free_path(&dir, &base, &ext);
    std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
    let fname = file.file_name().unwrap().to_string_lossy().to_string();
    let note = free_path(&dir, &base, "md");
    let image = ["png", "jpg", "jpeg", "gif", "webp"].contains(&ext.as_str());
    let body = format!(
        "<!-- captured {} from {source} -->\n\n{}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M"),
        if image { format!("![{name}]({fname})") } else { format!("[{name}]({fname})") }
    );
    std::fs::write(&note, body).map_err(|e| e.to_string())?;
    Ok(note)
}

/// Encode an RGBA clipboard image as PNG.
pub fn png_from_rgba(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = vec![];
    {
        let mut enc = png::Encoder::new(&mut out, width as u32, height as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        w.write_image_data(rgba).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// What's on the clipboard now: text, or an image as PNG bytes.
pub enum Clip {
    Text(String),
    Png(Vec<u8>),
}

pub fn read_clipboard() -> Result<Clip, String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| format!("can't open the clipboard: {e}"))?;
    if let Ok(t) = cb.get_text() {
        if !t.trim().is_empty() {
            return Ok(Clip::Text(t));
        }
    }
    let img = cb.get_image().map_err(|_| "the clipboard has no text or image".to_string())?;
    Ok(Clip::Png(png_from_rgba(img.width, img.height, &img.bytes)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_text_and_files_with_dated_slugged_names() {
        let ws = std::env::temp_dir().join(format!("orch-inbox-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        let ws = ws.to_string_lossy().to_string();
        let p = save_text(&ws, "\n  New Fiverr message from Sam!\nCan you do it by Friday?", "test").unwrap();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        assert!(name.ends_with("-new-fiverr-message-from-sam.md"), "{name}");
        assert_eq!(name.len(), "YYYYMMDD-HHMM-".len() + "new-fiverr-message-from-sam.md".len());
        assert!(std::fs::read_to_string(&p).unwrap().contains("Can you do it by Friday?"));
        let again = save_text(&ws, "New Fiverr message from Sam!", "test").unwrap();
        assert_ne!(p, again, "never overwrites an earlier capture");
        let png = png_from_rgba(2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        let note = save_file(&ws, "Etsy stats.png", &png, "test").unwrap();
        let md = std::fs::read_to_string(&note).unwrap();
        assert!(md.contains("-etsy-stats.png)"), "{md}");
        assert!(save_text(&ws, "   ", "test").is_err());
    }
}
