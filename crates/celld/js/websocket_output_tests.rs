use super::*;

struct Socket {
    flush: Arc<WsFlushState>,
    registry: Arc<Mutex<WsRegistry>>,
    received: tokio::sync::mpsc::UnboundedReceiver<WsOut>,
}

impl Socket {
    fn new() -> Self {
        let registry = Arc::new(Mutex::new(WsRegistry::default()));
        let (sender, received) = tokio::sync::mpsc::unbounded_channel();
        registry.lock().unwrap().register(1, sender);
        Self {
            flush: Arc::new(WsFlushState::default()),
            registry,
            received,
        }
    }

    fn pending(&self, text: &str) -> WsFlushGuard {
        self.flush
            .emit_or_defer(&self.registry, 1, WsOut::Text(text.into()), true)
            .unwrap()
    }

    fn text(&mut self, expected: &str) {
        assert!(matches!(self.received.try_recv(), Ok(WsOut::Text(text)) if text == expected));
    }

    fn empty(&mut self) {
        assert!(matches!(
            self.received.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    fn failed(&mut self) {
        assert!(matches!(
            self.received.try_recv(),
            Ok(WsOut::Close(1011, _))
        ));
    }
}

#[test]
fn later_frame_keeps_its_own_durability_check() {
    let mut socket = Socket::new();
    let a = socket.pending("a");
    let b = socket.pending("b");
    a.complete(Ok(()));
    socket.text("a");
    socket.empty();
    b.complete(Ok(()));
    socket.text("b");
    socket.empty();
    assert!(socket.flush.flushes.lock().unwrap().is_empty());
}

#[test]
fn later_check_cannot_overtake_earlier_frame() {
    let mut socket = Socket::new();
    let a = socket.pending("a");
    let b = socket.pending("b");
    b.complete(Ok(()));
    socket.empty();
    a.complete(Ok(()));
    socket.text("a");
    socket.text("b");
    socket.empty();
}

#[test]
fn released_batch_waits_behind_pending_frames() {
    let mut socket = Socket::new();
    let a = socket.pending("a");
    ws_emit_ordered(
        &mut socket.flush.deferred.lock().unwrap(),
        &socket.registry,
        [(1, WsOut::Text("batch".into()))],
    );
    let b = socket.pending("b");
    a.complete(Ok(()));
    socket.text("a");
    socket.text("batch");
    socket.empty();
    b.complete(Ok(()));
    socket.text("b");
}

#[test]
fn failed_check_closes_and_discards_queued_and_future_frames() {
    let mut socket = Socket::new();
    let a = socket.pending("a");
    let b = socket.pending("b");
    b.complete(Err("unproven".into()));
    socket.failed();
    a.complete(Ok(()));
    assert!(socket
        .flush
        .emit_or_defer(&socket.registry, 1, WsOut::Text("later".into()), true,)
        .is_none());
    socket.empty();
    assert!(socket.flush.deferred.lock().unwrap().is_empty());
    assert!(socket.flush.flushes.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancelled_check_closes_and_releases_shutdown_waiters() {
    let mut socket = Socket::new();
    let a = socket.pending("a");
    let b = socket.pending("b");
    b.complete(Ok(()));
    drop(a);
    socket.failed();
    socket.empty();
    tokio::time::timeout(Duration::from_secs(1), async {
        socket.flush.await_flushes(1).await;
        socket.flush.await_all_flushes().await;
    })
    .await
    .unwrap();
}

#[test]
fn application_close_keeps_its_position_and_is_terminal() {
    let mut socket = Socket::new();
    let a = socket.pending("a");
    socket.flush.emit_or_defer(
        &socket.registry,
        1,
        WsOut::Close(1000, "done".into()),
        false,
    );
    let b = socket.pending("b");
    b.complete(Ok(()));
    socket.empty();
    a.complete(Ok(()));
    socket.text("a");
    assert!(matches!(
        socket.received.try_recv(),
        Ok(WsOut::Close(1000, _))
    ));
    socket.empty();
    assert!(socket.flush.deferred.lock().unwrap().is_empty());
}

#[test]
fn forced_close_prevents_late_completion_from_delivering_data() {
    let mut socket = Socket::new();
    let a = socket.pending("a");
    socket
        .registry
        .lock()
        .unwrap()
        .emit(1, WsOut::Close(1011, "reset".into()));
    socket.failed();
    a.complete(Ok(()));
    socket.empty();
    assert!(socket.flush.deferred.lock().unwrap().is_empty());
}
