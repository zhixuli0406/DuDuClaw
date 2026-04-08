use serde::{Deserialize, Serialize};

/// User role in the system — determines dashboard access scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    Admin,
    Manager,
    Employee,
}

impl UserRole {
    /// Returns the privilege level (higher = more privileges).
    pub fn level(self) -> u8 {
        match self {
            Self::Admin => 3,
            Self::Manager => 2,
            Self::Employee => 1,
        }
    }

    /// Returns `true` if this role has at least the privileges of `min`.
    pub fn at_least(self, min: Self) -> bool {
        self.level() >= min.level()
    }
}

impl std::fmt::Display for UserRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admin => write!(f, "admin"),
            Self::Manager => write!(f, "manager"),
            Self::Employee => write!(f, "employee"),
        }
    }
}

impl std::str::FromStr for UserRole {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "admin" => Ok(Self::Admin),
            "manager" => Ok(Self::Manager),
            "employee" => Ok(Self::Employee),
            _ => Err(format!("unknown role: {s}")),
        }
    }
}

/// User account status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UserStatus {
    Active,
    Suspended,
    Offboarded,
}

impl std::fmt::Display for UserStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Suspended => write!(f, "suspended"),
            Self::Offboarded => write!(f, "offboarded"),
        }
    }
}

impl std::str::FromStr for UserStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "active" => Ok(Self::Active),
            "suspended" => Ok(Self::Suspended),
            "offboarded" => Ok(Self::Offboarded),
            _ => Err(format!("unknown status: {s}")),
        }
    }
}

/// Agent access level for a bound user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessLevel {
    /// Full control — chat, train, modify SOUL.md, modify skills.
    Owner,
    /// Operational — chat and view memory, cannot modify SOUL/skills.
    Operator,
    /// Read-only — view status and memory only.
    Viewer,
}

impl AccessLevel {
    pub fn level(self) -> u8 {
        match self {
            Self::Owner => 3,
            Self::Operator => 2,
            Self::Viewer => 1,
        }
    }

    pub fn at_least(self, min: Self) -> bool {
        self.level() >= min.level()
    }
}

impl std::fmt::Display for AccessLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Owner => write!(f, "owner"),
            Self::Operator => write!(f, "operator"),
            Self::Viewer => write!(f, "viewer"),
        }
    }
}

impl std::str::FromStr for AccessLevel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "owner" => Ok(Self::Owner),
            "operator" => Ok(Self::Operator),
            "viewer" => Ok(Self::Viewer),
            _ => Err(format!("unknown access level: {s}")),
        }
    }
}

/// A user account record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub created_at: String,
    pub updated_at: String,
    pub last_login: Option<String>,
}

/// A binding between a user and an agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAgentBinding {
    pub user_id: String,
    pub agent_name: String,
    pub access_level: AccessLevel,
    pub bound_at: String,
}

/// An audit log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: i64,
    pub user_id: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub detail: Option<String>,
    pub ip: Option<String>,
    pub timestamp: String,
}
