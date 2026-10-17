use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Owner,
    Admin,
    Member,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::Member => "member",
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "owner" => Some(Role::Owner),
            "admin" => Some(Role::Admin),
            "member" => Some(Role::Member),
            _ => None,
        }
    }
    /// Owner and admin manage keys, the team and the system defaults.
    pub fn is_admin(self) -> bool {
        matches!(self, Role::Owner | Role::Admin)
    }
}

/// A user row with the password hash. Never sent out.
#[derive(Clone, FromRow)]
pub struct UserRow {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub password_hash: String,
    pub role: String,
    pub status: String,
    pub preferences: serde_json::Value,
    pub failed_logins: i32,
    pub locked_until: Option<DateTime<Utc>>,
    pub last_login_at: Option<DateTime<Utc>>,
    pub password_changed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

pub const USER_COLS: &str = "id, email::text AS email, name, password_hash, role, status, preferences, failed_logins, locked_until, last_login_at, password_changed_at, created_at";

impl UserRow {
    pub fn role(&self) -> Role {
        Role::parse(&self.role).unwrap_or(Role::Member)
    }
    pub fn active(&self) -> bool {
        self.status == "active"
    }
    pub fn out(&self, now: DateTime<Utc>) -> UserOut {
        UserOut {
            id: self.id,
            email: self.email.clone(),
            name: self.name.clone(),
            role: self.role(),
            status: self.status.clone(),
            preferences: self.preferences.clone(),
            last_login_at: self.last_login_at,
            locked_until: self.locked_until.filter(|t| *t > now),
            created_at: self.created_at,
        }
    }
}

#[derive(Clone, Serialize, ToSchema)]
pub struct UserOut {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub role: Role,
    /// `active` or `disabled`.
    pub status: String,
    /// Theme, notification toggles, `last_seen_inbox_at`.
    #[schema(value_type = Object)]
    pub preferences: serde_json::Value,
    pub last_login_at: Option<DateTime<Utc>>,
    /// Set while the account is locked after too many wrong passwords.
    pub locked_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
