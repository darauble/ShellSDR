//! Module pages in the Work Area (spec 19): opening a module's page,
//! handing the Work Area back to the VFO, and the page's keys.

use crate::app::AppState;
use crate::ui::nav::Focus;
use crate::ui::page::WorkOwner;

impl AppState {
    /// Whether a module's page is on screen.
    pub fn page_shown(&self) -> bool {
        self.work_owner != WorkOwner::Vfo && self.work_slot_visible
    }

    /// The layout's answer for the page slot (each frame): the VFO view is
    /// visible only while the VFO owns the slot.
    pub fn set_work_slot_visible(&mut self, on: bool) {
        self.work_slot_visible = on;
        self.vfo_view_visible = on && self.work_owner == WorkOwner::Vfo;
    }

    /// Give the Work Area to `owner`'s page and focus it (`[Manager]`, `o`).
    /// In Tabs the page tab is shown; the Spectrum stays as it is in Split.
    pub fn open_page(&mut self, owner: WorkOwner) {
        if owner == WorkOwner::Vfo {
            return self.close_page();
        }
        self.work_owner = owner;
        self.select_work_tab(crate::config::WorkAreaTab::Vfo);
        // With the page tab selected the slot is on screen in both layouts.
        self.set_work_slot_visible(true);
        self.nav.set_work_area(true);
        self.nav.focus = Focus::WorkArea;
    }

    /// `o`: open the focused module's page, if it has one.
    pub fn open_focused_page(&mut self) {
        if let Some(o) = WorkOwner::of_focus(self.nav.focus) {
            self.open_page(o);
        }
    }

    /// `Esc` in a page: the Work Area goes back to the active VFO, the
    /// focus to the page's module.
    pub fn close_page(&mut self) {
        let back = self.work_owner.module();
        self.work_owner = WorkOwner::Vfo;
        self.set_work_slot_visible(self.work_slot_visible);
        self.nav.set_work_area(self.work_area_view_shown());
        if let Some(f) = back {
            self.nav.focus = f;
        }
    }

    /// A key for the focused page; true when it took it.
    pub fn page_key(&mut self, action: crate::ui::run::Action) -> bool {
        use crate::ui::run::Action;
        if action == Action::Escape {
            self.close_page();
            return true;
        }
        match self.work_owner {
            WorkOwner::Bookmarks => self.bookmark_page_key(action),
            WorkOwner::Schedule => self.schedule_page_key(action),
            WorkOwner::Report => self.report_page_key(action),
            WorkOwner::Vfo => false,
        }
    }

    /// The Schedule page's keys (spec 19.5).
    fn schedule_page_key(&mut self, action: crate::ui::run::Action) -> bool {
        use crate::schedule::module::PageField;
        use crate::ui::run::Action;
        let m = &mut self.schedule;
        match action {
            Action::Up => m.page_up(),
            Action::Down => m.page_down(),
            Action::Left => m.page_horizontal(-1),
            Action::Right => m.page_horizontal(1),
            Action::Enter => {
                let req = m.page_enter();
                self.schedule_request(req);
            }
            Action::Home | Action::End if m.page_field() == PageField::Table => {
                let n = m.jobs().len();
                if n > 0 {
                    m.click_page_row(if action == Action::Home { 0 } else { n - 1 });
                }
            }
            Action::Delete => {
                let req = m.remove_request();
                self.schedule_request(req);
            }
            _ => return false,
        }
        true
    }

    /// The Bookmarks page's keys (spec 19.4).
    fn bookmark_page_key(&mut self, action: crate::ui::run::Action) -> bool {
        use crate::bookmarks::manager::PageField;
        use crate::ui::run::Action;
        let bm = &mut self.bookmarks;
        match action {
            Action::Up => bm.page_up(),
            Action::Down => bm.page_down(),
            Action::Left => bm.page_horizontal(-1),
            Action::Right => bm.page_horizontal(1),
            Action::Enter => {
                let req = bm.page_enter();
                self.bookmark_request(req);
            }
            Action::Home | Action::End if bm.page_field() == PageField::Table => {
                let n = bm.view().len();
                if n > 0 {
                    bm.click_page_row(if action == Action::Home { 0 } else { n - 1 });
                }
            }
            Action::PageUp | Action::PageDown if bm.page_field() == PageField::Table => {
                let step = crate::bookmarks::manager::ITEM_ROWS as i32 - 1;
                bm.page_wheel(if action == Action::PageDown {
                    step
                } else {
                    -step
                });
            }
            Action::Delete => {
                let req = bm.remove_request();
                self.bookmark_request(req);
            }
            Action::Find => self.bookmark_page_fkey(7),
            _ => return false,
        }
        true
    }

    /// A function key while a page is on screen.
    pub fn page_fkey(&mut self, n: u8) {
        match self.work_owner {
            WorkOwner::Bookmarks => self.bookmark_page_fkey(n),
            WorkOwner::Schedule => self.schedule_page_fkey(n),
            WorkOwner::Report => self.report_page_fkey(n),
            WorkOwner::Vfo => {}
        }
    }

    /// The Schedule page's function keys (`schedule_page::FKEYS`).
    fn schedule_page_fkey(&mut self, n: u8) {
        use crate::schedule::module::SchedRequest;
        let m = &mut self.schedule;
        let req = match n {
            1 => {
                self.toggle_help();
                return;
            }
            2 => SchedRequest::Add,
            3 => m.edit_request(),
            4 => m.copy_request(),
            5 => {
                m.toggle_job();
                return;
            }
            6 => m.move_request(),
            8 => m.remove_request(),
            _ => return,
        };
        self.schedule_request(req);
    }

    /// The Bookmarks page's function keys (`bookmark_page::FKEYS`).
    fn bookmark_page_fkey(&mut self, n: u8) {
        use crate::bookmarks::manager::{PageField, Request};
        let bm = &mut self.bookmarks;
        let req = match n {
            1 => {
                self.toggle_help();
                return;
            }
            2 => Request::Add,
            3 => bm.edit_request(),
            4 => bm.copy_request(),
            5 => bm.move_request(),
            6 => bm.tune_request(),
            7 => {
                bm.select_page_field(PageField::Find);
                bm.begin_find();
                return;
            }
            8 => bm.remove_request(),
            _ => return,
        };
        self.bookmark_request(req);
    }
}

#[cfg(test)]
mod tests {
    use crate::app::AppState;
    use crate::config::{WorkAreaLayout, WorkAreaTab};
    use crate::ui::nav::Focus;
    use crate::ui::page::WorkOwner;
    use crate::ui::run::{Action, apply_action};

    fn state() -> AppState {
        let mut s = AppState::default();
        s.set_work_slot_visible(false); // Tabs, Spectrum tab
        s
    }

    #[test]
    fn o_opens_the_focused_modules_page_and_esc_gives_it_back() {
        let mut s = state();
        s.nav.focus = Focus::Bookmarks;
        apply_action(&mut s, Action::Char('o'));
        assert_eq!(s.work_owner, WorkOwner::Bookmarks);
        assert_eq!(s.work_area.tab, WorkAreaTab::Vfo);
        assert_eq!(s.nav.focus, Focus::WorkArea);
        assert!(s.page_shown());
        assert!(
            !s.vfo_view_visible,
            "the VFO view is not drawn under a page"
        );
        apply_action(&mut s, Action::Escape);
        assert_eq!(s.work_owner, WorkOwner::Vfo);
        assert_eq!(s.nav.focus, Focus::Bookmarks);
        assert!(s.vfo_view_visible, "the VFO view is back in its tab");
    }

    #[test]
    fn o_does_nothing_where_there_is_no_page() {
        let mut s = state();
        for f in [Focus::Source, Focus::Radio, Focus::Global] {
            s.nav.focus = f;
            apply_action(&mut s, Action::Char('o'));
            assert_eq!(s.work_owner, WorkOwner::Vfo, "{f:?}");
        }
    }

    #[test]
    fn a_page_in_split_keeps_the_spectrum_and_leaves_with_esc() {
        let mut s = state();
        s.work_area.layout = WorkAreaLayout::Split;
        s.nav.focus = Focus::Schedule;
        apply_action(&mut s, Action::Char('o'));
        assert_eq!(s.work_owner, WorkOwner::Schedule);
        assert_eq!(s.nav.focus, Focus::WorkArea);
        // Tab moves on and comes back to the page
        apply_action(&mut s, Action::Nav(crate::ui::nav::NavKey::Tab));
        assert_ne!(s.nav.focus, Focus::WorkArea);
        apply_action(&mut s, Action::Nav(crate::ui::nav::NavKey::BackTab));
        assert_eq!(s.nav.focus, Focus::WorkArea);
        apply_action(&mut s, Action::Escape);
        assert_eq!(s.nav.focus, Focus::Schedule);
        assert_eq!(s.work_owner, WorkOwner::Vfo);
    }

    #[test]
    fn the_spectrum_tab_hides_the_page_without_closing_it() {
        let mut s = state();
        s.nav.focus = Focus::Bookmarks;
        apply_action(&mut s, Action::Char('o'));
        apply_action(&mut s, Action::SwitchTab);
        s.set_work_slot_visible(false); // the next frame's layout
        assert!(!s.page_shown());
        assert_eq!(s.work_owner, WorkOwner::Bookmarks);
        apply_action(&mut s, Action::SwitchTab);
        s.set_work_slot_visible(true);
        assert!(s.page_shown());
    }

    #[test]
    fn schedule_page_copies_into_a_new_job_and_switches_jobs_off() {
        use crate::schedule::job::{BookmarkRef, Job};
        let mut s = state();
        s.schedule.save_job(
            None,
            Job {
                bookmark: BookmarkRef {
                    list: "L".into(),
                    name: "DSC".into(),
                },
                ..Job::default()
            },
        );
        s.nav.focus = Focus::Schedule;
        apply_action(&mut s, Action::Char('o'));
        apply_action(&mut s, Action::FKey(4));
        let ed = s.sched_editor.as_ref().expect("the editor on a copy");
        assert_eq!(ed.original, None, "Apply adds a new job");
        assert_eq!(ed.bookmark.as_ref().unwrap().name, "DSC");
        s.sched_editor = None;
        apply_action(&mut s, Action::FKey(5));
        assert!(!s.schedule.jobs()[0].enabled, "F5 switches it off");
        apply_action(&mut s, Action::FKey(5));
        assert!(s.schedule.jobs()[0].enabled, "and on");
    }
}
