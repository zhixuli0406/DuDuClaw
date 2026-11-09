//! Bounded root-to-fresh-gateway setup channel. This is not an employee RPC.
use serde::{Deserialize, Serialize};
use crate::{Error, ids::RequestId};

pub const SOCKET_NAME: &str = ".migration-bootstrap.sock";
pub const SETUP_PATH: &str = "/api/migration/setup";
pub const MAX_SETUP_MESSAGE: usize = 8192;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupRequest {
    pub version: u16,
    pub migration: RequestId,
}
impl SetupRequest {
    pub fn authorize(&self, migration: RequestId, peer_uid: u32) -> Result<(), Error> {
        if peer_uid != 0 || self.version != 1 || self.migration != migration { return Err(Error::Authentication); }
        Ok(())
    }
}
// Deliberately no Debug: the response carries a bearer secret.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupReply {
    pub version: u16,
    pub migration: RequestId,
    pub port: u16,
    pub token: Option<String>,
    pub already_claimed: bool,
}
impl SetupReply {
    pub fn link(self, migration: RequestId) -> Result<String, Error> {
        if self.version != 1 || self.migration != migration || self.port == 0 {
            return Err(Error::Authentication);
        }
        match (self.token, self.already_claimed) {
            (None, true) => Err(Error::Conflict),
            (Some(token), false) if token.len() == 40 && token.bytes().all(|byte| byte.is_ascii_alphanumeric()) =>
                Ok(format!("http://127.0.0.1:{}{}#{}", self.port, SETUP_PATH, token)),
            _ => Err(Error::Authentication),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reply() -> SetupReply { SetupReply { version: 1, migration: RequestId([1;16]), port: 18889, token: Some("x".repeat(40)), already_claimed: false } }
    #[test]
    fn setup_reply_is_bound_and_uses_fragment_not_query() {
        assert_eq!(reply().link(RequestId([1;16])).unwrap(), format!("http://127.0.0.1:18889/api/migration/setup#{}", "x".repeat(40)));
        assert!(reply().link(RequestId([2;16])).is_err());
        let mut value=reply();value.token.as_mut().unwrap().push('\n');assert!(value.link(RequestId([1;16])).is_err());
        let mut value=reply();value.port=0;assert!(value.link(RequestId([1;16])).is_err());
        let mut value=reply();value.version=2;assert!(value.link(RequestId([1;16])).is_err());
        let mut value=reply();value.token=None;value.already_claimed=true;
        assert_eq!(value.link(RequestId([1;16])), Err(Error::Conflict));
        let mut value=reply();value.already_claimed=true;
        assert_eq!(value.link(RequestId([1;16])), Err(Error::Authentication));
    }
    #[test]
    fn setup_request_requires_root_and_current_migration() {
        let request = SetupRequest { version: 1, migration: RequestId([1;16]) };
        assert!(request.authorize(RequestId([1;16]), 0).is_ok());
        assert!(request.authorize(RequestId([1;16]), 991).is_err());
        assert!(request.authorize(RequestId([2;16]), 0).is_err());
        let request = SetupRequest { version: 2, migration: RequestId([1;16]) };
        assert!(request.authorize(RequestId([1;16]), 0).is_err());
    }
    #[test]
    fn setup_protocol_rejects_extra_authority_fields() {
        let bytes=br#"{"version":1,"migration":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"force":true}"#;
        assert!(serde_json::from_slice::<SetupRequest>(bytes).is_err());
    }
}
