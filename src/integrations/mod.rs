//! The app store: apps a workspace can install (Reoon first). One row per installed app in `integrations`,
//! with the key encrypted (AES-256-GCM, AAD = the row id). Routes: `/v1/integrations/apps*` (scope `integrations`).
//! Owner and admin sessions can install and uninstall; a member session is refused (`session::admin_only_for_sessions`).
pub mod routes;
pub mod store;
