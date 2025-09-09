use crate::eth::ERC20FaucetToken::ERC20FaucetTokenInstance;
use crate::eth::Router::RouterInstance;
use crate::model::{RequestId, Trade};
use crate::network::Network;
use crate::util::normalise_chain_id;
use alloy::primitives::TxHash;
use alloy::providers::Provider;
use moka::sync::Cache;
use std::collections::HashMap;

pub(crate) struct TradeExecutor<'a, P> {
    routers: HashMap<u64, &'a RouterInstance<P>>,
    tokens: HashMap<u64, &'a ERC20FaucetTokenInstance<P>>,
}

impl<'a, P: Provider> TradeExecutor<'a, P> {
    pub fn new(networks: &'a HashMap<u64, Network<P>>) -> Self {
        let routers = networks.iter().map(|(chain_id, net)| (*chain_id, &net.router)).collect();
        let tokens = networks.iter().map(|(chain_id, net)| (*chain_id, &net.token)).collect();
        Self { routers, tokens }
    }
    pub async fn execute(&self, trades: Vec<Trade>, in_flight: &mut Cache<RequestId, ()>) {
        for trade in trades {
            // first we add the trade to the cache so that we don't retry it in the next block
            // (before it's been finalised, potentially)
            in_flight.insert(trade.request_id, ());

            // then we get the contract bindings for the destination chain
            let router = self
                .routers
                .get(&normalise_chain_id(trade.dest_chain_id))
                .expect("somehow didn't have a router binding for a solved trade");
            let token = self
                .tokens
                .get(&normalise_chain_id(trade.dest_chain_id))
                .expect("somehow didn't have a token binding for a solved trade");

            // in theory, we shouldn't need to wait until the next block because txs will be processed in nonce order
            // but for whatever reason this doesn't seem to be the case :(
            let approve: eyre::Result<TxHash> = async {
                let tx = token.approve(*router.address(), trade.swap_amount).send().await?;
                let receipt = tx.watch().await?;
                Ok(receipt)
            }
            .await;
            match approve {
                Ok(_) => {}
                Err(e) => {
                    println!("error approving trade: {e}");
                }
            }

            // actually send the funds via the router contract
            let relay: eyre::Result<TxHash> = async {
                let tx = router
                    .relayTokens(
                        trade.token_addr,
                        trade.recipient_addr,
                        trade.swap_amount,
                        trade.request_id.into(),
                        trade.src_chain_id,
                    )
                    .send()
                    .await?;
                let receipt = tx.watch().await?;
                Ok(receipt)
            }
            .await;
            match relay {
                Ok(_) => println!("successfully traded {} on {}", trade.swap_amount, trade.dest_chain_id),
                Err(e) => println!("error trading {} on {}: {}", trade.swap_amount, trade.dest_chain_id, e),
            }
        }
    }
}
