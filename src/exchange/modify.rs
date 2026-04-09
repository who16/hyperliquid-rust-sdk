use either::Either;
use serde::{Deserialize, Serialize};

use super::{order::OrderRequest, ClientOrderRequest};

pub type Cloid = String;
pub type OidOrCloid = Either<u64, Cloid>;

#[derive(Debug)]
pub struct ClientModifyRequest {
    pub oid: OidOrCloid,
    pub order: ClientOrderRequest,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ModifyRequest {
    pub oid: OidOrCloid,
    pub order: OrderRequest,
}
