//! A single pending decoded picture. Compressed frames still decode in order so H.264
//! references remain valid; only presentation can skip obsolete pictures.
use std::sync::Mutex;

pub(crate) struct Mailbox<T>(Mutex<Option<T>>);
impl<T> Default for Mailbox<T> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}
impl<T> Mailbox<T> {
    /// True only when the consumer needs a wakeup. Replacements share the pending wakeup.
    pub(crate) fn put(&self, value: T) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(value)
            .is_none()
    }
    pub(crate) fn take(&self) -> Option<T> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_presenter_gets_latest_picture_with_one_wakeup() {
        let mailbox = Mailbox::default();
        assert!(mailbox.put(1));
        for frame in 2..=600 {
            assert!(!mailbox.put(frame));
        }
        assert_eq!(mailbox.take(), Some(600));
        assert_eq!(mailbox.take(), None);
        assert!(mailbox.put(601));
        assert_eq!(mailbox.take(), Some(601));
    }
}
