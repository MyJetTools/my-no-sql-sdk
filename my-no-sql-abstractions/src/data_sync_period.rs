#[derive(Debug, Clone, Copy)]
pub enum DataSynchronizationPeriod {
    Immediately,
    Sec1,
    Sec5,
    Sec15,
    Sec30,
    Min1,
    Asap,
}
