use std::sync::Arc;

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use once_cell::sync::OnceCell;

use crate::errors::{AppError, AppResult};

static METRICS_HANDLE: OnceCell<Arc<PrometheusHandle>> = OnceCell::new();

pub fn init() -> AppResult<Arc<PrometheusHandle>> {
    if let Some(handle) = METRICS_HANDLE.get() {
        return Ok(handle.clone());
    }

    let builder = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Prefix("ferrum_".to_string()),
            &[0.005, 0.02, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0],
        )
        .map_err(|e| AppError::Internal(format!("metrics buckets init failed: {e}")))?;

    let handle = builder
        .install_recorder()
        .map_err(|e| AppError::Internal(format!("metrics init failed: {e}")))?;

    let handle = Arc::new(handle);
    let _ = METRICS_HANDLE.set(handle.clone());
    Ok(handle)
}

pub fn gather() -> String {
    METRICS_HANDLE
        .get()
        .map(|h| h.render())
        .unwrap_or_else(|| "# metrics not initialized\n".to_string())
}
