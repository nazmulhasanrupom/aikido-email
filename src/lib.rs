//! AIKIDO EMAIL sending engine.
//! `email_management` is the existing connector code (OAuth and IMAP/SMTP connect flows).
pub mod accounts;
pub mod admin;
pub mod auth;
pub mod campaigns;
pub mod config;
pub mod crypto;
pub mod db;
pub mod docs;
pub mod email_management;
pub mod error;
pub mod followups;
pub mod generator;
pub mod leads;
pub mod replies;
pub mod sending;
pub mod state;
