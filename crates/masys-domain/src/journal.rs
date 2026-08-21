#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Emergency,
    Alert,
    Critical,
    Error,
    Warning,
    Notice,
    Info,
    Debug,
}

/// One line from `SystemService::journal`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub timestamp_ms: u64,
    pub unit: Option<String>,
    pub priority: Priority,
    pub message: String,
}
