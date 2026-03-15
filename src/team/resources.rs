use std::sync::Arc;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::Serialize;
use sysinfo::System;
use tokio::time::{sleep, Duration};

#[derive(Debug, Clone, Serialize)]
pub struct ResourceSnapshot {
    pub cpu_cores: usize,
    pub cpu_usage_pct: f32,
    pub total_memory_mb: u64,
    pub used_memory_mb: u64,
    pub available_memory_mb: u64,
    pub memory_pressure_pct: f32,
    pub suggested_ephemeral_capacity: usize,
    pub captured_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct ResourceMonitor {
    snapshot: Arc<RwLock<ResourceSnapshot>>,
    targets: Arc<RwLock<(usize, usize)>>,
}

impl ResourceMonitor {
    pub fn new(persistent_pool: usize, requested_ephemeral_cap: usize) -> Self {
        let targets = Arc::new(RwLock::new((persistent_pool, requested_ephemeral_cap)));
        let initial = capture_snapshot(persistent_pool, requested_ephemeral_cap);
        let monitor = Self {
            snapshot: Arc::new(RwLock::new(initial)),
            targets,
        };

        if tokio::runtime::Handle::try_current().is_ok() {
            let state = monitor.snapshot.clone();
            let targets = monitor.targets.clone();
            tokio::spawn(async move {
                loop {
                    let (persistent_pool, requested_ephemeral_cap) = *targets.read();
                    let next = capture_snapshot(persistent_pool, requested_ephemeral_cap);
                    *state.write() = next;
                    sleep(Duration::from_secs(2)).await;
                }
            });
        }

        monitor
    }

    pub fn update_targets(&self, persistent_pool: usize, requested_ephemeral_cap: usize) {
        *self.targets.write() = (persistent_pool, requested_ephemeral_cap);
        *self.snapshot.write() = capture_snapshot(persistent_pool, requested_ephemeral_cap);
    }

    pub fn snapshot(&self) -> ResourceSnapshot {
        self.snapshot.read().clone()
    }
}

fn capture_snapshot(persistent_pool: usize, requested_ephemeral_cap: usize) -> ResourceSnapshot {
    let mut system = System::new_all();
    system.refresh_all();

    let cpu_cores = system.cpus().len().max(1);
    let cpu_usage_pct = if system.cpus().is_empty() {
        0.0
    } else {
        system.cpus().iter().map(|cpu| cpu.cpu_usage()).sum::<f32>() / system.cpus().len() as f32
    };

    let total_memory_mb = system.total_memory() / 1024 / 1024;
    let available_memory_mb = (system.available_memory().max(system.free_memory())) / 1024 / 1024;
    let used_memory_mb = (system.used_memory() / 1024 / 1024)
        .max(total_memory_mb.saturating_sub(available_memory_mb));
    let memory_pressure_pct = if total_memory_mb == 0 {
        0.0
    } else {
        (used_memory_mb as f32 / total_memory_mb as f32) * 100.0
    };

    let cpu_budget = cpu_cores
        .saturating_mul(2)
        .saturating_sub(persistent_pool)
        .max(1);
    let memory_budget = (available_memory_mb / 384).max(1) as usize;
    let pressure_limiter = if cpu_usage_pct >= 95.0 || available_memory_mb < 256 {
        0
    } else if cpu_usage_pct >= 88.0 || available_memory_mb < 384 {
        1
    } else if cpu_usage_pct >= 78.0 || available_memory_mb < 768 || memory_pressure_pct >= 92.0 {
        requested_ephemeral_cap.saturating_div(2).max(1)
    } else {
        requested_ephemeral_cap
    };

    let suggested_ephemeral_capacity = requested_ephemeral_cap
        .min(cpu_budget)
        .min(memory_budget)
        .min(pressure_limiter);

    ResourceSnapshot {
        cpu_cores,
        cpu_usage_pct,
        total_memory_mb,
        used_memory_mb,
        available_memory_mb,
        memory_pressure_pct,
        suggested_ephemeral_capacity,
        captured_at: Utc::now(),
    }
}
