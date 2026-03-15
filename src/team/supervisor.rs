use std::sync::Arc;

use parking_lot::RwLock;

#[derive(Clone, Default)]
pub struct SupervisorControls {
    paused: Arc<RwLock<bool>>,
    outbound_kill_switch: Arc<RwLock<bool>>,
}

impl SupervisorControls {
    pub fn set_paused(&self, paused: bool) {
        *self.paused.write() = paused;
    }

    pub fn is_paused(&self) -> bool {
        *self.paused.read()
    }

    pub fn set_outbound_kill_switch(&self, enabled: bool) {
        *self.outbound_kill_switch.write() = enabled;
    }

    pub fn outbound_kill_switch(&self) -> bool {
        *self.outbound_kill_switch.read()
    }
}
