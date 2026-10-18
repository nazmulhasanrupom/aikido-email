//! Reoon email verifier. Build map: "Reoon Email Verifier Build Map".
//!
//! - `client.rs`: the `EmailVerifier` trait, the real client, `FakeVerifier`
//! - `routes.rs`: `/v1/verify/reoon/*` (scope `integrations`)
//!
//! Rule: the key is in the query string of every Reoon call. No URL, no key and no email text goes to a log.
pub mod auto;
pub mod client;
pub mod gate;
pub mod routes;
pub mod settings;
pub mod worker;
