use std::sync::Arc;

use arc_swap::ArcSwapOption;

// Holds the session id the writer received from the server during the Ping
// handshake. It is shared (behind an `Arc`) between the writer client (which
// replays it in the `session` header on every data request) and the ping pool
// (which obtains it on the first Ping and echoes it back on every subsequent
// one). The id is issued once and kept for the whole lifetime of the process —
// it is never reset, so even after a server restart/GC the client keeps sending
// the same id and the server re-adopts it. `None` means we have no session yet
// (the first ping, or an old server that does not issue sessions) — in that
// case no header is sent.
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
}
