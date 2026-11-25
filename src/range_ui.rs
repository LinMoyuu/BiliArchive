//! Session-only availability and choices; saved range preferences remain independent.
use crate::{collection, AppWindow};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct RangeState {
    pub total: Option<u64>,
    pub pending: bool,
    pending_batch: Option<u64>,
    pub manual_enabled: Option<bool>,
    pub from: String,
    pub to: String,
    preferred_from: String,
    preferred_to: String,
    auto_end: bool,
}
impl RangeState {
    fn new(saved: &collection::PageRange) -> Self {
        Self {
            total: None,
            pending: false,
            pending_batch: None,
            manual_enabled: None,
            from: String::new(),
            to: String::new(),
            preferred_from: saved.from.to_string(),
            preferred_to: if saved.to == 0 {
                String::new()
            } else {
                saved.to.to_string()
            },
            auto_end: saved.to == 0,
        }
    }
    pub fn available(&self) -> bool {
        self.total.is_some_and(|n| n > 0)
    }
    pub fn enabled(&self) -> bool {
        self.available() && self.manual_enabled.unwrap_or(true)
    }
    pub fn update_total(&mut self, total: Option<u64>) {
        self.pending = false;
        self.pending_batch = None;
        let Some(total) = total else {
            return;
        }; // A failed refresh cannot become an empty result.
        let was_available = self.available();
        self.total = Some(total);
        if total == 0 {
            self.from.clear();
            self.to.clear();
            return;
        }
        let pages = total.div_ceil(collection::PAGE_SIZE);
        if !was_available {
            self.from = self.preferred_from.clone();
            self.to = self.preferred_to.clone();
            if self.from.is_empty() {
                self.from = "1".into();
            }
        }
        if self.auto_end {
            self.to = pages.to_string();
        }
        (self.from, self.to) = collection::corrected_bounds(&self.from, &self.to, Some(pages));
    }
    pub fn choose(&mut self, enabled: bool) {
        if self.available() {
            self.manual_enabled = Some(enabled);
        }
    }
    pub fn edit(&mut self, from: &str, to: &str) {
        if !self.enabled() {
            return;
        }
        if to != self.to {
            self.auto_end = false;
        }
        (self.from, self.to) = collection::corrected_bounds(
            from,
            to,
            self.total.map(|n| n.div_ceil(collection::PAGE_SIZE)),
        );
        self.preferred_from = self.from.clone();
        self.preferred_to = self.to.clone();
    }
    pub fn all(&mut self) {
        if !self.enabled() {
            return;
        }
        self.from = "1".into();
        self.to = self
            .total
            .unwrap()
            .div_ceil(collection::PAGE_SIZE)
            .to_string();
        self.preferred_from = "1".into();
        self.preferred_to.clear();
        self.auto_end = true;
    }
    pub fn disabled_preference(&self) -> Option<collection::PageRange> {
        collection::parse(false, &self.preferred_from, &self.preferred_to).ok()
    }
    fn label(&self) -> String {
        if self.pending {
            "读取中…".into()
        } else {
            self.total
                .map(|n| format!("共 {} 页 / {} 条", n.div_ceil(collection::PAGE_SIZE), n))
                .unwrap_or_else(|| "未获取 · 点击重试".into())
        }
    }
}
pub struct Session {
    pub ranges: BTreeMap<u32, RangeState>,
}
impl Session {
    pub fn new(saved: &collection::Ranges) -> Self {
        Self {
            ranges: collection::TYPES
                .into_iter()
                .map(|ty| {
                    (
                        ty,
                        RangeState::new(&saved.get(&ty).cloned().unwrap_or_default()),
                    )
                })
                .collect(),
        }
    }
    pub fn begin(&mut self, batch: u64, types: &[u32]) {
        for ty in types {
            if let Some(r) = self.ranges.get_mut(ty) {
                r.pending = true;
                r.pending_batch = Some(batch);
            }
        }
    }
    pub fn finish(&mut self, batch: u64) {
        for r in self.ranges.values_mut() {
            if r.pending_batch == Some(batch) {
                r.pending = false;
                r.pending_batch = None;
            }
        }
    }
    pub fn complete(&mut self, batch: u64, ty: u32, total: Option<u64>) -> bool {
        let Some(r) = self.ranges.get_mut(&ty) else {
            return false;
        };
        if r.pending_batch != Some(batch) {
            return false;
        }
        r.update_total(total);
        true
    }
    pub fn reset(&mut self, saved: &collection::Ranges, preserve_loading: bool) {
        let loading: Vec<_> = self
            .ranges
            .iter()
            .filter_map(|(&ty, r)| r.pending_batch.map(|batch| (ty, batch)))
            .collect();
        *self = Self::new(saved);
        if preserve_loading {
            for (ty, batch) in loading {
                self.begin(batch, &[ty]);
            }
        }
    }
}
pub fn render(w: &AppWindow, session: &Session) {
    macro_rules! row {
        ($ty:expr,$enabled:ident,$available:ident,$from:ident,$to:ident,$total:ident,$limit:ident,$loading:ident) => {
            let r = &session.ranges[&$ty];
            w.$enabled(r.enabled());
            w.$loading(r.pending);
            w.$available(r.available());
            w.$from(r.from.clone().into());
            w.$to(r.to.clone().into());
            w.$total(r.label().into());
            w.$limit(
                r.total
                    .map(|n| n.div_ceil(collection::PAGE_SIZE).to_string())
                    .unwrap_or_default()
                    .into(),
            );
        };
    }
    row!(
        1,
        set_range_video_enabled,
        set_range_video_available,
        set_range_video_from,
        set_range_video_to,
        set_range_video_total,
        set_range_video_limit,
        set_range_video_loading
    );
    row!(
        11,
        set_range_dynamic_enabled,
        set_range_dynamic_available,
        set_range_dynamic_from,
        set_range_dynamic_to,
        set_range_dynamic_total,
        set_range_dynamic_limit,
        set_range_dynamic_loading
    );
    row!(
        17,
        set_range_repost_enabled,
        set_range_repost_available,
        set_range_repost_from,
        set_range_repost_to,
        set_range_repost_total,
        set_range_repost_limit,
        set_range_repost_loading
    );
    row!(
        12,
        set_range_article_enabled,
        set_range_article_available,
        set_range_article_from,
        set_range_article_to,
        set_range_article_total,
        set_range_article_limit,
        set_range_article_loading
    );
    row!(
        14,
        set_range_audio_enabled,
        set_range_audio_available,
        set_range_audio_from,
        set_range_audio_to,
        set_range_audio_total,
        set_range_audio_limit,
        set_range_audio_loading
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batch_loading_progresses_per_type_and_preserves_last_success() {
        let mut s = Session::new(&collection::defaults());
        assert!(s.ranges.values().all(|r| !r.pending));
        s.begin(1, &collection::TYPES);
        assert!(s.ranges.values().all(|r| r.label() == "读取中…"));
        assert!(s.complete(1, 1, Some(65)));
        assert_eq!(s.ranges[&1].label(), "共 7 页 / 65 条");
        assert!(s.ranges[&11].pending);
        assert!(s.complete(1, 11, None));
        assert_eq!(s.ranges[&11].label(), "未获取 · 点击重试");
        assert!(s.complete(1, 17, Some(0)));
        assert!(!s.ranges[&17].enabled());
        s.finish(1);
        assert!(s.ranges.values().all(|r| !r.pending));
        s.begin(2, &[1]);
        s.complete(2, 1, None);
        assert_eq!(s.ranges[&1].total, Some(65));
    }
    #[test]
    fn old_batches_cannot_complete_or_clear_new_loading_after_reset() {
        let saved = collection::defaults();
        let mut s = Session::new(&saved);
        s.begin(1, &collection::TYPES);
        s.reset(&saved, true);
        assert!(s.ranges.values().all(|r| r.pending));
        s.begin(2, &collection::TYPES);
        s.finish(1);
        assert!(!s.complete(1, 1, Some(999)));
        assert!(s.ranges[&1].pending);
        s.reset(&saved, false);
        s.begin(3, &collection::TYPES);
        s.finish(2);
        assert!(s.ranges.values().all(|r| r.pending));
        s.finish(3);
        assert!(s.ranges.values().all(|r| !r.pending));
    }
    #[test]
    fn detection_respects_choices_preferences_and_failures_for_every_type() {
        let mut saved = collection::defaults();
        for r in saved.values_mut() {
            r.from = 3;
            r.to = 8;
        }
        let mut s = Session::new(&saved);
        for ty in collection::TYPES {
            let r = s.ranges.get_mut(&ty).unwrap();
            assert!(!r.enabled());
            assert_eq!((&*r.from, &*r.to), ("", ""));
            r.update_total(None);
            assert!(!r.available());
            r.update_total(Some(65));
            assert!(r.enabled());
            assert_eq!((&*r.from, &*r.to), ("3", "7"));
            r.choose(false);
            r.update_total(Some(90));
            assert!(!r.enabled());
            r.choose(true);
            r.edit("2", "4");
            r.update_total(Some(100));
            assert_eq!((&*r.from, &*r.to), ("2", "4"));
            r.update_total(None);
            assert!(r.enabled());
            assert_eq!(r.total, Some(100));
            r.update_total(Some(0));
            assert!(!r.available());
            assert!(!r.enabled());
            assert!(r.from.is_empty() && r.to.is_empty());
            assert_eq!(
                r.disabled_preference().map(|p| (p.from, p.to)),
                Some((2, 4))
            );
            r.choose(true);
            r.all();
            r.edit("9", "9");
            assert!(r.from.is_empty());
            r.update_total(Some(100));
            assert_eq!((&*r.from, &*r.to), ("2", "4"));
        }
    }
    #[test]
    fn automatic_end_tracks_new_totals_and_manual_end_does_not() {
        let mut r = RangeState::new(&collection::PageRange::default());
        r.update_total(Some(65));
        assert_eq!((&*r.from, &*r.to), ("1", "7"));
        r.update_total(Some(85));
        assert_eq!(r.to, "9");
        r.edit("2", "3");
        r.update_total(Some(105));
        assert_eq!(r.to, "3");
        r.all();
        assert_eq!((&*r.from, &*r.to), ("1", "11"));
        r.update_total(Some(115));
        assert_eq!(r.to, "12");
    }
}
