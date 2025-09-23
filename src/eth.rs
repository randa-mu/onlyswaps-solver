use alloy::sol;

sol!(
    #[allow(clippy::too_many_arguments)]
    #[derive(Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
    #[sol(rpc)]
    ERC20FaucetToken,
    "onlyswaps-solidity/out/ERC20FaucetToken.sol/ERC20FaucetToken.json"
);

sol!(
    #[allow(clippy::too_many_arguments)]
    #[derive(Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
    #[sol(rpc)]
    Router,
    "onlyswaps-solidity/out/Router.sol/Router.json",
);
