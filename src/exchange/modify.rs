use either::Either;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{order::OrderRequest, ClientOrderRequest};

pub type Cloid = Uuid;
pub type OidOrCloid = Either<u64, Cloid>;

#[derive(Debug)]
pub struct ClientModifyRequest {
    pub oid: OidOrCloid,
    pub order: ClientOrderRequest,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ModifyRequest {
    #[serde(with = "oid_or_cloid")]
    pub oid: OidOrCloid,
    pub order: OrderRequest,
}

mod oid_or_cloid {
    use either::Either;
    use serde::{Deserializer, Serializer, de};
    use uuid::Uuid;

    use crate::helpers::uuid_to_hex_string;

    pub(super) fn serialize<S>(value: &Either<u64, Uuid>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Either::Left(oid) => serializer.serialize_u64(*oid),
            Either::Right(cloid) => serializer.serialize_str(&uuid_to_hex_string(*cloid)),
        }
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<Either<u64, Uuid>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Either<u64, Uuid>;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a u64 oid or a hex string cloid")
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(Either::Left(v))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                let hex = v.strip_prefix("0x").unwrap_or(v);
                Uuid::parse_str(hex).map(Either::Right).map_err(de::Error::custom)
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}
