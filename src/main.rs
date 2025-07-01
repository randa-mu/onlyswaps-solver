use alloy::network::EthereumWallet;
use alloy::primitives::U256;
use alloy::providers::{Provider, ProviderBuilder, WsConnect};
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use axum::Router;
use axum::routing::get;
use clap::Parser;
use eyre::eyre;
use serde::Deserialize;
use shellexpand::tilde;
use std::fs;
use std::str::FromStr;
use tokio::net::TcpListener;

#[derive(Parser, Debug)]
struct CliArgs {
    #[arg(
        short = 'c',
        long = "config-path",
        env = "SOLVER_CONFIG_PATH",
        default_value = "~/.solver/config.json"
    )]
    config_path: String,

    #[arg(short = 's', long = "private-key", env = "SOLVER_PRIVATE_KEY")]
    private_key: String,

    #[arg(short = 'p', long = "port", env = "SOLVER_PORT", default_value = "8080")]
    port: u16,
}

#[derive(Deserialize, Debug)]
struct ConfigFile {
    networks: Vec<NetworkConfig>,
}

#[derive(Deserialize, Debug)]
struct NetworkConfig {
    chain_id: String,
    name: String,
    rpc_url: String,
}

sol!(
    #[sol(rpc)]
    ERC20FaucetToken,
    "onlysubs-solidity/out/ERC20FaucetToken.sol/ERC20FaucetToken.json"
);

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let cli = CliArgs::parse();
    let config: ConfigFile = load_config_file(&cli);

    let app = Router::new().route("/health", get(healthcheck_handler));
    let listener = TcpListener::bind(("0.0.0.0", cli.port)).await?;

    if config.networks.is_empty() {
        return Err(eyre!("no networks configured"));
    }

    let network = config.networks.get(0).expect("should be impossibru");
    let signer = PrivateKeySigner::from_str(&cli.private_key)?;
    let our_address = signer.address();
    println!("using address {} for chain {}", our_address, &network.name);

    let provider = create_provider(&network.rpc_url, PrivateKeySigner::from_str(&cli.private_key)?).await?;
    let rusd_token_contract = ERC20FaucetToken::new("0xb1F323844dcfde76710fC801F33D4E24d7201B84".parse()?, provider);

    let rusd_balance = rusd_token_contract.balanceOf(our_address).call().await?;
    if rusd_balance == U256::from(0) {
        println!("withdrawing some tokens");
        let tx = rusd_token_contract.mint().send().await?;
        let receipt = tx.get_receipt().await?;
        println!("withdrew tokens: {}", receipt.transaction_hash);
    } else {
        println!("balance {} - not withdrawing tokens", rusd_balance);
    }

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    println!("{} chain(s) have been configured", config.networks.len());
    println!("Listening on port {}", cli.port);
    tokio::select! {
        _ = sigterm.recv() => {
            println!("received SIGTERM, shutting down...");
            Ok(())
        },

        _ = sigint.recv() => {
            println!("received SIGINT, shutting down...");
            Ok(())
        },

        _ = tokio::signal::ctrl_c() => {
            println!("received ctrl+c, shutting down...");
            Ok(())
        },

        res = axum::serve(listener, app) => {
            match res {
                Ok(_) => Err(eyre!("http server stopped unexpectedly")),
                Err(e) => Err(eyre!("http server stopped unexpectedly: {}", e))
            }
        }
    }
}

fn load_config_file(cli: &CliArgs) -> ConfigFile {
    match fs::read(tilde(&cli.config_path).into_owned()) {
        Ok(contents) => serde_json::from_slice(&contents)
            .expect(format!("failed to parse config file at {}", cli.config_path).as_str()),
        Err(err) => panic!(
            "failed to read config file at {}: {:?}",
            cli.config_path,
            err.to_string()
        ),
    }
}

async fn healthcheck_handler() -> &'static str {
    "ok"
}

async fn create_provider(rpc_url: &str, signer: PrivateKeySigner) -> eyre::Result<Box<impl Provider>> {
    let base = ProviderBuilder::new().wallet(EthereumWallet::from(signer));
    if rpc_url.starts_with("http") {
        Ok(Box::new(base.connect_http(rpc_url.parse()?)))
    } else if rpc_url.starts_with("ws") {
        Ok(Box::new(base.connect_ws(WsConnect::new(rpc_url)).await?))
    } else {
        Err(eyre!("RPC URL {} is not http or ws", rpc_url))
    }
}
