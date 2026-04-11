use std::sync::Mutex;

use anyhow::Result;
use tokio::sync::broadcast;
use walrus_rust::Walrus;

use crate::storage::models::Span;

const TOPIC: &str = "spans";
const CHANNEL_CAPACITY: usize = 4096;

pub struct SpanBus {
    wal: Mutex<Walrus>,
    tx: broadcast::Sender<String>,
}

impl SpanBus {
    pub fn new() -> Result<Self> {
        let wal = Walrus::new()?;
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        Ok(Self {
            wal: Mutex::new(wal),
            tx,
        })
    }

    pub fn publish(&self, spans: &[Span]) -> Result<()> {
        let json = serde_json::to_string(spans)?;
        {
            let wal = self.wal.lock().unwrap();
            wal.append_for_topic(TOPIC, json.as_bytes())?;
        }
        // Ignore send errors (no active subscribers)
        let _ = self.tx.send(json);
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<String> {
        self.tx.subscribe()
    }
}
