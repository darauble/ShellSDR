//! File-open dialog for the baseband File source (spec 7.2).
//!
//! A modal directory browser: the current folder, `../`, sub-folders first,
//! then `*.wav` files (or all files), a details line for the selected file
//! (rate, center, length from its header), and a row of visible buttons
//! `[Open] [Up] [Home] [Path…] [WAV only] [Cancel]` ("no hidden shortcuts":
//! the keys are accelerators for these). The model touches the file system
//! only to list a folder and read a header, so it is tested on a temp dir.

use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};

use crate::ui::mouse::{HitMap, HitTarget};
use crate::ui::render::{Tagged, record_spans, untag};
use crate::ui::theme::Theme;

/// What a listed entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Parent,
    Dir,
    File,
}

/// One row of the listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub kind: EntryKind,
    /// File size in bytes (0 for folders).
    pub size: u64,
}

/// The dialog's buttons, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserButton {
    Open,
    Up,
    Home,
    Path,
    Filter,
    Cancel,
}

impl BrowserButton {
    pub const ALL: [BrowserButton; 6] = [
        BrowserButton::Open,
        BrowserButton::Up,
        BrowserButton::Home,
        BrowserButton::Path,
        BrowserButton::Filter,
        BrowserButton::Cancel,
    ];

    fn label(self, show_all: bool) -> &'static str {
        match self {
            BrowserButton::Open => "[Open]",
            BrowserButton::Up => "[Up]",
            BrowserButton::Home => "[Home]",
            BrowserButton::Path => "[Path…]",
            BrowserButton::Filter if show_all => "[All files]",
            BrowserButton::Filter => "[WAV only]",
            BrowserButton::Cancel => "[Cancel]",
        }
    }
}

/// Where the keyboard is inside the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserFocus {
    List,
    Buttons,
}

/// What the dialog asks the app to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing for the app (navigation happened inside the dialog).
    Stay,
    /// Load this file.
    Choose(PathBuf),
    /// Switch to the typed-path prompt, starting in this folder.
    TypePath(PathBuf),
    Cancel,
}

/// The open dialog's state.
#[derive(Debug, Clone, PartialEq)]
pub struct FileBrowser {
    pub dir: PathBuf,
    pub entries: Vec<Entry>,
    pub cursor: usize,
    /// First listed row shown (kept so the window does not jump).
    pub scroll: usize,
    /// List every file, not only `*.wav`.
    pub show_all: bool,
    pub focus: BrowserFocus,
    pub button: BrowserButton,
    /// Why the last navigation or load failed.
    pub error: Option<String>,
    /// Details of the selected entry (header of a WAV, item count of a
    /// folder), refreshed when the cursor moves.
    pub details: String,
    /// Rows the list showed in the last draw (for paging).
    pub page: usize,
}

/// The user's home folder, if known.
pub fn home_dir() -> Option<PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())
}

fn is_wav(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("wav"))
}

/// List `dir`: `..` (unless at the root), folders, then files (only `*.wav`
/// unless `show_all`), each group sorted case-insensitively. Hidden (dot)
/// entries are skipped; entries that cannot be inspected are skipped too.
pub fn list_dir(dir: &Path, show_all: bool) -> std::io::Result<Vec<Entry>> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let Ok(e) = e else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        // `metadata` follows symlinks, so a link to a folder is a folder.
        let Ok(md) = std::fs::metadata(e.path()) else {
            continue;
        };
        if md.is_dir() {
            dirs.push(Entry {
                name,
                kind: EntryKind::Dir,
                size: 0,
            });
        } else if show_all || is_wav(&name) {
            files.push(Entry {
                name,
                kind: EntryKind::File,
                size: md.len(),
            });
        }
    }
    let key = |e: &Entry| e.name.to_lowercase();
    dirs.sort_by_key(key);
    files.sort_by_key(key);
    let mut out = Vec::new();
    if dir.parent().is_some() {
        out.push(Entry {
            name: "..".to_string(),
            kind: EntryKind::Parent,
            size: 0,
        });
    }
    out.extend(dirs);
    out.extend(files);
    Ok(out)
}

/// A file size as `12.3 MB` / `512 kB` / `20 B`.
pub fn size_label(bytes: u64) -> String {
    match bytes {
        b if b >= 1_000_000_000 => format!("{:.1} GB", b as f64 / 1e9),
        b if b >= 1_000_000 => format!("{:.1} MB", b as f64 / 1e6),
        b if b >= 1_000 => format!("{} kB", b / 1_000),
        b => format!("{b} B"),
    }
}

impl FileBrowser {
    /// Open in `dir`, with the cursor on `select` if it is listed. A folder
    /// that cannot be read falls back to its parents, then home.
    pub fn open(dir: &Path, select: Option<&str>) -> Self {
        let mut b = Self {
            dir: PathBuf::new(),
            entries: Vec::new(),
            cursor: 0,
            scroll: 0,
            show_all: false,
            focus: BrowserFocus::List,
            button: BrowserButton::Open,
            error: None,
            details: String::new(),
            page: 10,
        };
        let mut candidates: Vec<PathBuf> = dir.ancestors().map(Path::to_path_buf).collect();
        candidates.extend(home_dir());
        for d in candidates {
            if b.enter(&d).is_ok() {
                break;
            }
        }
        if let Some(name) = select {
            b.select_name(name);
        }
        b
    }

    /// List `dir` and make it current (cursor on the first real entry).
    /// On failure the current listing is kept and the error is shown.
    pub fn enter(&mut self, dir: &Path) -> Result<(), String> {
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        match list_dir(&dir, self.show_all) {
            Ok(entries) => {
                self.dir = dir;
                self.entries = entries;
                self.cursor = usize::from(
                    self.entries.first().map(|e| e.kind) == Some(EntryKind::Parent)
                        && self.entries.len() > 1,
                );
                self.scroll = 0;
                self.error = None;
                self.refresh_details();
                Ok(())
            }
            Err(e) => {
                let msg = format!("{}: {e}", dir.display());
                self.error = Some(msg.clone());
                Err(msg)
            }
        }
    }

    /// Put the cursor on the entry called `name`, if listed.
    pub fn select_name(&mut self, name: &str) {
        if let Some(i) = self.entries.iter().position(|e| e.name == name) {
            self.cursor = i;
            self.refresh_details();
        }
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    /// Go to the parent folder, with the cursor on the folder we left.
    pub fn up(&mut self) {
        let here = self.dir.clone();
        if let Some(parent) = here.parent()
            && self.enter(parent).is_ok()
            && let Some(name) = here.file_name()
        {
            self.select_name(&name.to_string_lossy());
        }
    }

    pub fn home(&mut self) {
        if let Some(h) = home_dir() {
            let _ = self.enter(&h);
        }
    }

    /// Toggle `*.wav` only / all files, keeping the selection by name.
    pub fn toggle_filter(&mut self) {
        let keep = self.selected().map(|e| e.name.clone());
        self.show_all = !self.show_all;
        let dir = self.dir.clone();
        let _ = self.enter(&dir);
        if let Some(n) = keep {
            self.select_name(&n);
        }
    }

    /// Move the cursor by `delta` rows, clamped.
    pub fn move_by(&mut self, delta: i64) {
        if self.entries.is_empty() {
            return;
        }
        let last = self.entries.len() as i64 - 1;
        self.cursor = (self.cursor as i64 + delta).clamp(0, last) as usize;
        self.error = None;
        self.refresh_details();
    }

    pub fn first(&mut self) {
        self.move_by(-(self.entries.len() as i64));
    }

    pub fn last(&mut self) {
        self.move_by(self.entries.len() as i64);
    }

    /// Type-ahead: jump to the next entry (after the cursor, wrapping) whose
    /// name starts with `c`, ignoring case.
    pub fn jump_to(&mut self, c: char) {
        let n = self.entries.len();
        let c = c.to_lowercase().to_string();
        for k in 1..=n {
            let i = (self.cursor + k) % n;
            if self.entries[i].name.to_lowercase().starts_with(&c) {
                self.cursor = i;
                self.refresh_details();
                return;
            }
        }
    }

    /// ENTER on the list: open a folder (or `..`), or choose a file.
    pub fn activate(&mut self) -> Outcome {
        let Some(e) = self.selected().cloned() else {
            return Outcome::Stay;
        };
        match e.kind {
            EntryKind::Parent => self.up(),
            EntryKind::Dir => {
                let d = self.dir.join(&e.name);
                let _ = self.enter(&d);
            }
            EntryKind::File => return Outcome::Choose(self.dir.join(&e.name)),
        }
        Outcome::Stay
    }

    /// Press a button.
    pub fn press(&mut self, b: BrowserButton) -> Outcome {
        self.button = b;
        match b {
            BrowserButton::Open => return self.activate(),
            BrowserButton::Up => self.up(),
            BrowserButton::Home => self.home(),
            BrowserButton::Path => return Outcome::TypePath(self.dir.clone()),
            BrowserButton::Filter => self.toggle_filter(),
            BrowserButton::Cancel => return Outcome::Cancel,
        }
        Outcome::Stay
    }

    /// Select the previous/next button (wraps).
    pub fn step_button(&mut self, dir: i32) {
        let all = BrowserButton::ALL;
        let i = all.iter().position(|b| *b == self.button).unwrap_or(0) as i32;
        self.button = all[(i + dir).rem_euclid(all.len() as i32) as usize];
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            BrowserFocus::List => BrowserFocus::Buttons,
            BrowserFocus::Buttons => BrowserFocus::List,
        };
    }

    /// A click on listed row `i`: the first click selects it, a click on the
    /// selected row opens it (the terminal's stand-in for a double click).
    pub fn click_row(&mut self, i: usize) -> Outcome {
        self.focus = BrowserFocus::List;
        if i >= self.entries.len() {
            return Outcome::Stay;
        }
        if i == self.cursor {
            return self.activate();
        }
        self.cursor = i;
        self.error = None;
        self.refresh_details();
        Outcome::Stay
    }

    /// Recompute the details line for the selected entry.
    fn refresh_details(&mut self) {
        self.details = match self.selected() {
            None => "(empty folder)".to_string(),
            Some(e) => match e.kind {
                EntryKind::Parent => "Parent folder".to_string(),
                EntryKind::Dir => match list_dir(&self.dir.join(&e.name), self.show_all) {
                    Ok(v) => {
                        let n = v.iter().filter(|x| x.kind != EntryKind::Parent).count();
                        format!("Folder, {n} item(s)")
                    }
                    Err(err) => format!("Folder: {err}"),
                },
                EntryKind::File => match crate::source::file::probe(&self.dir.join(&e.name)) {
                    Ok(m) => {
                        let center = m
                            .center_hz
                            .map(|c| format!("{:.6} MHz", c as f64 / 1e6))
                            .unwrap_or_else(|| "center ?".to_string());
                        format!(
                            "I/Q {:.0} kHz  {center}  {}",
                            m.sample_rate as f64 / 1e3,
                            crate::ui::render::clock_label(m.duration_s())
                        )
                    }
                    Err(err) => format!("Not playable: {err}"),
                },
            },
        };
    }

    /// Keep the cursor inside a window of `rows` list rows and remember the
    /// page size (called by the event loop before each draw).
    pub fn reconcile_scroll(&mut self, rows: usize) {
        self.page = rows.max(1);
        self.scroll = window_start(self.scroll, self.cursor, self.entries.len(), rows);
    }
}

/// First visible row of a `rows`-row window over `len` entries that starts
/// near `scroll` and shows `cursor`.
fn window_start(scroll: usize, cursor: usize, len: usize, rows: usize) -> usize {
    if rows == 0 {
        return scroll;
    }
    let mut s = scroll;
    if cursor < s {
        s = cursor;
    } else if cursor >= s + rows {
        s = cursor + 1 - rows;
    }
    s.min(len.saturating_sub(rows))
}

/// Shorten `s` to `w` columns keeping its end.
fn tail(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let t: String = s.chars().skip(n - (w - 1)).collect();
    format!("…{t}")
}

/// Shorten `s` to `w` columns keeping its start.
fn head(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let h: String = s.chars().take(w - 1).collect();
    format!("{h}…")
}

/// Dialog box size limits.
const MAX_W: u16 = 76;
const MAX_H: u16 = 26;

/// The dialog's box, centered in `area`.
fn dialog_rect(area: Rect) -> Rect {
    let w = MAX_W.min(area.width);
    let h = MAX_H.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
    )
}

/// List rows the dialog shows on a screen of `area` (box minus borders,
/// folder, details and button rows).
pub fn list_rows(area: Rect) -> usize {
    (dialog_rect(area).height as usize).saturating_sub(2 + 3)
}

/// Draw the dialog centered in `area` and record its hit regions.
pub fn render_browser(
    b: &FileBrowser,
    area: Rect,
    theme: &Theme,
    buf: &mut Buffer,
    hits: &mut HitMap,
) {
    let rect = dialog_rect(area);
    hits.push(rect, HitTarget::BrowserFrame);
    Clear.render(rect, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.border_style(true))
        .title("Open baseband WAV");
    let inner = block.inner(rect);
    block.render(rect, buf);
    // Rows: folder, list..., details, buttons.
    if inner.height < 4 || inner.width < 10 {
        return;
    }
    let iw = inner.width as usize;
    let list_rows = inner.height as usize - 3;
    let scroll = window_start(b.scroll, b.cursor, b.entries.len(), list_rows);

    let mut rows: Vec<Vec<Tagged>> = Vec::new();
    rows.push(vec![(
        Span::styled(tail(&b.dir.display().to_string(), iw), theme.dim_style()),
        None,
    )]);
    let list_focused = b.focus == BrowserFocus::List;
    for r in 0..list_rows {
        let i = scroll + r;
        let Some(e) = b.entries.get(i) else {
            if i == 0 {
                rows.push(vec![(
                    Span::styled("(no WAV files here)", theme.dim_style()),
                    None,
                )]);
            } else {
                rows.push(Vec::new());
            }
            continue;
        };
        let (name, size) = match e.kind {
            EntryKind::Parent => ("../".to_string(), String::new()),
            EntryKind::Dir => (format!("{}/", e.name), String::new()),
            EntryKind::File => (e.name.clone(), size_label(e.size)),
        };
        let marker = if i == b.cursor { '›' } else { ' ' };
        let name_w = iw.saturating_sub(size.chars().count() + 3);
        let text = format!("{marker}{:<name_w$} {size}", head(&name, name_w));
        let selected = i == b.cursor;
        let style = if selected {
            theme.field_style(true, list_focused)
        } else if e.kind == EntryKind::File {
            theme.text_style()
        } else {
            Style::default().fg(theme.accent)
        };
        rows.push(vec![(
            Span::styled(text, style),
            Some(HitTarget::BrowserRow(i)),
        )]);
    }
    let details = match &b.error {
        Some(e) => Span::styled(tail(e, iw), Style::default().fg(theme.accent)),
        None => Span::styled(head(&b.details, iw), theme.dim_style()),
    };
    rows.push(vec![(details, None)]);
    let mut br: Vec<Tagged> = Vec::new();
    for (k, btn) in BrowserButton::ALL.iter().enumerate() {
        if k > 0 {
            br.push((Span::raw(" "), None));
        }
        let sel = b.focus == BrowserFocus::Buttons && b.button == *btn;
        br.push((
            Span::styled(btn.label(b.show_all), theme.field_style(sel, false)),
            Some(HitTarget::BrowserButton(*btn)),
        ));
    }
    rows.push(br);

    let mut lines = Vec::new();
    for (y, r) in rows.iter().enumerate() {
        let yy = inner.y + y as u16;
        // A whole-row target under list rows, so a click past the text hits.
        if let Some((_, Some(t @ HitTarget::BrowserRow(_)))) = r.first() {
            hits.push(Rect::new(inner.x, yy, inner.width, 1), *t);
        }
        record_spans(hits, inner.x, yy, inner.width, r);
        lines.push(untag(r));
    }
    Paragraph::new(lines).render(inner, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp tree: sub/ (with x.wav), Beta/, .hidden/, a.wav (a real I/Q
    /// file), B.WAV, notes.txt, .secret.wav.
    fn tree(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("shellsdr-fb-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("Beta")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        let f = std::fs::File::create(root.join("a_7100000Hz.wav")).unwrap();
        let mut w = crate::record::wav::WavWriter::new(f, 2, 8_000).unwrap();
        w.write_f32(&vec![0.0; 16_000]).unwrap();
        w.finish("").unwrap();
        std::fs::write(root.join("B.WAV"), b"junk").unwrap();
        std::fs::write(root.join("notes.txt"), b"x").unwrap();
        std::fs::write(root.join(".secret.wav"), b"x").unwrap();
        std::fs::write(root.join("sub").join("x.wav"), b"x").unwrap();
        root.canonicalize().unwrap()
    }

    fn names(b: &FileBrowser) -> Vec<&str> {
        b.entries.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn lists_parent_folders_then_wav_files_sorted_without_hidden() {
        let root = tree("list");
        let b = FileBrowser::open(&root, None);
        assert_eq!(b.dir, root);
        assert_eq!(
            names(&b),
            vec!["..", "Beta", "sub", "a_7100000Hz.wav", "B.WAV"]
        );
        assert_eq!(b.cursor, 1, "starts on the first real entry");
    }

    #[test]
    fn filter_button_shows_all_files_and_keeps_the_selection() {
        let root = tree("filter");
        let mut b = FileBrowser::open(&root, Some("B.WAV"));
        assert_eq!(b.selected().unwrap().name, "B.WAV");
        assert_eq!(b.press(BrowserButton::Filter), Outcome::Stay);
        assert!(names(&b).contains(&"notes.txt"));
        assert_eq!(b.selected().unwrap().name, "B.WAV");
    }

    #[test]
    fn enter_folder_and_up_returns_to_it() {
        let root = tree("nav");
        let mut b = FileBrowser::open(&root, Some("sub"));
        assert_eq!(b.activate(), Outcome::Stay);
        assert_eq!(b.dir, root.join("sub"));
        assert_eq!(names(&b), vec!["..", "x.wav"]);
        b.first();
        assert_eq!(b.activate(), Outcome::Stay, "`..` goes up");
        assert_eq!(b.dir, root);
        assert_eq!(
            b.selected().unwrap().name,
            "sub",
            "cursor on the folder we left"
        );
    }

    #[test]
    fn choosing_a_file_returns_its_path() {
        let root = tree("choose");
        let mut b = FileBrowser::open(&root, Some("a_7100000Hz.wav"));
        assert_eq!(
            b.press(BrowserButton::Open),
            Outcome::Choose(root.join("a_7100000Hz.wav"))
        );
    }

    #[test]
    fn details_describe_the_selected_wav_or_folder() {
        let root = tree("details");
        let mut b = FileBrowser::open(&root, Some("a_7100000Hz.wav"));
        assert_eq!(b.details, "I/Q 8 kHz  7.100000 MHz  0:01.0");
        b.select_name("B.WAV");
        assert!(b.details.starts_with("Not playable"), "{}", b.details);
        b.select_name("sub");
        assert_eq!(b.details, "Folder, 1 item(s)");
    }

    #[test]
    fn type_ahead_cycles_through_matching_names() {
        let root = tree("jump");
        let mut b = FileBrowser::open(&root, None);
        b.jump_to('b');
        assert_eq!(b.selected().unwrap().name, "B.WAV");
        b.jump_to('b');
        assert_eq!(
            b.selected().unwrap().name,
            "Beta",
            "wraps to the next match"
        );
    }

    #[test]
    fn clicking_selects_then_opens() {
        let root = tree("click");
        let mut b = FileBrowser::open(&root, None);
        let sub = b.entries.iter().position(|e| e.name == "sub").unwrap();
        assert_eq!(b.click_row(sub), Outcome::Stay);
        assert_eq!(b.dir, root, "first click only selects");
        b.click_row(sub);
        assert_eq!(b.dir, root.join("sub"), "second click opens");
    }

    #[test]
    fn a_missing_folder_falls_back_to_an_existing_ancestor() {
        let root = tree("missing");
        let b = FileBrowser::open(&root.join("gone").join("deeper"), None);
        assert_eq!(b.dir, root);
    }

    #[test]
    fn path_and_cancel_buttons_hand_back_to_the_app() {
        let root = tree("buttons");
        let mut b = FileBrowser::open(&root, None);
        assert_eq!(
            b.press(BrowserButton::Path),
            Outcome::TypePath(root.clone())
        );
        assert_eq!(b.press(BrowserButton::Cancel), Outcome::Cancel);
        b.button = BrowserButton::Cancel;
        b.step_button(1);
        assert_eq!(b.button, BrowserButton::Open, "wraps");
    }

    #[test]
    fn the_scroll_window_follows_the_cursor() {
        let mut b = FileBrowser::open(&tree("scroll"), None);
        b.last();
        b.reconcile_scroll(2);
        assert_eq!(b.scroll, b.entries.len() - 2);
        b.first();
        b.reconcile_scroll(2);
        assert_eq!(b.scroll, 0);
    }

    #[test]
    fn size_labels() {
        assert_eq!(size_label(40_196_564), "40.2 MB");
        assert_eq!(size_label(512_000), "512 kB");
        assert_eq!(size_label(20), "20 B");
    }
}
