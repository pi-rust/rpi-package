use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::core::Error;

pub trait Serializable: Send + Sync + std::fmt::Debug {
    fn serialize_value(&self, value: &Value) -> Result<Vec<u8>, Error>;
    fn deserialize_value(&self, data: &[u8]) -> Result<Value, Error>;
}

pub type SerializerRef = Arc<dyn Serializable>;

#[derive(Debug)]
pub struct JsonSerializer;

impl JsonSerializer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for JsonSerializer {
    fn default() -> Self {
        Self::new()
    }
}

impl Serializable for JsonSerializer {
    fn serialize_value(&self, value: &Value) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(value).map_err(|e| Error::SerializationError(e.to_string()))
    }

    fn deserialize_value(&self, data: &[u8]) -> Result<Value, Error> {
        serde_json::from_slice(data).map_err(|e| Error::DeserializationError(e.to_string()))
    }
}

pub fn default_serializer() -> SerializerRef {
    Arc::new(JsonSerializer)
}

pub fn serialize_with<T: Serialize + ?Sized>(
    serializer: &dyn Serializable,
    value: &T,
) -> Result<Vec<u8>, Error> {
    let value =
        serde_json::to_value(value).map_err(|e| Error::SerializationError(e.to_string()))?;
    serializer.serialize_value(&value)
}

pub fn deserialize_with<T: DeserializeOwned>(
    serializer: &dyn Serializable,
    data: &[u8],
) -> Result<T, Error> {
    let value = serializer.deserialize_value(data)?;
    serde_json::from_value(value).map_err(|e| Error::DeserializationError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Serialize, Deserialize, Debug, PartialEq)]
    struct TestData {
        name: String,
        value: i32,
    }

    #[test]
    fn test_json_serializer() {
        let serializer = JsonSerializer::new();
        let data = TestData {
            name: "test".to_string(),
            value: 42,
        };

        let serialized = serialize_with(&serializer, &data).unwrap();
        let deserialized: TestData = deserialize_with(&serializer, &serialized).unwrap();

        assert_eq!(data, deserialized);
    }

    #[test]
    fn test_default_serializer() {
        let serializer = default_serializer();
        let data = vec![1_u8, 2_u8, 3_u8];

        let serialized = serialize_with(serializer.as_ref(), &data).unwrap();
        let deserialized: Vec<u8> = deserialize_with(serializer.as_ref(), &serialized).unwrap();

        assert_eq!(data, deserialized);
    }
}
