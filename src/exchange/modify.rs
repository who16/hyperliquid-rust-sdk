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
    pub oid: Either<u64, String>,
    pub order: OrderRequest,
}
