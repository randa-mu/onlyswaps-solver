use crate::eth::IRouter::SwapRequestParameters;
use crate::model::{ChainState, RequestId, Trade, Transfer};
use crate::util::normalise_chain_id;
use alloy::primitives::U256;
use async_trait::async_trait;
use moka::sync::Cache;
use std::collections::HashMap;

#[async_trait]
pub(crate) trait ChainStateProvider {
    async fn fetch_state(&self) -> eyre::Result<ChainState>;
}

pub(crate) struct Solver<'a, CSP> {
    states: HashMap<u64, ChainState>,
    chains: &'a HashMap<u64, CSP>,
}

impl<'a, CSP: ChainStateProvider> Solver<'a, CSP> {
    pub async fn from(chains: &'a HashMap<u64, CSP>) -> eyre::Result<Self> {
        let mut states: HashMap<u64, ChainState> = HashMap::new();

        // fetch the initial state for each chain before we listen for blocks
        for (chain_id, chain) in chains {
            states.insert(*chain_id, chain.fetch_state().await?);
        }

        Ok(Self { states, chains })
    }

    pub async fn fetch_state(&mut self, chain_id: u64, in_flight: &Cache<RequestId, ()>) -> eyre::Result<Vec<Trade>> {
        let chain = self.chains.get(&chain_id).expect("somehow got event for a non-existent chain");
        let updated_state = chain.fetch_state().await?;
        self.states.insert(chain_id, updated_state);
        Ok(calculate_trades(chain_id, &self.states, in_flight))
    }
}

fn calculate_trades(chain_id: u64, states: &HashMap<u64, ChainState>, in_flight: &Cache<RequestId, ()>) -> Vec<Trade> {
    let mut trades = Vec::new();
    let mut owned_states = states.clone();
    // we only want the current chain's transactions, as we may have trades in flight for other chains
    let transfers = &states
        .get(&chain_id)
        .expect("somehow we got a block from a chain that doesn't have a state")
        .transfers;

    for transfer in transfers {
        if in_flight.contains_key(&transfer.request_id) {
            continue;
        }
        solve(transfer, &mut trades, &mut owned_states);
    }

    trades
}

fn solve(transfer_request: &Transfer, trades: &mut Vec<Trade>, states: &mut HashMap<u64, ChainState>) {
    let SwapRequestParameters {
        dstChainId,
        tokenOut,
        amountOut,
        solverFee,
        executed,
        ..
    } = transfer_request.params;

    let dest_state = match states.get_mut(&normalise_chain_id(dstChainId)) {
        None => return,
        Some(state) => state,
    };

    if executed {
        return;
    }

    if dest_state.already_fulfilled.contains(&transfer_request.request_id) {
        return;
    }

    if dest_state.native_balance == U256::from(0) {
        return;
    }

    if dest_state.token_balance < amountOut {
        return;
    }

    // just takes a flat fee for the moment
    if solverFee < U256::from(1) {
        return;
    }

    if tokenOut != dest_state.token_addr {
        return;
    }

    // we commit some of our tokens to this trade so the next one doesn't fail
    dest_state.token_balance -= amountOut;
    trades.push(transfer_request.into())
}

#[cfg(test)]
mod tests {
    use crate::eth::IRouter::SwapRequestParameters;
    use crate::model::{ChainState, Trade, Transfer};
    use crate::solver::{ChainStateProvider, Solver, calculate_trades};
    use crate::util::test::{generate_address, generate_request_id};
    use alloy::primitives::{Address, U256, address};
    use async_trait::async_trait;
    use moka::sync::Cache;
    use speculoos::assert_that;
    use speculoos::vec::VecAssertions;
    use std::collections::HashMap;

    static USER_ADDR: Address = address!("0xdeadbeef6964af9d7eed9e03e53415d37aa96045");
    static TOKEN_ADDR: Address = address!("0xd8da6bf26964af9d7eed9e03e53415d37aa96045");

    #[tokio::test]
    async fn transfers_created_through_solver_create_trades() {
        // given
        let chain_id = 1;
        let transfer_params = create_transfer_params(USER_ADDR, 1, 2, 100);
        let chain_one_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(1),
            token_balance: U256::from(1),
            transfers: vec![transfer_params.clone()],
            already_fulfilled: vec![],
        };
        let chain_two_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(100),
            token_balance: U256::from(1000),
            transfers: Vec::default(),
            already_fulfilled: vec![],
        };
        let chain_one = StubbedChain { state: chain_one_state };
        let chain_two = StubbedChain { state: chain_two_state };
        let networks = HashMap::from([(1, chain_one), (2, chain_two)]);

        // when
        let mut solver = Solver::from(&networks).await.unwrap();
        let trades = solver.fetch_state(chain_id, &Cache::new(1)).await.unwrap();

        // then
        let expected_output_amount = transfer_params.params.amountOut;
        let expected_trade = Trade {
            request_id: transfer_params.request_id,
            token_addr: transfer_params.params.tokenOut,
            src_chain_id: transfer_params.params.srcChainId,
            dest_chain_id: transfer_params.params.dstChainId,
            recipient_addr: transfer_params.params.recipient,
            swap_amount: expected_output_amount,
        };
        assert_that!(trades).has_length(1);
        assert_that!(trades[0]).is_equal_to(expected_trade);
    }

    #[test]
    fn multiple_transfers_create_multiple_trades() {
        // given
        // both transfers use 100
        let transfer_params = create_transfer_params(USER_ADDR, 1, 2, 100);
        let transfer_params_2 = create_transfer_params(USER_ADDR, 1, 2, 100);

        let src_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(0),
            token_balance: U256::from(0),
            transfers: vec![transfer_params, transfer_params_2],
            already_fulfilled: vec![],
        };
        // on dst_chain, we only have enough balance to cover one tx
        let dst_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(1000),
            token_balance: U256::from(200),
            transfers: vec![],
            already_fulfilled: vec![],
        };
        let state = HashMap::from([(1, src_chain_state), (2, dst_chain_state)]);

        // when
        let trades = calculate_trades(1, &state, &Cache::new(1));

        // then
        assert_that!(trades).has_length(2);
    }

    #[test]
    fn transfers_across_multiple_chains_only_create_trades_for_src_chain() {
        // given
        // both transfers use 100
        let transfer_params = create_transfer_params(USER_ADDR, 1, 2, 100);
        let transfer_params_2 = create_transfer_params(USER_ADDR, 1, 2, 100);

        let src_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(1000),
            token_balance: U256::from(100),
            transfers: vec![transfer_params],
            already_fulfilled: vec![],
        };
        // on dst_chain, we only have enough balance to cover one tx
        let dst_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(1000),
            token_balance: U256::from(200),
            transfers: vec![transfer_params_2],
            already_fulfilled: vec![],
        };
        let state = HashMap::from([(1, src_chain_state), (2, dst_chain_state)]);

        // when
        let trades = calculate_trades(1, &state, &Cache::new(1));

        // then
        assert_that!(trades).has_length(1);
    }

    #[test]
    fn no_transfers_creates_no_trades() {
        // given
        let src_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(0),
            token_balance: U256::from(0),
            transfers: vec![],
            already_fulfilled: vec![],
        };
        let dst_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(0),
            token_balance: U256::from(1000),
            transfers: vec![],
            already_fulfilled: vec![],
        };
        let state = HashMap::from([(1, src_chain_state), (2, dst_chain_state)]);

        // when
        let trades = calculate_trades(1, &state, &Cache::new(1));

        // then
        assert_that!(trades).has_length(0);
    }

    #[test]
    fn no_native_currency_on_dest_chain_doesnt_trade() {
        // given
        let src_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(0),
            token_balance: U256::from(0),
            transfers: vec![create_transfer_params(USER_ADDR, 1, 2, 100)],
            already_fulfilled: vec![],
        };
        let dst_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(0),
            token_balance: U256::from(1000),
            transfers: vec![],
            already_fulfilled: vec![],
        };
        let state = HashMap::from([(1, src_chain_state), (2, dst_chain_state)]);

        // when
        let trades = calculate_trades(1, &state, &Cache::new(1));

        // then
        assert_that!(trades).has_length(0);
    }

    #[test]
    fn no_token_balance_doesnt_trade() {
        // given
        let src_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(0),
            token_balance: U256::from(0),
            transfers: vec![create_transfer_params(USER_ADDR, 1, 2, 100)],
            already_fulfilled: vec![],
        };
        let dst_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(1000),
            token_balance: U256::from(0),
            transfers: vec![],
            already_fulfilled: vec![],
        };
        let state = HashMap::from([(1, src_chain_state), (2, dst_chain_state)]);

        // when
        let trades = calculate_trades(1, &state, &Cache::new(1));

        // then
        assert_that!(trades).has_length(0);
    }

    #[test]
    fn already_executed_doesnt_create_tx() {
        // given
        let mut transfer_params = create_transfer_params(USER_ADDR, 1, 2, 100);
        transfer_params.params.executed = true;

        let src_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(0),
            token_balance: U256::from(0),
            transfers: vec![transfer_params],
            already_fulfilled: vec![],
        };
        let dst_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(1000),
            token_balance: U256::from(1000),
            transfers: vec![],
            already_fulfilled: vec![],
        };
        let state = HashMap::from([(1, src_chain_state), (2, dst_chain_state)]);

        // when
        let trades = calculate_trades(1, &state, &Cache::new(1));

        // then
        assert_that!(trades).has_length(0);
    }

    #[test]
    fn no_fee_gives_no_trade() {
        // given
        let mut transfer_params = create_transfer_params(USER_ADDR, 1, 2, 100);
        transfer_params.params.solverFee = U256::from(0);

        let src_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(0),
            token_balance: U256::from(0),
            transfers: vec![transfer_params],
            already_fulfilled: vec![],
        };
        let dst_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(1000),
            token_balance: U256::from(1000),
            transfers: vec![],
            already_fulfilled: vec![],
        };
        let state = HashMap::from([(1, src_chain_state), (2, dst_chain_state)]);

        // when
        let trades = calculate_trades(1, &state, &Cache::new(1));

        // then
        assert_that!(trades).has_length(0);
    }

    #[test]
    fn invalid_token_addr_gives_no_trade() {
        // given
        let mut transfer_params = create_transfer_params(USER_ADDR, 1, 2, 100);
        transfer_params.params.tokenOut = generate_address();

        let src_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(0),
            token_balance: U256::from(0),
            transfers: vec![transfer_params],
            already_fulfilled: vec![],
        };
        let dst_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(1000),
            token_balance: U256::from(1000),
            transfers: vec![],
            already_fulfilled: vec![],
        };
        let state = HashMap::from([(1, src_chain_state), (2, dst_chain_state)]);

        // when
        let trades = calculate_trades(1, &state, &Cache::new(1));

        // then
        assert_that!(trades).has_length(0);
    }

    #[test]
    fn subsequent_calls_dont_use_same_balance() {
        // given
        // both transfers use 100
        let transfer_params = create_transfer_params(USER_ADDR, 1, 2, 100);
        let transfer_params_2 = create_transfer_params(USER_ADDR, 1, 2, 100);

        let src_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(0),
            token_balance: U256::from(0),
            transfers: vec![transfer_params, transfer_params_2],
            already_fulfilled: vec![],
        };
        // on dst_chain, we only have enough balance to cover one tx
        let dst_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(1000),
            token_balance: U256::from(150),
            transfers: vec![],
            already_fulfilled: vec![],
        };
        let state = HashMap::from([(1, src_chain_state), (2, dst_chain_state)]);

        // when
        let trades = calculate_trades(1, &state, &Cache::new(1));

        // then
        assert_that!(trades).has_length(1);
    }

    #[test]
    fn transfers_that_have_already_been_fulfilled_dont_make_trades() {
        // given
        // both transfers use 100
        let transfer_params = create_transfer_params(USER_ADDR, 1, 2, 100);

        let src_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(0),
            token_balance: U256::from(0),
            transfers: vec![transfer_params.clone()],
            already_fulfilled: vec![],
        };
        // on dst_chain, we only have enough balance to cover one tx
        let dst_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(1000),
            token_balance: U256::from(150),
            transfers: vec![],
            already_fulfilled: vec![transfer_params.request_id],
        };
        let state = HashMap::from([(1, src_chain_state), (2, dst_chain_state)]);

        // when
        let trades = calculate_trades(1, &state, &Cache::new(1));

        // then
        assert_that!(trades).has_length(0);
    }

    #[test]
    fn transfer_that_exist_in_cache_dont_make_trades() {
        // given
        // transfer use 100
        let transfer_params = create_transfer_params(USER_ADDR, 1, 2, 100);

        let src_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(0),
            token_balance: U256::from(0),
            transfers: vec![transfer_params.clone()],
            already_fulfilled: vec![],
        };
        let dst_chain_state = ChainState {
            token_addr: TOKEN_ADDR,
            native_balance: U256::from(1000),
            token_balance: U256::from(200),
            transfers: vec![],
            already_fulfilled: vec![],
        };
        // we create a cache that already has the request_id in it
        let cache = Cache::new(1);
        let id = transfer_params.clone().request_id;
        cache.insert(id, ());
        let state = HashMap::from([(1, src_chain_state), (2, dst_chain_state)]);

        // when
        let trades = calculate_trades(1, &state, &cache);

        // then
        assert_that!(trades).has_length(0);
    }

    fn create_transfer_params(sender: Address, src_chain_id: u64, dest_chain_id: u64, amount: u64) -> Transfer {
        Transfer {
            request_id: generate_request_id(),
            params: SwapRequestParameters {
                srcChainId: U256::from(src_chain_id),
                dstChainId: U256::from(dest_chain_id),
                sender,
                recipient: sender,
                tokenIn: TOKEN_ADDR,
                tokenOut: TOKEN_ADDR,
                amountOut: U256::from(amount),
                verificationFee: U256::from(2),
                solverFee: U256::from(1),
                nonce: U256::from(100),
                executed: false,
                requestedAt: U256::from(12345),
            },
        }
    }
    struct StubbedChain {
        state: ChainState,
    }

    #[async_trait]
    impl ChainStateProvider for StubbedChain {
        async fn fetch_state(&self) -> eyre::Result<ChainState> {
            Ok(self.state.clone())
        }
    }
}
