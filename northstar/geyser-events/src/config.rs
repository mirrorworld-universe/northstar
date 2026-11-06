use {
    serde::Deserialize,
    std::{
        io,
        net::{Ipv4Addr, SocketAddr},
        time::Duration,
    },
};

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Commitment {
    #[default]
    Confirmed,
    Finalized,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub libpath: Option<String>,
    pub name: Option<String>,
    pub listen_addr: SocketAddr,
    pub commitment: Commitment,
    pub replay_seconds: u64,
    pub unsubscribe_grace_seconds: u64,
    pub max_programs: usize,
    pub max_connections: usize,
    pub live_capacity: usize,
    pub max_events_per_program: usize,
    pub max_replay_bytes: usize,
    pub max_pending_slots: usize,
    pub max_pending_transactions: usize,
    pub max_pending_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            libpath: None,
            name: None,
            listen_addr: (Ipv4Addr::LOCALHOST, 9100).into(),
            commitment: Commitment::Confirmed,
            replay_seconds: 600,
            unsubscribe_grace_seconds: 30,
            max_programs: 1024,
            max_connections: 256,
            live_capacity: 1024,
            max_events_per_program: 10_000,
            max_replay_bytes: 64 * 1024 * 1024,
            max_pending_slots: 512,
            max_pending_transactions: 200_000,
            max_pending_bytes: 64 * 1024 * 1024,
        }
    }
}

impl Config {
    pub fn validate(&self) -> io::Result<()> {
        if self.replay_seconds == 0
            || self.max_programs == 0
            || self.max_connections == 0
            || self.live_capacity == 0
            || self.max_events_per_program == 0
            || self.max_replay_bytes == 0
            || self.max_pending_slots == 0
            || self.max_pending_transactions == 0
            || self.max_pending_bytes == 0
            || self.live_capacity > (1 << 20)
            || Duration::from_secs(self.replay_seconds).as_secs() > 86_400
            || self.unsubscribe_grace_seconds > 86_400
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid event plugin limits (retention/grace must not exceed one day)",
            ));
        }
        Ok(())
    }
}
