//! Notification port: surfacing OS-level notifications.

/// A notification to present to the user.
#[derive(Debug, Clone)]
pub struct Notice {
    /// Short title.
    pub title: String,
    /// Body text.
    pub body: String,
}

/// Presents notifications via the host platform (desktop toast, Android
/// notification, or a no-op on headless servers).
pub trait NotificationSink: Send + Sync {
    /// Present a notice.
    fn notify(&self, notice: Notice);
}
