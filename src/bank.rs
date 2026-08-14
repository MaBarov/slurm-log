use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, OpenOptions},
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, BufReader, BufWriter, IsTerminal, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    style::{Attribute, Print, SetAttribute},
    terminal::{self, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use serde::{Deserialize, Serialize};

use crate::{
    command::{
        output_with_timeout, remote_scheduler_command, ssh_with_input, text, text_with_input,
    },
    config::{ClusterConfig, Config, SbatchBankConfig},
    model::{Job, valid_job_id},
};

mod scan_limits;

const MAX_SCRIPTS: usize = 20_000;
const MAX_DEPTH: usize = 3;
const MAX_SCRIPT_BYTES: u64 = 4 * 1024 * 1024;
const BANK_SCAN_TIME_LIMIT: Duration = Duration::from_secs(3);
const BANK_CACHE_TTL: Duration = Duration::from_secs(30);
const MAX_BANK_CACHE_BYTES: u64 = 64 * 1024 * 1024;
const BANK_CACHE_SCHEMA: u8 = 2;

fn ignored_directory(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".hg"
            | ".svn"
            | ".cache"
            | ".venv"
            | "venv"
            | "env"
            | "node_modules"
            | "target"
            | "build"
            | "dist"
            | "__pycache__"
            | ".mypy_cache"
            | ".pytest_cache"
            | ".ruff_cache"
    )
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Script {
    pub bank: String,
    pub relative: PathBuf,
    pub name: String,
    pub directives: Vec<String>,
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub declared_results: Vec<String>,
    pub(crate) bytes: Vec<u8>,
}

#[cfg(test)]
fn scan(root: &Path) -> Result<(Vec<Script>, Vec<String>)> {
    scan_direct(root)
}

include!("bank/scan.rs");
include!("bank/catalog.rs");
include!("bank/submit.rs");
include!("bank/index.rs");
include!("bank/preflight.rs");
include!("bank/bundle.rs");
include!("bank/ui.rs");

#[cfg(test)]
#[path = "bank/tests/bundle.rs"]
mod bundle_tests;
#[cfg(test)]
#[path = "bank/tests/edge.rs"]
mod edge_tests;
#[cfg(test)]
mod tests;
