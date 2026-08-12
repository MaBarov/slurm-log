use std::{
    collections::{HashMap, HashSet},
    env,
    io::{self, Write},
    time::{Duration, Instant},
};

use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};

use crate::{config::Config, model::Job, slurm, state::Ledger};

pub struct PickResult {
    pub jobs: Vec<Job>,
    pub show_log_warnings: bool,
}

#[derive(Clone)]
struct Row {
    name: String,
    job: Option<usize>,
    members: Vec<usize>,
    nested: bool,
    expanded: bool,
}

struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, cursor::Show);
    }
}

include!("ui/picker.rs");
include!("ui/rows.rs");
include!("ui/help.rs");
include!("ui/render.rs");

#[cfg(test)]
mod tests;
