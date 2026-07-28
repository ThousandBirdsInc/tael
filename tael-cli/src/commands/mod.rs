pub mod alert;
pub mod anomalies;
#[cfg(not(windows))] // keystore management runs where the server runs
pub mod auth;
pub mod comment;
pub mod condition;
#[cfg(not(windows))] // reads the server's retention config on its host
pub mod config;
pub mod correlate;
pub mod diagnose;
pub mod eval;
pub mod experiment;
pub mod get;
pub mod ingest;
pub mod issue;
pub mod query;
pub mod reliability;
pub mod review;
pub mod score;
pub mod server;
pub mod services;
pub mod signal;
pub mod similar;
pub mod skill;
pub mod suite;
pub mod summarize;
pub mod topology;
pub mod watch;
