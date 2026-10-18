//! Campaigns: a sequence, a pool of sender accounts, a set of leads, and a daily limit of new leads.
//! Build map: "Lead Management Build Map", sections 10 and 11.
//!
//! The main idea: adding 5000 leads does not enroll 5000 at once. The feeder (`feeder.rs`) moves waiting
//! leads into the sequence a few each day. The sending engine still decides when mail goes out.
//!
//! - `model.rs`: rows and enums
//! - `routes.rs`: the API (scope `campaigns`)
//! - `feeder.rs`: the tick loop that enrolls the leads
pub mod company;
pub mod feeder;
pub mod model;
pub mod routes;
pub mod emails;
pub mod email_routes;
