//! Exact BPE tokenizers (`tiktoken-rs`), Effective Cost (`C_eff`) ledger,
//! SQLite `PrefixLedger` store, and Flight-Recorder audit logging for Aphelion.

pub mod audit;
pub mod bpe;
pub mod ledger;
pub mod prefix_store;
