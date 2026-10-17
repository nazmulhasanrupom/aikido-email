//! Web users: accounts, passwords, sessions, invites, first-boot setup.
//! `/v1/auth/*` needs no key. `/v1/users/*` needs a session.
pub mod model;
pub mod password;
pub mod rate;
pub mod repo;
pub mod routes;
pub mod session;
pub mod setup;
