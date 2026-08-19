use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub cluster: String,
    pub id: String,
    pub state: String,
    pub name: String,
    pub elapsed: String,
    pub reason: String,
    pub ended: String,
    #[serde(default)]
    pub partition: String,
    #[serde(default)]
    pub start_time: String,
    #[serde(default)]
    pub priority: String,
    #[serde(default)]
    pub exit_code: String,
    #[serde(default)]
    pub max_rss: String,
    #[serde(default)]
    pub alloc_tres: String,
    #[serde(default)]
    pub interactive: bool,
}

impl Job {
    pub fn key(&self) -> String {
        let mut key = String::with_capacity(self.cluster.len() + self.id.len() + 1);
        self.write_key(&mut key);
        key
    }
    pub fn write_key(&self, key: &mut String) {
        key.clear();
        key.reserve(self.cluster.len() + self.id.len() + 1);
        key.push_str(&self.cluster);
        key.push(':');
        key.push_str(&self.id);
    }
    pub fn active(&self) -> bool {
        self.state.starts_with("PENDING") || self.state.starts_with("RUNNING")
    }
    pub fn pending(&self) -> bool {
        self.state.starts_with("PENDING")
    }
    pub fn running(&self) -> bool {
        self.state.starts_with("RUNNING")
    }
    pub fn failed(&self) -> bool {
        [
            "FAILED",
            "TIMEOUT",
            "OUT_OF_MEMORY",
            "NODE_FAIL",
            "CANCELLED",
        ]
        .iter()
        .any(|state| self.state.starts_with(state))
    }
    pub fn completed(&self) -> bool {
        self.state.starts_with("COMPLETED")
    }
    #[allow(dead_code)]
    pub fn pending_tag(&self) -> &str {
        if !self.pending() {
            return "";
        }
        let reason = self.reason.trim_matches(['(', ')']);
        if reason == "Resources" {
            "Resources"
        } else if reason == "Priority" {
            "Priority"
        } else if reason.starts_with("QOSMaxJobs")
            || reason.starts_with("QOSMaxSubmit")
            || reason == "JobArrayTaskLimit"
            || reason.starts_with("PartitionMaxJobs")
            || reason.starts_with("PartitionJob")
        {
            "Rate Limit"
        } else if reason.starts_with("QOS")
            || reason.starts_with("Assoc")
            || reason.starts_with("Partition")
            || reason.starts_with("Association")
        {
            "Quota Limit"
        } else if reason.starts_with("DependencyNeverSatisfied") {
            "Dep Failed"
        } else if reason.starts_with("Dependency") {
            "Dependency"
        } else if reason.starts_with("ReqNodeNotAvail")
            || reason == "NodeDown"
            || reason == "NodeDrain"
        {
            "Node Unavail"
        } else if reason == "BadConstraints" {
            "Bad Constraints"
        } else if reason == "BeginTime" {
            "Begin Time"
        } else if reason.starts_with("Reservation") {
            "Reservation"
        } else if reason.starts_with("Licenses") {
            "License"
        } else if reason.starts_with("JobHold") {
            "Held"
        } else if reason.is_empty() || reason == "None" {
            ""
        } else {
            reason
        }
    }

    pub fn state_display(&self) -> &str {
        self.state.as_str()
    }

    pub fn blocked_category(&self) -> bool {
        self.interactive || self.reason.contains("DependencyNeverSatisfied")
    }
    pub fn insight(&self) -> String {
        if self.pending() {
            let explanation = pending_explanation(&self.reason);
            let start = if self.start_time.is_empty()
                || self.start_time == "N/A"
                || self.start_time == "Unknown"
            {
                String::new()
            } else {
                format!(" · estimated start {}", self.start_time)
            };
            let priority = if self.priority.is_empty() {
                String::new()
            } else {
                format!(" · priority {}", self.priority)
            };
            format!("{explanation}{start}{priority}")
        } else if self.failed() {
            let mut parts = Vec::new();
            if !self.exit_code.is_empty() && self.exit_code != "0:0" {
                parts.push(format!("exit {}", self.exit_code));
            }
            if !self.max_rss.is_empty() {
                parts.push(format!("peak memory {}", self.max_rss));
            }
            parts.join(" · ")
        } else {
            String::new()
        }
    }
}

pub fn pending_explanation(reason: &str) -> String {
    let reason = reason.trim_matches(['(', ')']);
    let explanation = if reason == "Priority" {
        "waiting behind higher-priority jobs"
    } else if reason == "Resources" {
        "waiting for requested compute resources"
    } else if reason.starts_with("DependencyNeverSatisfied") {
        "dependency can never be satisfied"
    } else if reason.starts_with("Dependency") {
        "waiting for a dependency"
    } else if reason.starts_with("QOSMaxJobs") || reason.starts_with("QOSMaxSubmit") {
        "rate limit: user job limit for QOS"
    } else if reason.starts_with("QOSMax")
        || reason.starts_with("QOSMin")
        || reason.starts_with("QOSUsage")
        || reason.starts_with("QOSResource")
    {
        "quota limit: QOS resource limit reached"
    } else if reason.starts_with("QOSJob")
        || reason.starts_with("QOSGroupJobs")
        || reason.starts_with("QOSGroupSubmit")
    {
        "rate limit: QOS job limit reached"
    } else if reason.starts_with("QOSGroup") {
        "quota limit: QOS group limit reached"
    } else if reason.starts_with("AssocMaxJobs") || reason.starts_with("AssocMaxSubmit") {
        "rate limit: account job limit reached"
    } else if reason.starts_with("AssocMax") {
        "quota limit: account resource limit reached"
    } else if reason.starts_with("Association") {
        "quota limit: account limit reached"
    } else if reason == "JobArrayTaskLimit" {
        "rate limit: array task concurrency limit reached"
    } else if reason.starts_with("PartitionMaxJobs") || reason.starts_with("PartitionJob") {
        "rate limit: partition job limit reached"
    } else if reason.starts_with("Partition") {
        "quota limit: partition limit reached"
    } else if reason.starts_with("QOS") || reason.starts_with("Assoc") {
        "waiting on an account or QOS limit"
    } else if reason.starts_with("ReqNodeNotAvail") || reason == "NodeDown" || reason == "NodeDrain"
    {
        "requested node(s) unavailable or in maintenance"
    } else if reason == "BadConstraints" {
        "invalid or unsatisfied node constraints"
    } else if reason == "BeginTime" {
        "waiting for its requested begin time"
    } else if reason.starts_with("Reservation") {
        "waiting for a reservation"
    } else if reason.starts_with("Licenses") {
        "waiting for a license"
    } else if reason.starts_with("JobHold") {
        "held by administrator or user"
    } else if reason.is_empty() || reason == "None" {
        "pending"
    } else {
        reason
    };
    explanation.to_string()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default, Serialize, Deserialize)]
pub enum StateFilter {
    #[default]
    All,
    Running,
    Pending,
    Failed,
    Completed,
}

impl StateFilter {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Running,
            Self::Running => Self::Pending,
            Self::Pending => Self::Failed,
            Self::Failed => Self::Completed,
            Self::Completed => Self::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "ALL",
            Self::Running => "RUNNING",
            Self::Pending => "PENDING",
            Self::Failed => "FAILED",
            Self::Completed => "COMPLETED",
        }
    }

    pub fn notice(self) -> &'static str {
        match self {
            Self::All => {
                "State filter: ALL (f cycle: all · running · pending · failed · completed)"
            }
            Self::Running => {
                "State filter: RUNNING only (f cycle: all · running · pending · failed · completed)"
            }
            Self::Pending => {
                "State filter: PENDING only (f cycle: all · running · pending · failed · completed)"
            }
            Self::Failed => {
                "State filter: FAILED only (f cycle: all · running · pending · failed · completed)"
            }
            Self::Completed => {
                "State filter: COMPLETED only (f cycle: all · running · pending · failed · completed)"
            }
        }
    }

    pub fn matches(self, job: &Job) -> bool {
        match self {
            Self::All => true,
            Self::Running => job.running(),
            Self::Pending => job.pending(),
            Self::Failed => job.failed(),
            Self::Completed => job.completed(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pane {
    pub id: String,
    pub cluster: String,
    pub job_id: String,
}

pub fn valid_job_id(id: &str) -> bool {
    let mut parts = id.split('_');
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let Some(first) = parts.next() else {
        return false;
    };
    digits(first) && parts.next().is_none_or(digits) && parts.next().is_none()
}

/// Make scheduler and bank metadata safe to print in a terminal. Logs have a
/// richer MCP-specific sanitizer; UI metadata must never carry raw escape,
/// carriage-return, clipboard, or other control sequences into a terminal.
pub fn terminal_text(value: &str) -> String {
    if !value.bytes().any(|b| b < 0x20 || b == 0x7F) {
        return value.to_string();
    }
    let mut safe = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\x1b' => safe.push_str("\\x1b"),
            '\n' => safe.push_str("\\n"),
            '\r' => safe.push_str("\\r"),
            '\t' => safe.push_str("\\t"),
            value if value.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(safe, "\\u{{{:x}}}", u32::from(value));
            }
            value => safe.push(value),
        }
    }
    safe
}

/// Extract a whitespace-delimited key-value token from scheduler metadata (e.g. `JobId=123`).
pub fn token<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.split_whitespace()
        .find_map(|part| part.strip_prefix(prefix))
}

#[cfg(test)]
#[path = "model/tests.rs"]
mod tests;
