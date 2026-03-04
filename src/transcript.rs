pub enum TranscriptEvent {
    Final {
        delta: String,
        accumulated: String,
    },
    Interim(String),
}
