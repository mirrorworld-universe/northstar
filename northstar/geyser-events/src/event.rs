use {
    crate::config::Commitment,
    serde::Serialize,
    std::{fmt, str::FromStr},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EventId {
    pub slot: u64,
    pub tx_index: usize,
    pub log_index: usize,
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.slot, self.tx_index, self.log_index)
    }
}

impl FromStr for EventId {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split(':');
        let mut next = || {
            parts
                .next()
                .filter(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
                .ok_or("invalid event ID")
        };
        let id = Self {
            slot: next()?.parse().map_err(|_| "invalid event ID")?,
            tx_index: next()?.parse().map_err(|_| "invalid event ID")?,
            log_index: next()?.parse().map_err(|_| "invalid event ID")?,
        };
        if parts.next().is_some() {
            return Err("invalid event ID");
        }
        Ok(id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Log,
    Cpi,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProgramEvent {
    pub program_id: String,
    pub slot: u64,
    pub block_time: Option<i64>,
    pub signature: String,
    pub tx_index: usize,
    /// Transaction-wide event ordinal: log payloads first, then CPI payloads.
    pub log_index: usize,
    pub source: Source,
    pub commitment: Commitment,
    pub data: String,
}

impl ProgramEvent {
    pub fn id(&self) -> EventId {
        EventId {
            slot: self.slot,
            tx_index: self.tx_index,
            log_index: self.log_index,
        }
    }

    pub fn size(&self) -> usize {
        self.program_id
            .len()
            .saturating_add(self.signature.len())
            .saturating_add(self.data.len())
            .saturating_add(256)
    }
}

#[derive(Debug)]
pub enum StreamMessage {
    Event(ProgramEvent),
    Resync {
        program_id: String,
        last_available_id: Option<String>,
        reason: &'static str,
    },
}

impl StreamMessage {
    pub fn encode(&self) -> String {
        match self {
            Self::Event(event) => format!(
                "id: {}\nevent: program_event\ndata: {}\n\n",
                event.id(),
                serde_json::to_string(event).expect("event serialization")
            ),
            Self::Resync {
                program_id,
                last_available_id,
                reason,
            } => format!(
                "event: resync_required\ndata: {}\n\n",
                serde_json::json!({ "program_id": program_id, "last_available_id": last_available_id, "reason": reason })
            ),
        }
    }
}
