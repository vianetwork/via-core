use std::sync::Arc;

use via_fee_model::ViaGasAdjuster;

use crate::resource::Resource;

/// A resource that provides [`GasAdjuster`] to the service.
#[derive(Debug, Clone)]
pub struct ViaGasAdjusterResource(pub Arc<ViaGasAdjuster>);

impl Resource for ViaGasAdjusterResource {
    fn name() -> String {
        "common/via_gas_adjuster".into()
    }
}

impl From<Arc<ViaGasAdjuster>> for ViaGasAdjusterResource {
    fn from(gas_adjuster: Arc<ViaGasAdjuster>) -> Self {
        Self(gas_adjuster)
    }
}
