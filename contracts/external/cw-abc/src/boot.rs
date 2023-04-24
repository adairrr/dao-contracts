use crate::msg::*;
use boot_core::{ArtifactsDir, ContractWrapper, Daemon, Mock, TxHandler, Uploadable, WasmPath};
use boot_core::{contract, Contract, CwEnv};
use cosmwasm_std::Empty;

#[contract(InstantiateMsg, ExecuteMsg, QueryMsg, Empty)]
pub struct CwAbc<Chain>;

impl<Chain: CwEnv> CwAbc<Chain> {
    pub fn new(name: &str, chain: Chain) -> Self {
        let mut contract = Contract::new(name, chain);
        Self(contract)
    }
}

impl Uploadable<Mock> for CwAbc<Mock> {
    fn source(&self) -> <Mock as TxHandler>::ContractSource {
        let aoeu = ContractWrapper::new(
            crate::contract::execute,
            crate::contract::instantiate,
            crate::contract::query,
        );
        Box::new(aoeu)
    }
}

impl Uploadable<Daemon> for CwAbc<Daemon> {
    fn source(&self) -> <Daemon as TxHandler>::ContractSource {
        ArtifactsDir::env().unwrap().find_wasm_path("cw_abc").unwrap()
    }
}