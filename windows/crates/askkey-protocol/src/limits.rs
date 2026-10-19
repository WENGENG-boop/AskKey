use std::time::Duration;

/// Hard limits, matching the macOS edition's `BrokerLimits`.
pub struct Limits;

impl Limits {
    pub const MAX_FRAME_BYTES: usize = 64 * 1024;
    pub const MAX_FIELD_BYTES: usize = 4 * 1024;
    pub const MAX_COMMAND_ARGUMENTS: usize = 256;
    pub const MAX_CREDENTIALS_PER_RUN: usize = 16;
    pub const MAX_INHERITED_VARIABLES: usize = 64;
    pub const MAX_CONNECTIONS: usize = 16;
    pub const MAX_PENDING_APPROVALS: usize = 64;
    pub const MAX_RETAINED_OPERATIONS: usize = 256;
    pub const READ_DEADLINE: Duration = Duration::from_secs(2);
    pub const WRITE_DEADLINE: Duration = Duration::from_secs(2);
    /// Pending approvals expire after five minutes.
    pub const APPROVAL_LIFETIME: Duration = Duration::from_secs(5 * 60);
}
