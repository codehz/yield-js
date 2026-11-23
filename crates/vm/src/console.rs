//! Owned console notifications. No guest handles cross the Printer boundary.

use serde::{Deserialize, Serialize};

use std::collections::BTreeMap;
use std::time::Duration;

use crate::JsString;

pub(crate) mod preview;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct ConsoleState {
    #[serde(with = "crate::snapshot::ordered_pairs")]
    pub(crate) counts: BTreeMap<JsString, f64>,
    #[serde(with = "crate::snapshot::ordered_pairs")]
    pub(crate) timers: BTreeMap<JsString, Duration>,
    pub(crate) groups: Vec<ConsoleGroup>,
}

impl ConsoleState {
    pub(crate) fn validate(&self, elapsed: Duration) -> Result<(), crate::VmError> {
        let invalid = |part| crate::VmError::InvalidSnapshot(format!("invalid console {part}"));
        if self.timers.values().any(|&start| start > elapsed) {
            return Err(invalid("timer start"));
        }
        if self
            .counts
            .values()
            .any(|&n| !n.is_finite() || n < 0.0 || n.fract() != 0.0)
        {
            return Err(invalid("count"));
        }
        if self.groups.iter().any(|group| {
            group.title.is_empty()
                || !group.title.iter().all(|arg| match arg {
                    ConsoleArg::Value(value) => preview::valid(&value.value),
                    ConsoleArg::Formatted(fragments) => {
                        fragments.iter().all(|fragment| match fragment {
                            ConsoleFragment::Value(value) => preview::valid(&value.value),
                            ConsoleFragment::Text(_) | ConsoleFragment::Style(_) => true,
                        })
                    }
                })
        }) {
            return Err(invalid("group preview"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConsoleEvent {
    pub level: ConsoleLevel,
    pub groups: Vec<ConsoleGroup>,
    pub payload: ConsolePayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleLevel {
    Assert,
    Clear,
    Count,
    CountReset,
    Debug,
    Dir,
    Dirxml,
    Error,
    Group,
    GroupCollapsed,
    Info,
    Log,
    TimeLog,
    TimeEnd,
    Trace,
    Warn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsoleGroup {
    pub title: Vec<ConsoleArg>,
    pub collapsed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConsolePayload {
    Args(Vec<ConsoleArg>),
    Table(ConsoleTable),
    Trace {
        args: Vec<ConsoleArg>,
        frames: Vec<ConsoleFrame>,
    },
    Dir {
        value: ConsolePreview,
        options: Option<ConsolePreview>,
    },
    Clear,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConsoleTable {
    pub columns: Vec<JsString>,
    pub rows: Vec<ConsoleRow>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConsoleRow {
    pub index: JsString,
    pub cells: Vec<Option<ConsoleValue>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleFrame {
    pub name: String,
    pub module: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConsoleArg {
    Value(ConsolePreview),
    Formatted(Vec<ConsoleFragment>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConsoleFragment {
    Text(JsString),
    Value(ConsolePreview),
    Style(JsString),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsolePreviewMode {
    Generic,
    Optimal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsolePreview {
    pub mode: ConsolePreviewMode,
    pub value: ConsoleValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConsoleValue {
    Undefined,
    Null,
    Bool(bool),
    Number(#[serde(with = "crate::value::number_serde")] f64),
    BigInt(String),
    String(JsString),
    Symbol(Option<JsString>),
    Object(ConsoleObject),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsoleObject {
    pub kind: String,
    pub length: Option<usize>,
    pub properties: Vec<(ConsoleKey, ConsoleProperty)>,
    pub circular: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConsoleKey {
    String(JsString),
    Symbol(Option<JsString>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ConsoleProperty {
    Data(ConsoleValue),
    Accessor,
}
