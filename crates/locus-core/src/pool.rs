//! Open connections, shared by every tab that uses the same saved connection
//! (in any Space). A connection is opened on the first Run that needs it.
//! After a connection-level failure the slot is emptied, so the next Run
//! connects again instead of failing forever.

use crate::driver::AnyDriver;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// One saved connection's driver, opened lazily. The async mutex makes two
/// tabs that run at the same moment share one connect instead of racing.
pub type Slot = Arc<tokio::sync::Mutex<Option<Arc<AnyDriver>>>>;

pub struct Pool {
    rt: tokio::runtime::Handle,
    slots: HashMap<String, Slot>,
    /// Per connection: its sessions must be server-side read-only (locked Production).
    ro: HashMap<String, Arc<AtomicBool>>,
}

impl Pool {
    pub fn new(rt: tokio::runtime::Handle) -> Self {
        Self {
            rt,
            slots: HashMap::new(),
            ro: HashMap::new(),
        }
    }

    pub fn rt(&self) -> &tokio::runtime::Handle {
        &self.rt
    }

    /// The read-only flag a new connection reads when it finishes connecting.
    pub fn ro_flag(&mut self, conn_id: &str) -> Arc<AtomicBool> {
        self.ro.entry(conn_id.to_string()).or_default().clone()
    }

    /// Sets the flag and puts the open driver (or the one being connected) in
    /// the matching mode.
    pub fn set_read_only(&mut self, conn_id: &str, on: bool) {
        self.ro_flag(conn_id).store(on, Ordering::SeqCst);
        let slot = self.slot(conn_id);
        self.rt.spawn(async move {
            if let Some(d) = slot.lock().await.as_ref() {
                d.set_read_only(on);
            }
        });
    }

    pub fn slot(&mut self, conn_id: &str) -> Slot {
        self.slots.entry(conn_id.to_string()).or_default().clone()
    }

    /// Settings changed or the connection was deleted: the next Run opens a
    /// new driver. The old one stays alive 10 s so a KILL it sent can finish.
    pub fn retire(&mut self, conn_id: &str) {
        crate::tunnel::close(&self.rt, conn_id);
        if let Some(slot) = self.slots.remove(conn_id) {
            self.rt.spawn(async move {
                let old = slot.lock().await.take();
                tokio::time::sleep(Duration::from_secs(10)).await;
                drop(old);
            });
        }
    }

    /// Already-open driver, if any (bench mode, edit apply).
    pub fn open_driver(&self, conn_id: &str) -> Option<Arc<AnyDriver>> {
        self.slots
            .get(conn_id)
            .and_then(|s| s.try_lock().ok().and_then(|g| g.clone()))
    }
}

/// Empties `slot` if it still holds `driver` (another tab may have
/// reconnected already).
pub async fn invalidate(slot: &Slot, driver: &Arc<AnyDriver>) {
    let mut g = slot.lock().await;
    if g.as_ref().is_some_and(|d| Arc::ptr_eq(d, driver)) {
        *g = None;
    }
}
