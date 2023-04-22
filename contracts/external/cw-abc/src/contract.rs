#[cfg(not(feature = "library"))]
use cosmwasm_std::entry_point;
use cosmwasm_std::{
    coins, to_binary, BankMsg, Binary, Deps, DepsMut, Env, MessageInfo, Response,
    StdError, StdResult, Uint128,
};
use cw2::set_contract_version;
use token_bindings::{TokenFactoryMsg, TokenFactoryQuery, TokenMsg};

use crate::curves::DecimalPlaces;
use crate::error::ContractError;
use crate::msg::{CurveInfoResponse, ExecuteMsg, InstantiateMsg, QueryMsg};
use crate::state::{CurveState, CURVE_STATE, CURVE_TYPE, SUPPLY_DENOM, PHASE_CONFIG, PHASE};
use cw_utils::{must_pay, nonpayable};
use crate::abc::{CommonsPhase, CommonsPhaseConfig, CurveFn};

// version info for migration info
const CONTRACT_NAME: &str = "crates.io:cw20-abc";
const CONTRACT_VERSION: &str = env!("CARGO_PKG_VERSION");

// By default, the prefix for token factory tokens is "factory"
const DENOM_PREFIX: &str = "factory";

type CwAbcResult<T = Response<TokenFactoryMsg>> = Result<T, ContractError>;

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn instantiate(
    deps: DepsMut<TokenFactoryQuery>,
    env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> CwAbcResult {
    nonpayable(&info)?;
    set_contract_version(deps.storage, CONTRACT_NAME, CONTRACT_VERSION)?;

    msg.validate()?;

    let InstantiateMsg {
        supply,
        reserve,
        curve_type,
        phase_config,
    } = msg;

    // Create supply denom with metadata
    let create_supply_denom_msg = TokenMsg::CreateDenom {
        subdenom: supply.subdenom.clone(),
        metadata: Some(supply.metadata),
    };

    // TODO validate denom?

    // Save the denom
    SUPPLY_DENOM.save(
        deps.storage,
        &format!(
            "{}/{}/{}",
            DENOM_PREFIX,
            env.contract.address.into_string(),
            supply.subdenom
        ),
    )?;

    // Save the curve type and state
    let normalization_places = DecimalPlaces::new(supply.decimals, reserve.decimals);
    let curve_state = CurveState::new(reserve.denom, normalization_places);
    CURVE_STATE.save(deps.storage, &curve_state)?;
    CURVE_TYPE.save(deps.storage, &curve_type)?;

    PHASE_CONFIG.save(deps.storage, &phase_config)?;

    Ok(Response::default().add_message(create_supply_denom_msg))
}


#[cfg_attr(not(feature = "library"), entry_point)]
pub fn execute(
    deps: DepsMut<TokenFactoryQuery>,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> CwAbcResult {
    // default implementation stores curve info as enum, you can do something else in a derived
    // contract and just pass in your custom curve to do_execute
    let curve_type = CURVE_TYPE.load(deps.storage)?;
    let curve_fn = curve_type.to_curve_fn();
    do_execute(deps, env, info, msg, curve_fn)
}

/// We pull out logic here, so we can import this from another contract and set a different Curve.
/// This contacts sets a curve with an enum in InstantiateMsg and stored in state, but you may want
/// to use custom math not included - make this easily reusable
pub fn do_execute(
    deps: DepsMut<TokenFactoryQuery>,
    env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
    curve_fn: CurveFn,
) -> CwAbcResult {
    match msg {
        ExecuteMsg::Buy {} => execute_buy(deps, env, info, curve_fn),
        ExecuteMsg::Burn { amount } => Ok(execute_sell(deps, env, info, curve_fn, amount)?),
    }
}

pub fn execute_buy(
    deps: DepsMut<TokenFactoryQuery>,
    env: Env,
    info: MessageInfo,
    curve_fn: CurveFn,
) -> CwAbcResult {
    let mut curve_state = CURVE_STATE.load(deps.storage)?;

    let payment = must_pay(&info, &curve_state.reserve_denom)?;

    // Load the phase config and phase
    let phase_config = PHASE_CONFIG.load(deps.storage)?;
    let mut phase = PHASE.load(deps.storage)?;

    match phase {
        CommonsPhase::Hatch(ref mut hatch_phase) => {
            let hatch_config = &phase_config.hatch;

            // Check that the potential hatcher is allowlisted
            hatch_config.assert_allowlisted(&info.sender)?;
            // Add the sender to the list of hatchers
            hatch_phase.hatchers.insert(info.sender.clone());

            // reserve percentage gets sent to the reserve
            // TODO: WE LEFT OFF HERE

            // Finally, check if the initial_raise max has been met
            if curve_state.reserve + payment >= hatch_config.initial_raise.1 {
                // Transition to the Open phase, the hatchers tokens are now vesting
                phase = CommonsPhase::Open;
                PHASE.save(deps.storage, &phase)?;
            }
        }
        // CommonsPhase::Vesting => {
        //     // Check if the vesting period has ended
        //     if env.block.time > phase_config.vesting.vesting_period {
        //         // Transition to the Open phase
        //         phase = CommonsPhase::Open;
        //         PHASE.save(deps.storage, &phase)?;
        //     }
        // }
        CommonsPhase::Open => {
            // TODO: what to do here?
            // Do nothing
        }
        CommonsPhase::Closed => {
            // Do nothing
        }
    }

    // calculate how many tokens can be purchased with this and mint them
    let curve = curve_fn(curve_state.clone().decimals);
    curve_state.reserve += payment;
    let new_supply = curve.supply(curve_state.reserve);
    let minted = new_supply
        .checked_sub(curve_state.supply)
        .map_err(StdError::overflow)?;
    curve_state.supply = new_supply;
    CURVE_STATE.save(deps.storage, &curve_state)?;

    let denom = SUPPLY_DENOM.load(deps.storage)?;
    // mint supply token
    let mint_msg = TokenMsg::MintTokens {
        denom,
        amount: minted,
        mint_to_address: info.sender.to_string(),
    };

    Ok(Response::new()
        .add_message(mint_msg)
        .add_attribute("action", "buy")
        .add_attribute("from", info.sender)
        .add_attribute("reserve", payment)
        .add_attribute("supply", minted))
}

pub fn execute_sell(
    deps: DepsMut<TokenFactoryQuery>,
    _env: Env,
    info: MessageInfo,
    curve_fn: CurveFn,
    amount: Uint128,
) -> CwAbcResult {
    let receiver = info.sender.clone();

    let denom = SUPPLY_DENOM.load(deps.storage)?;
    let payment = must_pay(&info, &denom)?;

    // calculate how many tokens can be purchased with this and mint them
    let mut state = CURVE_STATE.load(deps.storage)?;
    let curve = curve_fn(state.clone().decimals);
    state.supply = state
        .supply
        .checked_sub(amount)
        .map_err(StdError::overflow)?;
    let new_reserve = curve.reserve(state.supply);
    let released = state
        .reserve
        .checked_sub(new_reserve)
        .map_err(StdError::overflow)?;
    state.reserve = new_reserve;
    CURVE_STATE.save(deps.storage, &state)?;

    // Burn the tokens
    let burn_msg = TokenMsg::BurnTokens {
        denom,
        amount: payment,
        burn_from_address: info.sender.to_string(),
    };

    // now send the tokens to the sender (TODO: for sell_from we do something else, right???)
    let msg = BankMsg::Send {
        to_address: receiver.to_string(),
        amount: coins(released.u128(), state.reserve_denom),
    };

    Ok(Response::new()
        .add_message(msg)
        .add_message(burn_msg)
        .add_attribute("action", "burn")
        .add_attribute("from", info.sender)
        .add_attribute("supply", amount)
        .add_attribute("reserve", released))
}

#[cfg_attr(not(feature = "library"), entry_point)]
pub fn query(deps: Deps<TokenFactoryQuery>, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    // default implementation stores curve info as enum, you can do something else in a derived
    // contract and just pass in your custom curve to do_execute
    let curve_type = CURVE_TYPE.load(deps.storage)?;
    let curve_fn = curve_type.to_curve_fn();
    do_query(deps, env, msg, curve_fn)
}

/// We pull out logic here, so we can import this from another contract and set a different Curve.
/// This contacts sets a curve with an enum in InstantitateMsg and stored in state, but you may want
/// to use custom math not included - make this easily reusable
pub fn do_query(
    deps: Deps<TokenFactoryQuery>,
    _env: Env,
    msg: QueryMsg,
    curve_fn: CurveFn,
) -> StdResult<Binary> {
    match msg {
        // custom queries
        QueryMsg::CurveInfo {} => to_binary(&query_curve_info(deps, curve_fn)?),
        // QueryMsg::GetDenom {
        //     creator_address,
        //     subdenom,
        // } => to_binary(&get_denom(deps, creator_address, subdenom)),
    }
}

pub fn query_curve_info(
    deps: Deps<TokenFactoryQuery>,
    curve_fn: CurveFn,
) -> StdResult<CurveInfoResponse> {
    let CurveState {
        reserve,
        supply,
        reserve_denom,
        decimals,
    } = CURVE_STATE.load(deps.storage)?;

    // This we can get from the local digits stored in instantiate
    let curve = curve_fn(decimals);
    let spot_price = curve.spot_price(supply);

    Ok(CurveInfoResponse {
        reserve,
        supply,
        spot_price,
        reserve_denom,
    })
}

// // TODO, maybe we don't need this
// pub fn get_denom(
//     deps: Deps<TokenFactoryQuery>,
//     creator_addr: String,
//     subdenom: String,
// ) -> GetDenomResponse {
//     let querier = TokenQuerier::new(&deps.querier);
//     let response = querier.full_denom(creator_addr, subdenom).unwrap();

//     GetDenomResponse {
//         denom: response.denom,
//     }
// }

// fn validate_denom(
//     deps: DepsMut<TokenFactoryQuery>,
//     denom: String,
// ) -> Result<(), TokenFactoryError> {
//     let denom_to_split = denom.clone();
//     let tokenfactory_denom_parts: Vec<&str> = denom_to_split.split('/').collect();

//     if tokenfactory_denom_parts.len() != 3 {
//         return Result::Err(TokenFactoryError::InvalidDenom {
//             denom,
//             message: std::format!(
//                 "denom must have 3 parts separated by /, had {}",
//                 tokenfactory_denom_parts.len()
//             ),
//         });
//     }

//     let prefix = tokenfactory_denom_parts[0];
//     let creator_address = tokenfactory_denom_parts[1];
//     let subdenom = tokenfactory_denom_parts[2];

//     if !prefix.eq_ignore_ascii_case("factory") {
//         return Result::Err(TokenFactoryError::InvalidDenom {
//             denom,
//             message: std::format!("prefix must be 'factory', was {}", prefix),
//         });
//     }

//     // Validate denom by attempting to query for full denom
//     let response = TokenQuerier::new(&deps.querier)
//         .full_denom(String::from(creator_address), String::from(subdenom));
//     if response.is_err() {
//         return Result::Err(TokenFactoryError::InvalidDenom {
//             denom,
//             message: response.err().unwrap().to_string(),
//         });
//     }

//     Result::Ok(())
// }

// this is poor mans "skip" flag
#[cfg(test)]
mod tests {
    use std::marker::PhantomData;
    use cosmwasm_std::{CosmosMsg, Decimal, OwnedDeps, SubMsg};
    use cosmwasm_std::testing::{mock_env, mock_info, MockApi, MockQuerier, MockStorage};
    use token_bindings::{Metadata, TokenQuery};
    use crate::abc::{CurveType, HatchConfig, ReserveToken, SupplyToken};
    use super::*;
    use speculoos::prelude::*;
//     use crate::msg::CurveType;
//     use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info};
//     use cosmwasm_std::{coin, Decimal, OverflowError, OverflowOperation, StdError, SubMsg};
//     use cw_utils::PaymentError;

    const DENOM: &str = "satoshi";
    const CREATOR: &str = "creator";
    const INVESTOR: &str = "investor";
    const BUYER: &str = "buyer";

    const SUPPLY_DENOM: &str = "subdenom";



    fn default_supply_metadata() -> Metadata {
        Metadata {
            name: Some("Bonded".to_string()),
            symbol: Some("EPOXY".to_string()),
            description: None,
            denom_units: vec![],
            base: None,
            display: None,
        }
    }

    fn default_instantiate(
        decimals: u8,
        reserve_decimals: u8,
        curve_type: CurveType,
    ) -> InstantiateMsg {
        InstantiateMsg {
            supply: SupplyToken {
                subdenom: SUPPLY_DENOM.to_string(),
                metadata: default_supply_metadata(),
                decimals,
            },
            reserve: ReserveToken {
                denom: DENOM.to_string(),
                decimals: reserve_decimals,
            },
            phase_config: HatchConfig {
                initial_raise: (Uint128::one(), Uint128::from(100u128)),
                initial_price: Uint128::one(),
                initial_allocation: 10,
                reserve_percentage: 10,
            },
            curve_type,
        }
    }

//     fn get_balance<U: Into<String>>(deps: Deps, addr: U) -> Uint128 {
//         query_balance(deps, addr.into()).unwrap().balance
//     }

//     fn setup_test(deps: DepsMut, decimals: u8, reserve_decimals: u8, curve_type: CurveType) {
//         // this matches `linear_curve` test case from curves.rs
//         let creator = String::from(CREATOR);
//         let msg = default_instantiate(decimals, reserve_decimals, curve_type);
//         let info = mock_info(&creator, &[]);

//         // make sure we can instantiate with this
//         let res = instantiate(deps, mock_env(), info, msg).unwrap();
//         assert_eq!(0, res.messages.len());
//     }

    /// Mock token factory querier dependencies
    fn mock_tf_dependencies() -> OwnedDeps<MockStorage, MockApi, MockQuerier<TokenFactoryQuery>, TokenFactoryQuery> {
        OwnedDeps {
            storage: MockStorage::default(),
            api: MockApi::default(),
            querier: MockQuerier::<TokenFactoryQuery>::new(&[]),
            custom_query_type: PhantomData::<TokenFactoryQuery>,
        }
    }

    #[test]
    fn proper_instantiation() -> CwAbcResult<()> {
        let mut deps = mock_tf_dependencies();

        // this matches `linear_curve` test case from curves.rs
        let creator = String::from("creator");
        let curve_type = CurveType::SquareRoot {
            slope: Uint128::new(1),
            scale: 1,
        };
        let msg = default_instantiate(2, 8, curve_type.clone());
        let info = mock_info(&creator, &[]);

        // make sure we can instantiate with this
        let res = instantiate(deps.as_mut(), mock_env(), info, msg.clone())?;
        assert_that!(res.messages.len()).is_equal_to(1);
        let submsg = res.messages.get(0).unwrap();
        assert_that!(submsg.msg).is_equal_to(CosmosMsg::Custom(TokenFactoryMsg::Token(TokenMsg::CreateDenom {
            subdenom: SUPPLY_DENOM.to_string(),
            metadata: Some(default_supply_metadata()),
        })));

        // TODO!
        // // token info is proper
        // let token = query_token_info(deps.as_ref()).unwrap();
        // assert_that!(&token.name, &msg.name);
        // assert_that!(&token.symbol, &msg.symbol);
        // assert_that!(token.decimals, 2);
        // assert_that!(token.total_supply, Uint128::zero());

        // curve state is sensible
        let state = query_curve_info(deps.as_ref(), curve_type.to_curve_fn())?;
        assert_that!(state.reserve).is_equal_to(Uint128::zero());
        assert_that!(state.supply).is_equal_to(Uint128::zero());
        assert_that!(state.reserve_denom.as_str()).is_equal_to(DENOM);
        // spot price 0 as supply is 0
        assert_that!(state.spot_price).is_equal_to(Decimal::zero());

        // curve type is stored properly
        let curve = CURVE_TYPE.load(&deps.storage).unwrap();
        assert_eq!(curve_type, curve);

        // no balance
        // assert_eq!(get_balance(deps.as_ref(), &creator), Uint128::zero());

        Ok(())
    }

//     #[test]
//     fn buy_issues_tokens() {
//         let mut deps = mock_dependencies();
//         let curve_type = CurveType::Linear {
//             slope: Uint128::new(1),
//             scale: 1,
//         };
//         setup_test(deps.as_mut(), 2, 8, curve_type.clone());

//         // succeeds with proper token (5 BTC = 5*10^8 satoshi)
//         let info = mock_info(INVESTOR, &coins(500_000_000, DENOM));
//         let buy = ExecuteMsg::Buy {};
//         execute(deps.as_mut(), mock_env(), info, buy.clone()).unwrap();

//         // bob got 1000 EPOXY (10.00)
//         assert_eq!(get_balance(deps.as_ref(), INVESTOR), Uint128::new(1000));
//         assert_eq!(get_balance(deps.as_ref(), BUYER), Uint128::zero());

//         // send them all to buyer
//         let info = mock_info(INVESTOR, &[]);
//         let send = ExecuteMsg::Transfer {
//             recipient: BUYER.into(),
//             amount: Uint128::new(1000),
//         };
//         execute(deps.as_mut(), mock_env(), info, send).unwrap();

//         // ensure balances updated
//         assert_eq!(get_balance(deps.as_ref(), INVESTOR), Uint128::zero());
//         assert_eq!(get_balance(deps.as_ref(), BUYER), Uint128::new(1000));

//         // second stake needs more to get next 1000 EPOXY
//         let info = mock_info(INVESTOR, &coins(1_500_000_000, DENOM));
//         execute(deps.as_mut(), mock_env(), info, buy).unwrap();

//         // ensure balances updated
//         assert_eq!(get_balance(deps.as_ref(), INVESTOR), Uint128::new(1000));
//         assert_eq!(get_balance(deps.as_ref(), BUYER), Uint128::new(1000));

//         // check curve info updated
//         let curve = query_curve_info(deps.as_ref(), curve_type.to_curve_fn()).unwrap();
//         assert_eq!(curve.reserve, Uint128::new(2_000_000_000));
//         assert_eq!(curve.supply, Uint128::new(2000));
//         assert_eq!(curve.spot_price, Decimal::percent(200));

//         // check token info updated
//         let token = query_token_info(deps.as_ref()).unwrap();
//         assert_eq!(token.decimals, 2);
//         assert_eq!(token.total_supply, Uint128::new(2000));
//     }

//     #[test]
//     fn bonding_fails_with_wrong_denom() {
//         let mut deps = mock_dependencies();
//         let curve_type = CurveType::Linear {
//             slope: Uint128::new(1),
//             scale: 1,
//         };
//         setup_test(deps.as_mut(), 2, 8, curve_type);

//         // fails when no tokens sent
//         let info = mock_info(INVESTOR, &[]);
//         let buy = ExecuteMsg::Buy {};
//         let err = execute(deps.as_mut(), mock_env(), info, buy.clone()).unwrap_err();
//         assert_eq!(err, PaymentError::NoFunds {}.into());

//         // fails when wrong tokens sent
//         let info = mock_info(INVESTOR, &coins(1234567, "wei"));
//         let err = execute(deps.as_mut(), mock_env(), info, buy.clone()).unwrap_err();
//         assert_eq!(err, PaymentError::MissingDenom(DENOM.into()).into());

//         // fails when too many tokens sent
//         let info = mock_info(INVESTOR, &[coin(3400022, DENOM), coin(1234567, "wei")]);
//         let err = execute(deps.as_mut(), mock_env(), info, buy).unwrap_err();
//         assert_eq!(err, PaymentError::MultipleDenoms {}.into());
//     }

//     #[test]
//     fn burning_sends_reserve() {
//         let mut deps = mock_dependencies();
//         let curve_type = CurveType::Linear {
//             slope: Uint128::new(1),
//             scale: 1,
//         };
//         setup_test(deps.as_mut(), 2, 8, curve_type.clone());

//         // succeeds with proper token (20 BTC = 20*10^8 satoshi)
//         let info = mock_info(INVESTOR, &coins(2_000_000_000, DENOM));
//         let buy = ExecuteMsg::Buy {};
//         execute(deps.as_mut(), mock_env(), info, buy).unwrap();

//         // bob got 2000 EPOXY (20.00)
//         assert_eq!(get_balance(deps.as_ref(), INVESTOR), Uint128::new(2000));

//         // cannot burn too much
//         let info = mock_info(INVESTOR, &[]);
//         let burn = ExecuteMsg::Burn {
//             amount: Uint128::new(3000),
//         };
//         let err = execute(deps.as_mut(), mock_env(), info, burn).unwrap_err();
//         // TODO check error

//         // burn 1000 EPOXY to get back 15BTC (*10^8)
//         let info = mock_info(INVESTOR, &[]);
//         let burn = ExecuteMsg::Burn {
//             amount: Uint128::new(1000),
//         };
//         let res = execute(deps.as_mut(), mock_env(), info, burn).unwrap();

//         // balance is lower
//         assert_eq!(get_balance(deps.as_ref(), INVESTOR), Uint128::new(1000));

//         // ensure we got our money back
//         assert_eq!(1, res.messages.len());
//         assert_eq!(
//             &res.messages[0],
//             &SubMsg::new(BankMsg::Send {
//                 to_address: INVESTOR.into(),
//                 amount: coins(1_500_000_000, DENOM),
//             })
//         );

//         // check curve info updated
//         let curve = query_curve_info(deps.as_ref(), curve_type.to_curve_fn()).unwrap();
//         assert_eq!(curve.reserve, Uint128::new(500_000_000));
//         assert_eq!(curve.supply, Uint128::new(1000));
//         assert_eq!(curve.spot_price, Decimal::percent(100));

//         // check token info updated
//         let token = query_token_info(deps.as_ref()).unwrap();
//         assert_eq!(token.decimals, 2);
//         assert_eq!(token.total_supply, Uint128::new(1000));
//     }
}