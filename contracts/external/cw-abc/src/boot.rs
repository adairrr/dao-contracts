use crate::msg::*;
use boot_core::ContractWrapper;
use boot_core::{contract, Contract, CwEnv};

#[contract(InstantiateMsg, ExecuteMsg, QueryMsg, Empty)]
pub struct CwAbc<Chain>;

impl<Chain: CwEnv> CwAbc<Chain> {
    pub fn new(name: &str, chain: Chain) -> Self {
        let mut contract = Contract::new(name, chain);
        contract = contract
            .with_wasm_path("abstract_etf_app")
            .with_mock(Box::new(
                ContractWrapper::new(
                    crate::contract::execute,
                    crate::contract::instantiate,
                    crate::contract::query,
                )
            ));
        Self(contract)
    }
}