mod failover;
mod health;
mod local;
mod registry;
mod router;
mod traits;

pub use failover::{FailoverExecutor, RetryConfig};
pub use health::{HealthCheckHandle, HealthMonitor, HealthSnapshot, OriginHealthState};
pub use local::LocalOrigin;
pub use registry::OriginRegistry;
pub use router::{LatencyStats, Router};
pub use musicfs_core::OriginType;
pub use traits::{Origin, WatchCallback, WatchEvent, WatchHandle};
