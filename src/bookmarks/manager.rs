//! Bookmarks module state: the store plus the left-panel view (pure).
//!
//! Step 7. Owns the `BookmarkStore` and everything the left-panel Bookmarks
//! module navigates: the selected list, the field cursor, the button cursors,
//! the sort order, the find filter, the selected bookmark and the scroll
//! window of the bookmark rows. Actions that need a dialog or touch the radio
//! (pick a list, name a list, confirm a delete, open the editor, tune) are
//! returned as a `Request` for `AppState` to carry out, so this module stays
//! free of UI and radio concerns and is unit-tested directly.
//!
//! Panel field order (0.3.0, spec 19.4: one row each, then the bookmark
//! rows): `[Manager] [Quick Add]`, List, Find, items. The list and bookmark
//! management lives on the Manager page (its own field cursor, `PageField`):
//! the lists, the list buttons, strip rows, sort, find and the table.
//! Up/Down move between fields and through the rows; Left/Right act in the
//! field (cycle the list, move between buttons, change a value); Left and
//! Right on the bookmark rows switch to the previous/next list.

use serde::{Deserialize, Serialize};

use super::model::{Bookmark, BookmarkList, BookmarkStore};
use crate::ui::text_input::TextInput;

/// Bookmark rows shown at once in the left panel (the list scrolls).
pub const ITEM_ROWS: usize = 10;
/// Strip text rows: user-selectable 1..=10 (SDR++'s range).
pub const MIN_STRIP_ROWS: u16 = 1;
pub const MAX_STRIP_ROWS: u16 = 10;
/// Default strip rows.
pub const DEFAULT_STRIP_ROWS: u16 = 3;

/// The panel's fields, in navigation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookmarkField {
    /// `[Manager] [Quick Add]` (spec 19.4).
    Manager,
    List,
    Find,
    Items,
}

impl BookmarkField {
    pub const ALL: [BookmarkField; 4] = [
        BookmarkField::Manager,
        BookmarkField::List,
        BookmarkField::Find,
        BookmarkField::Items,
    ];

    /// Content row of the field inside the module (Items = first item row).
    pub fn row(self) -> usize {
        Self::ALL.iter().position(|f| *f == self).unwrap_or(0)
    }

    fn step(self, dir: i32) -> Self {
        step_in(&Self::ALL, self, dir)
    }
}

/// The Manager page's fields (spec 19.4), in navigation order: the lists
/// column, then the table's header row and the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageField {
    /// The list rows (the cursor is the selected list).
    Lists,
    ListButtons,
    StripRows,
    Sort,
    Find,
    /// The bookmark rows.
    Table,
}

impl PageField {
    pub const ALL: [PageField; 6] = [
        PageField::Lists,
        PageField::ListButtons,
        PageField::StripRows,
        PageField::Sort,
        PageField::Find,
        PageField::Table,
    ];

    fn step(self, dir: i32) -> Self {
        step_in(&Self::ALL, self, dir)
    }
}

/// The panel's buttons on its first row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelButton {
    /// Open the Manager page.
    Manager,
    /// The active VFO into the list shown, without the editor.
    QuickAdd,
}

impl PanelButton {
    pub const ALL: [PanelButton; 2] = [PanelButton::Manager, PanelButton::QuickAdd];

    pub fn label(self) -> &'static str {
        match self {
            PanelButton::Manager => "[Manager]",
            PanelButton::QuickAdd => "[Quick Add]",
        }
    }
}

/// Buttons acting on the selected list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListButton {
    New,
    Rename,
    Delete,
    /// Show/hide the list on the strip.
    Visibility,
}

impl ListButton {
    pub const ALL: [ListButton; 4] = [
        ListButton::New,
        ListButton::Rename,
        ListButton::Delete,
        ListButton::Visibility,
    ];

    /// Button label; the visibility button names the action it performs.
    pub fn label(self, shown: bool) -> &'static str {
        match self {
            ListButton::New => "[New]",
            ListButton::Rename => "[Rename]",
            ListButton::Delete => "[Del]",
            ListButton::Visibility if shown => "[Hide]",
            ListButton::Visibility => "[Show]",
        }
    }
}

/// Bookmark row order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SortKey {
    #[default]
    Frequency,
    Name,
}

impl SortKey {
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Frequency => "Frequency",
            SortKey::Name => "Name",
        }
    }

    fn toggled(self) -> Self {
        match self {
            SortKey::Frequency => SortKey::Name,
            SortKey::Name => SortKey::Frequency,
        }
    }
}

/// Something the manager cannot do alone; `AppState` carries it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    None,
    /// Open the module's page in the Work Area.
    OpenPage,
    /// Add the active VFO to the selected list without the editor.
    QuickAdd,
    /// Pick a list to copy / move bookmark `idx` of the selected list to.
    Copy(usize),
    Move(usize),
    /// Open the list picker.
    PickList,
    /// Ask for a new list's name.
    NewList,
    /// Ask for the selected list's new name.
    RenameList,
    /// Confirm deleting the selected list.
    DeleteList,
    /// Open the editor on a new bookmark captured from the active VFO.
    Add,
    /// Open the editor on bookmark `idx` of the selected list.
    Edit(usize),
    /// Confirm removing bookmark `idx` of the selected list.
    Remove(usize),
    /// Tune the active VFO to bookmark (`list`, `idx`).
    Tune(usize, usize),
}

/// Per-instance view settings persisted in the TOML config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BookmarkView {
    /// Name of the selected list.
    pub list: Option<String>,
    pub strip_rows: u16,
    pub sort: SortKey,
}

impl Default for BookmarkView {
    fn default() -> Self {
        Self {
            list: None,
            strip_rows: DEFAULT_STRIP_ROWS,
            sort: SortKey::default(),
        }
    }
}

/// The Bookmarks module state.
#[derive(Debug, Clone, PartialEq)]
pub struct BookmarkManager {
    pub store: BookmarkStore,
    /// Selected list (index into `store.lists`; meaningless when empty).
    list: usize,
    /// Selected bookmark (index into the selected list's bookmarks).
    selected: Option<usize>,
    field: BookmarkField,
    list_button: ListButton,
    panel_button: PanelButton,
    /// The Manager page's field cursor.
    page_field: PageField,
    pub sort: SortKey,
    pub filter: TextInput,
    filter_editing: bool,
    /// First visible bookmark row (index into `view()`).
    scroll: usize,
    pub strip_rows: u16,
    /// Set when the store changed and must be saved.
    dirty: bool,
}

impl Default for BookmarkManager {
    fn default() -> Self {
        Self::new(BookmarkStore::default())
    }
}

/// Validate a list or bookmark name: trimmed, non-empty.
fn clean_name(name: &str) -> Result<String, String> {
    let n = name.trim();
    if n.is_empty() {
        Err("Name must not be empty".to_string())
    } else {
        Ok(n.to_string())
    }
}

impl BookmarkManager {
    pub fn new(store: BookmarkStore) -> Self {
        let mut m = Self {
            store,
            list: 0,
            selected: None,
            field: BookmarkField::Items,
            list_button: ListButton::New,
            panel_button: PanelButton::Manager,
            page_field: PageField::Table,
            sort: SortKey::default(),
            filter: TextInput::default(),
            filter_editing: false,
            scroll: 0,
            strip_rows: DEFAULT_STRIP_ROWS,
            dirty: false,
        };
        m.select_first();
        m
    }

    // ── Accessors ────────────────────────────────────────────────────────

    pub fn field(&self) -> BookmarkField {
        self.field
    }

    pub fn list_button(&self) -> ListButton {
        self.list_button
    }

    pub fn panel_button(&self) -> PanelButton {
        self.panel_button
    }

    pub fn page_field(&self) -> PageField {
        self.page_field
    }

    pub fn is_filter_editing(&self) -> bool {
        self.filter_editing
    }

    /// Index of the selected list, if any list exists.
    pub fn list_index(&self) -> Option<usize> {
        (self.list < self.store.lists.len()).then_some(self.list)
    }

    pub fn current_list(&self) -> Option<&BookmarkList> {
        self.store.lists.get(self.list)
    }

    /// Index (in the selected list) of the selected bookmark.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn selected_bookmark(&self) -> Option<&Bookmark> {
        self.current_list()?.bookmarks.get(self.selected?)
    }

    /// The selected list's bookmark indices that pass the filter, sorted.
    pub fn view(&self) -> Vec<usize> {
        let Some(list) = self.current_list() else {
            return Vec::new();
        };
        let needle = self.filter.text().to_lowercase();
        let mut idx: Vec<usize> = list
            .bookmarks
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                needle.is_empty()
                    || b.name.to_lowercase().contains(&needle)
                    || b.notes.to_lowercase().contains(&needle)
                    || b.geoinfo.to_lowercase().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect();
        let bms = &list.bookmarks;
        // Cached keys: the view is rebuilt several times per frame, so avoid
        // lowercasing names on every comparison (Pi budget).
        match self.sort {
            SortKey::Frequency => {
                idx.sort_by_cached_key(|&i| (bms[i].frequency_hz, bms[i].name.to_lowercase()))
            }
            SortKey::Name => {
                idx.sort_by_cached_key(|&i| (bms[i].name.to_lowercase(), bms[i].frequency_hz))
            }
        }
        idx
    }

    /// Position of the selected bookmark within `view()`.
    pub fn cursor(&self) -> Option<usize> {
        let sel = self.selected?;
        self.view().iter().position(|&i| i == sel)
    }

    /// Rows the bookmark window occupies (at least one, for "(empty)").
    pub fn window_rows(&self) -> usize {
        self.view().len().clamp(1, ITEM_ROWS)
    }

    /// First visible view row, clamped to the content.
    pub fn scroll(&self) -> usize {
        let max = self.view().len().saturating_sub(ITEM_ROWS);
        self.scroll.min(max)
    }

    /// Every bookmark of every list shown on the strip, as (list, index).
    pub fn strip_entries(&self) -> Vec<(usize, usize, &Bookmark)> {
        self.store
            .lists
            .iter()
            .enumerate()
            .filter(|(_, l)| l.show_on_strip)
            .flat_map(|(li, l)| l.bookmarks.iter().enumerate().map(move |(i, b)| (li, i, b)))
            .collect()
    }

    /// Take (and clear) the "store changed, save me" flag.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    // ── Selection helpers ────────────────────────────────────────────────

    fn select_first(&mut self) {
        self.selected = self.view().first().copied();
        self.scroll = 0;
    }

    /// Select view row `pos` and scroll it into view.
    fn select_pos(&mut self, pos: usize) {
        let view = self.view();
        if let Some(&i) = view.get(pos) {
            self.selected = Some(i);
            self.ensure_visible(pos);
        }
    }

    fn ensure_visible(&mut self, pos: usize) {
        if pos < self.scroll {
            self.scroll = pos;
        } else if pos >= self.scroll + ITEM_ROWS {
            self.scroll = pos + 1 - ITEM_ROWS;
        }
    }

    /// Keep the selection valid after the view changed (filter, sort, edit):
    /// keep it when still visible, else take the first row.
    fn revalidate_selection(&mut self) {
        match self.cursor() {
            Some(pos) => self.ensure_visible(pos),
            None => self.select_first(),
        }
    }

    /// Select view row `pos` from a click (no field change).
    pub fn click_row(&mut self, pos: usize) {
        self.field = BookmarkField::Items;
        self.select_pos(pos);
    }

    /// A click on the page's table row `pos` (a position in `view()`).
    pub fn click_page_row(&mut self, pos: usize) {
        self.page_field = PageField::Table;
        self.select_pos(pos);
    }

    /// A click on the page's list row `i`.
    pub fn click_page_list(&mut self, i: usize) {
        self.page_field = PageField::Lists;
        self.select_list(i);
    }

    /// The wheel over the page's table: move the selection.
    pub fn page_wheel(&mut self, dir: i32) {
        let n = self.view().len();
        if n == 0 {
            return;
        }
        let pos = self.cursor().unwrap_or(0) as i32 + dir;
        self.select_pos(pos.clamp(0, n as i32 - 1) as usize);
    }

    /// Select bookmark `idx` of list `list` (e.g. clicked on the strip):
    /// switches the list, clears a filter that would hide it, scrolls to it.
    pub fn select_bookmark(&mut self, list: usize, idx: usize) {
        if list >= self.store.lists.len() || idx >= self.store.lists[list].bookmarks.len() {
            return;
        }
        if list != self.list {
            self.list = list;
            self.scroll = 0;
        }
        self.selected = Some(idx);
        if self.cursor().is_none() {
            self.filter.clear();
        }
        if let Some(pos) = self.cursor() {
            self.ensure_visible(pos);
        }
    }

    /// Scroll the bookmark window by `delta` rows without moving the
    /// selection (mouse wheel).
    pub fn scroll_view(&mut self, delta: i32) {
        let max = self.view().len().saturating_sub(ITEM_ROWS) as i32;
        self.scroll = (self.scroll() as i32 + delta).clamp(0, max.max(0)) as usize;
    }

    // ── Field navigation ─────────────────────────────────────────────────

    /// Put the field cursor on `field`.
    pub fn select_field(&mut self, field: BookmarkField) {
        self.field = field;
        if field == BookmarkField::Items && self.selected.is_none() {
            self.select_first();
        }
    }

    pub fn select_list_button(&mut self, b: ListButton) {
        self.page_field = PageField::ListButtons;
        self.list_button = b;
    }

    pub fn select_panel_button(&mut self, b: PanelButton) {
        self.field = BookmarkField::Manager;
        self.panel_button = b;
    }

    /// Put the page's field cursor on `f`.
    pub fn select_page_field(&mut self, f: PageField) {
        self.page_field = f;
        if f == PageField::Table && self.selected.is_none() {
            self.select_first();
        }
    }

    /// Up: previous bookmark row, or the previous field (leaving the rows
    /// from the first one goes to Find).
    pub fn up(&mut self) {
        if self.field == BookmarkField::Items
            && let Some(pos) = self.cursor()
            && pos > 0
        {
            self.select_pos(pos - 1);
            return;
        }
        self.select_field(self.field.step(-1));
    }

    /// Down: next bookmark row, or the next field (wrapping to List after
    /// the last row).
    pub fn down(&mut self) {
        if self.field == BookmarkField::Items {
            let n = self.view().len();
            match self.cursor() {
                Some(pos) if pos + 1 < n => {
                    self.select_pos(pos + 1);
                    return;
                }
                None if n > 0 => {
                    self.select_pos(0);
                    return;
                }
                _ => {}
            }
        }
        self.select_field(self.field.step(1));
    }

    /// Home/End on the bookmark rows.
    pub fn first(&mut self) {
        if self.field == BookmarkField::Items {
            self.select_pos(0);
        }
    }

    pub fn last(&mut self) {
        if self.field == BookmarkField::Items {
            let n = self.view().len();
            if n > 0 {
                self.select_pos(n - 1);
            }
        }
    }

    /// Page the bookmark rows by one window.
    pub fn page(&mut self, down: bool) {
        let n = self.view().len();
        if n == 0 {
            return;
        }
        let pos = self.cursor().unwrap_or(0);
        let step = ITEM_ROWS - 1;
        let new = if down {
            (pos + step).min(n - 1)
        } else {
            pos.saturating_sub(step)
        };
        self.select_pos(new);
    }

    /// Left/Right (`dir` = -1/+1) in the focused field.
    pub fn horizontal(&mut self, dir: i32) {
        match self.field {
            BookmarkField::List | BookmarkField::Items => self.cycle_list(dir),
            BookmarkField::Manager => {
                self.panel_button = step_in(&PanelButton::ALL, self.panel_button, dir);
            }
            BookmarkField::Find => {}
        }
    }

    /// ENTER on the focused field.
    pub fn enter(&mut self) -> Request {
        match self.field {
            BookmarkField::Manager => self.press_panel_button(self.panel_button),
            BookmarkField::List => Request::PickList,
            BookmarkField::Find => {
                self.begin_find();
                Request::None
            }
            BookmarkField::Items => self.tune_request(),
        }
    }

    pub fn press_list_button(&mut self, b: ListButton) -> Request {
        self.select_list_button(b);
        if self.list_index().is_none() && b != ListButton::New {
            return Request::None;
        }
        match b {
            ListButton::New => Request::NewList,
            ListButton::Rename => Request::RenameList,
            ListButton::Delete => Request::DeleteList,
            ListButton::Visibility => {
                self.toggle_visibility();
                Request::None
            }
        }
    }

    pub fn press_panel_button(&mut self, b: PanelButton) -> Request {
        self.select_panel_button(b);
        match b {
            PanelButton::Manager => Request::OpenPage,
            PanelButton::QuickAdd => Request::QuickAdd,
        }
    }

    // ── The Manager page (spec 19.4) ─────────────────────────────────────

    /// Up on the page: the previous list or bookmark row, else the previous
    /// field (wrapping).
    pub fn page_up(&mut self) {
        match self.page_field {
            PageField::Lists if self.list > 0 => return self.select_list(self.list - 1),
            PageField::Table => {
                if let Some(pos) = self.cursor()
                    && pos > 0
                {
                    return self.select_pos(pos - 1);
                }
            }
            _ => {}
        }
        let f = self.page_field.step(-1);
        self.select_page_field(f);
        if f == PageField::Lists && !self.store.lists.is_empty() {
            self.select_list(self.store.lists.len() - 1);
        }
        if f == PageField::Table {
            self.last();
        }
    }

    /// Down on the page: the next list or bookmark row, else the next field
    /// (wrapping).
    pub fn page_down(&mut self) {
        match self.page_field {
            PageField::Lists if self.list + 1 < self.store.lists.len() => {
                return self.select_list(self.list + 1);
            }
            PageField::Table => {
                let n = self.view().len();
                match self.cursor() {
                    Some(pos) if pos + 1 < n => return self.select_pos(pos + 1),
                    None if n > 0 => return self.select_pos(0),
                    _ => {}
                }
            }
            _ => {}
        }
        let f = self.page_field.step(1);
        self.select_page_field(f);
        if f == PageField::Lists && !self.store.lists.is_empty() {
            self.select_list(0);
        }
    }

    /// Left/Right on the page.
    pub fn page_horizontal(&mut self, dir: i32) {
        match self.page_field {
            PageField::Lists | PageField::Table => self.cycle_list(dir),
            PageField::ListButtons => {
                self.list_button = step_in(&ListButton::ALL, self.list_button, dir);
            }
            PageField::StripRows => self.adjust_rows(dir),
            PageField::Sort => self.toggle_sort(),
            PageField::Find => {}
        }
    }

    /// ENTER on the page's field.
    pub fn page_enter(&mut self) -> Request {
        match self.page_field {
            PageField::Lists => {
                self.select_page_field(PageField::Table);
                Request::None
            }
            PageField::ListButtons => self.press_list_button(self.list_button),
            PageField::StripRows => {
                self.strip_rows = if self.strip_rows >= MAX_STRIP_ROWS {
                    MIN_STRIP_ROWS
                } else {
                    self.strip_rows + 1
                };
                Request::None
            }
            PageField::Sort => {
                self.toggle_sort();
                Request::None
            }
            PageField::Find => {
                self.begin_find();
                Request::None
            }
            PageField::Table => self.tune_request(),
        }
    }

    /// Edit, copy, move or remove the selected bookmark (the page's F-keys).
    pub fn edit_request(&self) -> Request {
        self.selected.map_or(Request::None, Request::Edit)
    }

    pub fn copy_request(&self) -> Request {
        self.selected.map_or(Request::None, Request::Copy)
    }

    pub fn move_request(&self) -> Request {
        self.selected.map_or(Request::None, Request::Move)
    }

    /// Copy (`keep`) or move bookmark `idx` of the selected list into list
    /// `target`, renamed `Name (2)`... when the name is taken there. A move
    /// selects it in its new list; a copy keeps the selection.
    pub fn transfer(&mut self, idx: usize, target: usize, keep: bool) -> Result<(), String> {
        let Some(cur) = self.list_index() else {
            return Err("No list".to_string());
        };
        if target >= self.store.lists.len() {
            return Err("No such list".to_string());
        }
        let Some(mut bm) = self.store.lists[cur].bookmarks.get(idx).cloned() else {
            return Err("No such bookmark".to_string());
        };
        if target == cur && !keep {
            return Ok(());
        }
        bm.name = self.unique_bookmark_name(target, &bm.name);
        if keep {
            self.store.lists[target].bookmarks.push(bm);
            self.dirty = true;
            self.revalidate_selection();
            Ok(())
        } else {
            let name = bm.name.clone();
            self.save_bookmark(Some((cur, idx)), target, bm)
                .map(|()| debug_assert!(self.selected_bookmark().is_some_and(|b| b.name == name)))
        }
    }

    /// Tune request for the selected bookmark.
    pub fn tune_request(&self) -> Request {
        match (self.list_index(), self.selected) {
            (Some(l), Some(i)) => Request::Tune(l, i),
            _ => Request::None,
        }
    }

    /// Remove request for the selected bookmark (the page's Delete, F8).
    pub fn remove_request(&self) -> Request {
        self.selected.map_or(Request::None, Request::Remove)
    }

    pub fn adjust_rows(&mut self, dir: i32) {
        let r = self.strip_rows as i32 + dir;
        self.strip_rows = r.clamp(MIN_STRIP_ROWS as i32, MAX_STRIP_ROWS as i32) as u16;
    }

    pub fn toggle_sort(&mut self) {
        self.sort = self.sort.toggled();
        self.revalidate_selection();
    }

    // ── Find filter ──────────────────────────────────────────────────────

    /// Start typing into the find filter.
    pub fn begin_find(&mut self) {
        self.field = BookmarkField::Find;
        self.filter_editing = true;
        self.filter.select_all();
    }

    pub fn find_char(&mut self, c: char) {
        if self.filter.insert(c) {
            self.revalidate_selection();
        }
    }

    pub fn find_backspace(&mut self) {
        if self.filter.backspace() {
            self.revalidate_selection();
        }
    }

    pub fn find_delete(&mut self) {
        if self.filter.delete() {
            self.revalidate_selection();
        }
    }

    /// Finish typing. `keep` (ENTER / focus-out) keeps the filter and moves
    /// to the rows; otherwise (Esc) the filter is cleared.
    pub fn end_find(&mut self, keep: bool) {
        self.filter_editing = false;
        if keep {
            self.field = BookmarkField::Items;
        } else {
            self.filter.clear();
        }
        self.revalidate_selection();
    }

    // ── Lists ────────────────────────────────────────────────────────────

    /// Select list `i` (first bookmark selected, filter kept).
    pub fn select_list(&mut self, i: usize) {
        if i < self.store.lists.len() {
            self.list = i;
            self.select_first();
        }
    }

    pub fn cycle_list(&mut self, dir: i32) {
        let n = self.store.lists.len();
        if n > 0 {
            let i = ((self.list as i32 + dir) % n as i32 + n as i32) as usize % n;
            self.select_list(i);
        }
    }

    /// Create a list named `name` and select it.
    pub fn create_list(&mut self, name: &str) -> Result<(), String> {
        let name = clean_name(name)?;
        if self.store.list_index(&name).is_some() {
            return Err(format!("A list named \"{name}\" exists"));
        }
        self.store.lists.push(BookmarkList::new(name));
        self.select_list(self.store.lists.len() - 1);
        self.dirty = true;
        Ok(())
    }

    /// Rename the selected list.
    pub fn rename_list(&mut self, name: &str) -> Result<(), String> {
        let name = clean_name(name)?;
        let Some(cur) = self.list_index() else {
            return Err("No list selected".to_string());
        };
        if self.store.list_index(&name).is_some_and(|i| i != cur) {
            return Err(format!("A list named \"{name}\" exists"));
        }
        self.store.lists[cur].name = name;
        self.dirty = true;
        Ok(())
    }

    /// Delete the selected list (with its bookmarks).
    pub fn delete_list(&mut self) {
        if let Some(cur) = self.list_index() {
            self.store.lists.remove(cur);
            self.list = cur.min(self.store.lists.len().saturating_sub(1));
            self.select_first();
            self.dirty = true;
        }
    }

    /// Show/hide the selected list on the strip.
    pub fn toggle_visibility(&mut self) {
        if let Some(l) = self.store.lists.get_mut(self.list) {
            l.show_on_strip = !l.show_on_strip;
            self.dirty = true;
        }
    }

    /// The list names in order (for pickers).
    pub fn list_names(&self) -> Vec<String> {
        self.store.lists.iter().map(|l| l.name.clone()).collect()
    }

    // ── Bookmarks ────────────────────────────────────────────────────────

    /// Whether `name` is free in list `list` (ignoring bookmark `except`).
    fn name_free(&self, list: usize, name: &str, except: Option<usize>) -> bool {
        self.store.lists[list]
            .bookmarks
            .iter()
            .enumerate()
            .all(|(i, b)| b.name != name || Some(i) == except)
    }

    /// A bookmark name not yet used in list `list`.
    pub fn unique_bookmark_name(&self, list: usize, base: &str) -> String {
        super::model::unique_name(base, |n| {
            list < self.store.lists.len() && !self.name_free(list, n, None)
        })
    }

    /// Save bookmark `bm` into list `target`, replacing `original`
    /// (`(list, idx)` of the bookmark being edited) or adding a new one.
    /// The saved bookmark becomes the selection (switching to `target`).
    pub fn save_bookmark(
        &mut self,
        original: Option<(usize, usize)>,
        target: usize,
        mut bm: Bookmark,
    ) -> Result<(), String> {
        bm.name = clean_name(&bm.name)?;
        if target >= self.store.lists.len() {
            return Err("No such list".to_string());
        }
        let except = original.filter(|(l, _)| *l == target).map(|(_, i)| i);
        if !self.name_free(target, &bm.name, except) {
            return Err(format!("\"{}\" already exists in this list", bm.name));
        }
        let idx = match original {
            Some((l, i)) if l == target => {
                self.store.lists[l].bookmarks[i] = bm;
                i
            }
            Some((l, i)) => {
                self.store.lists[l].bookmarks.remove(i);
                self.store.lists[target].bookmarks.push(bm);
                self.store.lists[target].bookmarks.len() - 1
            }
            None => {
                self.store.lists[target].bookmarks.push(bm);
                self.store.lists[target].bookmarks.len() - 1
            }
        };
        self.dirty = true;
        self.select_bookmark(target, idx);
        Ok(())
    }

    /// Remove bookmark `idx` of the selected list; the selection moves to
    /// the row that took its place.
    pub fn remove_bookmark(&mut self, idx: usize) {
        let Some(cur) = self.list_index() else { return };
        if idx >= self.store.lists[cur].bookmarks.len() {
            return;
        }
        let pos = self.cursor();
        self.store.lists[cur].bookmarks.remove(idx);
        self.dirty = true;
        let view = self.view();
        self.selected = None;
        if !view.is_empty() {
            let p = pos.unwrap_or(0).min(view.len() - 1);
            self.select_pos(p);
        }
    }

    // ── Persistence ──────────────────────────────────────────────────────

    /// Replace the store with one reloaded from disk, keeping the selected
    /// list and bookmark by name where they still exist.
    pub fn replace_store(&mut self, store: BookmarkStore) {
        let list_name = self.current_list().map(|l| l.name.clone());
        let bm_name = self.selected_bookmark().map(|b| b.name.clone());
        self.store = store;
        self.list = list_name
            .and_then(|n| self.store.list_index(&n))
            .unwrap_or(0);
        self.selected = bm_name.and_then(|n| self.current_list()?.position(&n));
        self.revalidate_selection();
    }

    pub fn view_config(&self) -> BookmarkView {
        BookmarkView {
            list: self.current_list().map(|l| l.name.clone()),
            strip_rows: self.strip_rows,
            sort: self.sort,
        }
    }

    pub fn apply_view_config(&mut self, v: &BookmarkView) {
        self.strip_rows = v.strip_rows.clamp(MIN_STRIP_ROWS, MAX_STRIP_ROWS);
        self.sort = v.sort;
        if let Some(i) = v.list.as_deref().and_then(|n| self.store.list_index(n)) {
            self.list = i;
        }
        self.select_first();
    }
}

/// Step `cur` by `dir` within `all`, wrapping.
fn step_in<T: Copy + PartialEq>(all: &[T], cur: T, dir: i32) -> T {
    let n = all.len() as i32;
    let i = all.iter().position(|x| *x == cur).unwrap_or(0) as i32;
    all[((i + dir) % n + n) as usize % all.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfo::model::Mode;

    fn b(name: &str, khz: u64) -> Bookmark {
        Bookmark {
            name: name.into(),
            frequency_hz: khz * 1000,
            settings: crate::bookmarks::model::voice_part(Mode::Usb, None, None),
            ..Bookmark::default()
        }
    }

    /// Two lists: VOLMET (3 bookmarks, deliberately unsorted) and Empty.
    fn mgr() -> BookmarkManager {
        let mut volmet = BookmarkList::new("VOLMET");
        volmet.bookmarks = vec![b("Shannon", 8957), b("Rostov", 8939), b("Bangkok", 6676)];
        let store = BookmarkStore {
            lists: vec![volmet, BookmarkList::new("Empty")],
            ..BookmarkStore::default()
        };
        BookmarkManager::new(store)
    }

    fn names(m: &BookmarkManager) -> Vec<String> {
        let l = m.current_list().unwrap();
        m.view()
            .iter()
            .map(|&i| l.bookmarks[i].name.clone())
            .collect()
    }

    #[test]
    fn starts_on_the_rows_with_the_lowest_frequency_selected() {
        let m = mgr();
        assert_eq!(m.field(), BookmarkField::Items);
        assert_eq!(names(&m), ["Bangkok", "Rostov", "Shannon"]);
        assert_eq!(m.selected_bookmark().unwrap().name, "Bangkok");
    }

    #[test]
    fn sort_by_name_keeps_the_selection() {
        let mut m = mgr();
        m.down(); // Rostov
        m.toggle_sort();
        assert_eq!(m.sort, SortKey::Name);
        assert_eq!(names(&m), ["Bangkok", "Rostov", "Shannon"]);
        assert_eq!(m.selected_bookmark().unwrap().name, "Rostov");
    }

    #[test]
    fn down_walks_rows_then_wraps_to_the_list_field() {
        let mut m = mgr();
        m.down();
        m.down();
        assert_eq!(m.selected_bookmark().unwrap().name, "Shannon");
        m.down();
        assert_eq!(m.field(), BookmarkField::Manager, "past the last row");
    }

    #[test]
    fn up_from_the_first_row_goes_to_find_then_fields() {
        let mut m = mgr();
        m.up();
        assert_eq!(m.field(), BookmarkField::Find);
        m.up();
        assert_eq!(m.field(), BookmarkField::List);
        m.up();
        assert_eq!(m.field(), BookmarkField::Manager);
        m.down();
        m.down();
        m.down();
        assert_eq!(m.field(), BookmarkField::Items);
        assert_eq!(
            m.selected_bookmark().unwrap().name,
            "Bangkok",
            "re-entering the rows keeps the selection"
        );
    }

    #[test]
    fn left_right_on_rows_switch_lists() {
        let mut m = mgr();
        m.horizontal(1);
        assert_eq!(m.current_list().unwrap().name, "Empty");
        assert!(m.selected().is_none());
        assert_eq!(m.window_rows(), 1, "empty list still shows one row");
        m.horizontal(1);
        assert_eq!(m.current_list().unwrap().name, "VOLMET", "wraps");
    }

    #[test]
    fn enter_on_rows_requests_tuning_the_selection() {
        let mut m = mgr();
        m.down(); // Rostov = index 1 in the list
        assert_eq!(m.enter(), Request::Tune(0, 1));
    }

    #[test]
    fn find_filters_case_insensitively_and_esc_clears() {
        let mut m = mgr();
        m.begin_find();
        assert!(m.is_filter_editing());
        for c in "ROS".chars() {
            m.find_char(c);
        }
        assert_eq!(names(&m), ["Rostov"]);
        assert_eq!(
            m.selected_bookmark().unwrap().name,
            "Rostov",
            "selection follows the filter"
        );
        m.end_find(true);
        assert_eq!(m.field(), BookmarkField::Items, "ENTER goes to the rows");
        assert_eq!(m.filter.text(), "ROS", "kept as typed");
        m.begin_find();
        m.end_find(false);
        assert_eq!(m.filter.text(), "", "Esc clears");
        assert_eq!(names(&m).len(), 3);
    }

    #[test]
    fn find_matches_notes_and_geo_info() {
        let mut m = mgr();
        m.store.lists[0].bookmarks[0].notes = "Ireland H+25".into();
        m.begin_find();
        for c in "ireland".chars() {
            m.find_char(c);
        }
        assert_eq!(names(&m), ["Shannon"]);
    }

    #[test]
    fn list_buttons_move_with_left_right_and_request_dialogs() {
        let mut m = mgr();
        m.select_page_field(PageField::ListButtons);
        assert_eq!(m.page_enter(), Request::NewList);
        m.page_horizontal(1);
        assert_eq!(m.list_button(), ListButton::Rename);
        assert_eq!(m.page_enter(), Request::RenameList);
        m.page_horizontal(1);
        assert_eq!(m.page_enter(), Request::DeleteList);
        m.page_horizontal(1);
        assert_eq!(m.page_enter(), Request::None, "visibility toggles in place");
        assert!(!m.current_list().unwrap().show_on_strip);
        assert!(m.take_dirty(), "visibility change is saved");
        assert_eq!(ListButton::Visibility.label(false), "[Show]");
    }

    #[test]
    fn panel_buttons_and_page_keys_request_the_dialogs() {
        let mut m = mgr();
        assert_eq!(
            m.press_panel_button(PanelButton::Manager),
            Request::OpenPage
        );
        assert_eq!(
            m.press_panel_button(PanelButton::QuickAdd),
            Request::QuickAdd
        );
        m.select_field(BookmarkField::Manager);
        m.horizontal(-1);
        assert_eq!(m.panel_button(), PanelButton::Manager);
        // Bangkok is index 2 in the list.
        assert_eq!(m.edit_request(), Request::Edit(2));
        assert_eq!(m.copy_request(), Request::Copy(2));
        assert_eq!(m.move_request(), Request::Move(2));
        assert_eq!(m.remove_request(), Request::Remove(2));
        assert_eq!(m.tune_request(), Request::Tune(0, 2));
        m.select_list(1); // empty
        assert_eq!(m.edit_request(), Request::None);
        assert_eq!(m.remove_request(), Request::None);
    }

    #[test]
    fn the_page_walks_lists_buttons_settings_and_the_table() {
        let mut m = mgr();
        m.select_page_field(PageField::Lists);
        m.select_list(0);
        m.page_down();
        assert_eq!(m.current_list().unwrap().name, "Empty", "down the lists");
        m.page_down();
        assert_eq!(m.page_field(), PageField::ListButtons, "past the last list");
        m.page_down();
        m.page_down();
        m.page_down();
        assert_eq!(m.page_field(), PageField::Find);
        m.page_up();
        assert_eq!(m.page_field(), PageField::Sort);
        m.page_horizontal(1);
        assert_eq!(m.sort, SortKey::Name, "Left/Right flip the sort");
        m.select_list(0);
        m.select_page_field(PageField::Table);
        m.page_up();
        assert_eq!(m.page_field(), PageField::Find, "up from the first row");
        m.page_down();
        m.page_down();
        assert_eq!(m.selected_bookmark().unwrap().name, "Rostov");
        m.page_down();
        m.page_down();
        assert_eq!(
            m.page_field(),
            PageField::Lists,
            "the table wraps to the lists"
        );
        assert_eq!(m.list_index(), Some(0));
        m.select_page_field(PageField::Lists);
        assert_eq!(m.page_enter(), Request::None);
        assert_eq!(
            m.page_field(),
            PageField::Table,
            "ENTER on a list goes to its table"
        );
    }

    #[test]
    fn copy_and_move_rename_on_a_clash_and_move_follows_the_bookmark() {
        let mut m = mgr();
        // Bangkok (index 2) into "Empty" twice: the second is renamed.
        m.transfer(2, 1, true).unwrap();
        m.transfer(2, 1, true).unwrap();
        assert_eq!(m.store.lists[1].bookmarks[0].name, "Bangkok");
        assert_eq!(m.store.lists[1].bookmarks[1].name, "Bangkok (1)");
        assert_eq!(
            m.store.lists[0].bookmarks.len(),
            3,
            "a copy keeps the original"
        );
        assert_eq!(m.list_index(), Some(0), "a copy stays in its list");
        assert!(m.take_dirty());
        m.transfer(2, 1, false).unwrap();
        assert_eq!(m.store.lists[0].bookmarks.len(), 2, "a move removes it");
        assert_eq!(m.current_list().unwrap().name, "Empty", "and follows it");
        assert_eq!(m.selected_bookmark().unwrap().name, "Bangkok (2)");
    }

    #[test]
    fn strip_rows_clamp_with_arrows_and_wrap_with_enter() {
        let mut m = mgr();
        m.select_page_field(PageField::StripRows);
        m.strip_rows = MAX_STRIP_ROWS;
        m.page_horizontal(1);
        assert_eq!(m.strip_rows, MAX_STRIP_ROWS);
        m.page_enter();
        assert_eq!(m.strip_rows, MIN_STRIP_ROWS);
        m.page_horizontal(-1);
        assert_eq!(m.strip_rows, MIN_STRIP_ROWS);
    }

    #[test]
    fn create_rename_delete_lists_validate_names() {
        let mut m = mgr();
        assert!(m.create_list("  ").is_err());
        assert!(m.create_list("VOLMET").is_err(), "duplicate");
        m.create_list(" Laivyba ").unwrap();
        assert_eq!(
            m.current_list().unwrap().name,
            "Laivyba",
            "trimmed, selected"
        );
        assert!(m.rename_list("Empty").is_err(), "duplicate");
        m.rename_list("Laivyba").unwrap(); // same name is fine
        m.rename_list("Ships").unwrap();
        assert_eq!(m.list_names(), ["VOLMET", "Empty", "Ships"]);
        m.delete_list();
        assert_eq!(m.list_names(), ["VOLMET", "Empty"]);
        assert_eq!(m.current_list().unwrap().name, "Empty");
        assert!(m.take_dirty());
    }

    #[test]
    fn deleting_the_last_list_leaves_no_selection() {
        let mut m = BookmarkManager::default();
        m.delete_list();
        assert!(m.current_list().is_none());
        assert_eq!(m.press_list_button(ListButton::Rename), Request::None);
        assert_eq!(m.press_list_button(ListButton::New), Request::NewList);
        assert!(m.view().is_empty());
    }

    #[test]
    fn save_bookmark_adds_rejects_duplicates_and_selects() {
        let mut m = mgr();
        assert!(m.save_bookmark(None, 0, b("Rostov", 1)).is_err());
        m.save_bookmark(None, 0, b("Tashkent", 10090)).unwrap();
        assert_eq!(m.selected_bookmark().unwrap().name, "Tashkent");
        assert_eq!(m.cursor(), Some(3), "highest frequency sorts last");
        assert!(m.is_dirty());
    }

    #[test]
    fn save_bookmark_edits_in_place_or_moves_between_lists() {
        let mut m = mgr();
        // Edit Rostov (index 1) keeping its name: allowed.
        let mut r = m.store.lists[0].bookmarks[1].clone();
        r.frequency_hz = 5_000_000;
        m.save_bookmark(Some((0, 1)), 0, r).unwrap();
        assert_eq!(m.store.lists[0].bookmarks[1].frequency_hz, 5_000_000);
        // Rename onto another existing name: rejected.
        let r2 = b("Shannon", 1);
        assert!(m.save_bookmark(Some((0, 1)), 0, r2).is_err());
        // Move Rostov to "Empty".
        let r3 = m.store.lists[0].bookmarks[1].clone();
        m.save_bookmark(Some((0, 1)), 1, r3).unwrap();
        assert_eq!(m.store.lists[0].bookmarks.len(), 2);
        assert_eq!(m.current_list().unwrap().name, "Empty");
        assert_eq!(m.selected_bookmark().unwrap().name, "Rostov");
    }

    #[test]
    fn remove_bookmark_selects_the_row_that_took_its_place() {
        let mut m = mgr();
        m.down(); // Rostov (view pos 1)
        m.remove_bookmark(m.selected().unwrap());
        assert_eq!(names(&m), ["Bangkok", "Shannon"]);
        assert_eq!(m.selected_bookmark().unwrap().name, "Shannon");
        m.remove_bookmark(m.selected().unwrap());
        assert_eq!(m.selected_bookmark().unwrap().name, "Bangkok", "clamped");
    }

    #[test]
    fn select_bookmark_switches_list_and_clears_a_hiding_filter() {
        let mut m = mgr();
        m.create_list("Other").unwrap();
        m.store.lists[2].bookmarks.push(b("Kaliningrad", 9000));
        m.begin_find();
        m.find_char('z');
        m.end_find(true);
        m.select_bookmark(2, 0);
        assert_eq!(m.current_list().unwrap().name, "Other");
        assert_eq!(m.filter.text(), "", "filter hid the target");
        assert_eq!(m.selected_bookmark().unwrap().name, "Kaliningrad");
    }

    #[test]
    fn long_lists_scroll_to_keep_the_selection_visible() {
        let mut list = BookmarkList::new("Many");
        list.bookmarks = (0..25).map(|i| b(&format!("B{i:02}"), 1000 + i)).collect();
        let mut m = BookmarkManager::new(BookmarkStore {
            lists: vec![list],
            ..BookmarkStore::default()
        });
        assert_eq!(m.window_rows(), ITEM_ROWS);
        for _ in 0..12 {
            m.down();
        }
        assert_eq!(m.cursor(), Some(12));
        assert_eq!(m.scroll(), 12 + 1 - ITEM_ROWS);
        m.last();
        assert_eq!(m.cursor(), Some(24));
        assert_eq!(m.scroll(), 15);
        m.page(false);
        assert_eq!(m.cursor(), Some(24 - (ITEM_ROWS - 1)));
        m.first();
        assert_eq!(m.scroll(), 0);
        m.scroll_view(100);
        assert_eq!(m.scroll(), 15, "wheel scroll clamps to the content");
        assert_eq!(m.cursor(), Some(0), "wheel does not move the selection");
    }

    #[test]
    fn strip_entries_skip_hidden_lists() {
        let mut m = mgr();
        m.store.lists[1].bookmarks.push(b("Hidden", 1));
        m.store.lists[1].show_on_strip = false;
        let e = m.strip_entries();
        assert_eq!(e.len(), 3);
        assert!(e.iter().all(|(l, _, _)| *l == 0));
    }

    #[test]
    fn replace_store_keeps_list_and_bookmark_by_name() {
        let mut m = mgr();
        m.down(); // Rostov
        let mut reloaded = m.store.clone();
        reloaded.lists.reverse(); // VOLMET now index 1
        reloaded.lists[1].bookmarks.insert(0, b("Aaa", 1));
        m.replace_store(reloaded);
        assert_eq!(m.current_list().unwrap().name, "VOLMET");
        assert_eq!(m.selected_bookmark().unwrap().name, "Rostov");
    }

    #[test]
    fn view_config_round_trips_list_rows_and_sort() {
        let mut m = mgr();
        m.select_list(1);
        m.strip_rows = 5;
        m.toggle_sort();
        let v = m.view_config();
        let mut m2 = mgr();
        m2.apply_view_config(&v);
        assert_eq!(m2.current_list().unwrap().name, "Empty");
        assert_eq!(m2.strip_rows, 5);
        assert_eq!(m2.sort, SortKey::Name);
    }
}
