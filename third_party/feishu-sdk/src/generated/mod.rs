use std::collections::HashMap;
use std::sync::LazyLock;

use crate::core::request::AccessTokenType;

#[derive(Debug)]
pub struct Endpoint {
    pub operation_id: &'static str,
    pub service: &'static str,
    pub version: &'static str,
    pub resource: &'static str,
    pub method_name: &'static str,
    pub http_method: &'static str,
    pub path: &'static str,
    pub token_types: &'static [AccessTokenType],
}

mod endpoints;
pub mod ops;

pub use endpoints::ENDPOINTS;

static ENDPOINTS_BY_ID: LazyLock<HashMap<&'static str, &'static Endpoint>> = LazyLock::new(|| {
    let mut map = HashMap::with_capacity(ENDPOINTS.len());
    for endpoint in ENDPOINTS {
        map.insert(endpoint.operation_id, endpoint);
    }
    map
});

pub fn find_endpoint(operation_id: &str) -> Option<&'static Endpoint> {
    ENDPOINTS_BY_ID.get(operation_id).copied()
}

#[cfg(test)]
mod tests {
    use super::{ops, *};

    #[test]
    fn endpoint_lookup_works() {
        let endpoint = find_endpoint("im.v1.chat.create");
        assert!(endpoint.is_some());
        let endpoint = endpoint.expect("expected endpoint");
        assert_eq!(endpoint.http_method, "POST");
        assert_eq!(endpoint.path, "/open-apis/im/v1/chats");
    }

    #[test]
    fn endpoint_lookup_misses_unknown() {
        assert!(find_endpoint("unknown.v1.resource.call").is_none());
    }

    #[test]
    fn operation_constants_match_catalog() {
        assert_eq!(ops::im::v1::chat::CREATE, "im.v1.chat.create");
        assert_eq!(ops::ALL_OPERATION_IDS.len(), ENDPOINTS.len());
    }
}
