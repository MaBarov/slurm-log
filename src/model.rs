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
        "waiting for requested resources"
    } else if reason.starts_with("DependencyNeverSatisfied") {
        "dependency can never be satisfied"
    } else if reason.starts_with("Dependency") {
        "waiting for a dependency"
    } else if reason.starts_with("QOS") || reason.starts_with("Assoc") {
        "waiting on an account or QOS limit"
    } else if reason.starts_with("ReqNodeNotAvail") {
        "requested node is unavailable"
    } else if reason == "BeginTime" {
        "waiting for its requested begin time"
    } else if reason.starts_with("Reservation") {
        "waiting for a reservation"
    } else if reason.starts_with("Licenses") {
        "waiting for a license"
    } else if reason.is_empty() || reason == "None" {
        "pending"
    } else {
        reason
    };
    explanation.to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn job_ids() {
        assert!(valid_job_id("3202710"));
        assert!(valid_job_id("3202690_1"));
        assert!(!valid_job_id("3202690_1_2"));
        assert!(!valid_job_id("abc"));
        for invalid in ["", "_", "1_", "_1", "1-2", " 1", "1\n", "١٢٣"] {
            assert!(!valid_job_id(invalid), "accepted invalid ID {invalid:?}");
        }
        for valid in ["0", "00001", "1_0", "999999999999999999999999"] {
            assert!(valid_job_id(valid), "rejected valid ID {valid:?}");
        }
    }

    #[test]
    fn terminal_text_escapes_controls() {
        assert_eq!(
            terminal_text("name\x1b]52;c;bad\x07\r\n"),
            "name\\x1b]52;c;bad\\u{7}\\r\\n"
        );
    }

    #[test]
    fn state_classification_handles_slurm_suffixes() {
        for state in [
            "FAILED",
            "FAILED+",
            "TIMEOUT",
            "OUT_OF_MEMORY",
            "NODE_FAIL",
            "CANCELLED by 1",
        ] {
            assert!(
                Job {
                    state: state.into(),
                    ..Job::default()
                }
                .failed()
            );
        }
        assert!(
            Job {
                state: "RUNNING+".into(),
                ..Job::default()
            }
            .running()
        );
        assert!(
            Job {
                state: "PENDING".into(),
                ..Job::default()
            }
            .pending()
        );
        assert!(
            !Job {
                state: "COMPLETED".into(),
                ..Job::default()
            }
            .active()
        );
    }

    #[test]
    fn insights_explain_pending_and_failed_jobs() {
        let pending = Job {
            state: "PENDING".into(),
            reason: "Resources".into(),
            start_time: "2026-08-11T18:00:00".into(),
            priority: "1234".into(),
            ..Job::default()
        };
        let insight = pending.insight();
        assert!(insight.contains("waiting for requested resources"));
        assert!(insight.contains("estimated start"));
        assert!(insight.contains("priority 1234"));

        let failed = Job {
            state: "OUT_OF_MEMORY".into(),
            exit_code: "0:9".into(),
            max_rss: "63G".into(),
            ..Job::default()
        };
        assert_eq!(failed.insight(), "exit 0:9 · peak memory 63G");
    }

    #[test]
    fn insights_cover_every_scheduler_reason_and_empty_metadata() {
        let cases = [
            ("Priority", "higher-priority"),
            ("(DependencyNeverSatisfied,foo)", "can never"),
            ("Dependency", "a dependency"),
            ("QOSGrpCpuLimit", "account or QOS"),
            ("AssocMaxJobsLimit", "account or QOS"),
            ("ReqNodeNotAvail", "node is unavailable"),
            ("BeginTime", "begin time"),
            ("Reservation", "reservation"),
            ("Licenses", "license"),
            ("None", "pending"),
            ("", "pending"),
            ("UnusualReason", "UnusualReason"),
        ];
        for (reason, expected) in cases {
            let job = Job {
                state: "PENDING".into(),
                reason: reason.into(),
                start_time: "N/A".into(),
                ..Job::default()
            };
            assert!(job.insight().contains(expected), "reason={reason}");
        }
        let unknown = Job {
            state: "PENDING".into(),
            start_time: "Unknown".into(),
            ..Job::default()
        };
        assert_eq!(unknown.insight(), "pending");
    }

    #[test]
    fn job_helpers_cover_keys_blocking_and_empty_insights() {
        let mut job = Job {
            cluster: "alpha".into(),
            id: "42".into(),
            state: "COMPLETED".into(),
            ..Job::default()
        };
        assert_eq!(job.key(), "alpha:42");
        let mut reusable = String::from("old allocation");
        job.write_key(&mut reusable);
        assert_eq!(reusable, "alpha:42");
        assert_eq!(job.insight(), "");
        assert!(!job.blocked_category());
        job.interactive = true;
        assert!(job.blocked_category());

        job.state = "FAILED".into();
        job.exit_code = "0:0".into();
        assert_eq!(job.insight(), "");
        job.max_rss = "1G".into();
        assert_eq!(job.insight(), "peak memory 1G");
    }
}
