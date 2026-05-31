use std::sync::Arc;

use arc_swap::ArcSwapOption;

// Holds the session id the writer received from the server during the Ping
// handshake. It is shared (behind an `Arc`) between the writer client (which
// replays it in the `session` header on every data request) and the ping pool
// (which both echoes it back on the next Ping and adopts whatever id the latest
// Ping response carries). `None` means we have no session yet (or the server is
// an old one that does not issue sessions) — in that case no header is sent.
pub struct WriterSession {
    session: ArcSwapOption<String>,
}

impl WriterSession {
    pub fn new() -> Self {
        Self {
            session: ArcSwapOption::empty(),
        }
    }

    pub fn get(&self) -> Option<Arc<String>> {
        self.session.load_full()
    }

    pub fn set(&self, session: String) {
        self.session.store(Some(Arc::new(session)));
    }

    pub fn clear(&self) {
        self.session.store(None);
    }
}
