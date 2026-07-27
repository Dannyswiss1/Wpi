#![no_std]

//! Mock AMM pool for testing wPi -> USDC swaps.
//! Hardcodes a 1:1 swap rate (or configurable) for testnet simulation without complex math.

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, token, Address, BytesN, Env,
};

mod usdc;
#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    TokenIn,  // wPi
    TokenOut, // Network USDC SAC
    Rate,     // Rate: out_amount = in_amount * Rate / 1_000_000
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    NotAdmin = 1,
    InsufficientLiquidity = 2,
    SlippageExceeded = 3,
    InvalidAmount = 4,
}

fn read_admin(env: &Env) -> Address {
    env.storage()
        .instance()
        .get::<DataKey, Address>(&DataKey::Admin)
        .unwrap()
}

fn read_token_out(env: &Env) -> Address {
    env.storage()
        .instance()
        .get::<DataKey, Address>(&DataKey::TokenOut)
        .unwrap()
}

#[contract]
pub struct MockAmm;

#[contractimpl]
impl MockAmm {
    pub fn initialize(env: Env, admin: Address, token_in: Address, rate_bps: u32) {
        let token_out = usdc::address(&env);
        admin.require_auth();
        Self::set_config(env, admin, token_in, token_out, rate_bps);
    }

    fn set_config(env: Env, admin: Address, token_in: Address, token_out: Address, rate_bps: u32) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::TokenIn, &token_in);
        env.storage().instance().set(&DataKey::TokenOut, &token_out);
        env.storage().instance().set(&DataKey::Rate, &rate_bps);
    }

    /// Swap token_in (wPi) for the network's USDC SAC.
    pub fn swap(
        env: Env,
        to: Address,
        amount_in: i128,
        min_amount_out: i128,
    ) -> Result<i128, Error> {
        to.require_auth();

        let token_in_addr: Address = env.storage().instance().get(&DataKey::TokenIn).unwrap();
        let token_out_addr: Address = env.storage().instance().get(&DataKey::TokenOut).unwrap();
        let rate: u32 = env.storage().instance().get(&DataKey::Rate).unwrap();

        let amount_out = (amount_in * rate as i128) / 1_000_000;

        if amount_out < min_amount_out {
            return Err(Error::SlippageExceeded);
        }

        let token_in = token::Client::new(&env, &token_in_addr);
        let token_out = token::Client::new(&env, &token_out_addr);

        let contract_addr = env.current_contract_address();

        if token_out.balance(&contract_addr) < amount_out {
            return Err(Error::InsufficientLiquidity);
        }

        token_in.transfer(&to, &contract_addr, &amount_in);
        token_out.transfer(&contract_addr, &to, &amount_out);

        Ok(amount_out)
    }

    pub fn deposit_liquidity(env: Env, from: Address, amount_out: i128) {
        from.require_auth();
        let token_out_addr: Address = env.storage().instance().get(&DataKey::TokenOut).unwrap();
        let token_out = token::Client::new(&env, &token_out_addr);
        token_out.transfer(&from, env.current_contract_address(), &amount_out);
    }

    /// Withdraws pooled `token_out` liquidity back out of the pool, the
    /// counterpart to [`MockAmm::deposit_liquidity`]. Restricted to the pool
    /// admin: this is a testnet mock with no LP-share accounting, so seeded
    /// liquidity is recoverable only by whoever seeded the pool.
    ///
    /// The admin is authenticated from stored state rather than taken as an
    /// argument, so a caller-supplied address can never stand in for it.
    pub fn withdraw_liquidity(env: Env, to: Address, amount: i128) -> Result<(), Error> {
        read_admin(&env).require_auth();
        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        let token_out = token::Client::new(&env, &read_token_out(&env));
        let pool = env.current_contract_address();
        if token_out.balance(&pool) < amount {
            return Err(Error::InsufficientLiquidity);
        }
        token_out.transfer(&pool, &to, &amount);
        Ok(())
    }

    pub fn upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        let current_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if admin != current_admin {
            return Err(Error::NotAdmin);
        }
        admin.require_auth();
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        Ok(())
    }
}

#[cfg(test)]
mod test {
    extern crate std;

    use super::{Error, MockAmm, MockAmmClient};
    use soroban_sdk::testutils::{Address as _, MockAuth, MockAuthInvoke};
    use soroban_sdk::{token, Address, Env, IntoVal};

    struct Pool<'a> {
        admin: Address,
        trader: Address,
        amm_id: Address,
        amm: MockAmmClient<'a>,
        wpi: token::Client<'a>,
        usdc: token::Client<'a>,
    }

    /// Registers a 1:1 pool whose admin holds 100 USDC and whose trader holds
    /// 100 wPi, with `token_out` wired to a locally registered SAC rather than
    /// the network-resolved USDC address.
    fn setup(env: &Env) -> Pool<'_> {
        let admin = Address::generate(env);
        let trader = Address::generate(env);
        let wpi = env.register_stellar_asset_contract_v2(Address::generate(env));
        let usdc = env.register_stellar_asset_contract_v2(Address::generate(env));
        let amm_id = env.register(MockAmm, ());

        env.as_contract(&amm_id, || {
            MockAmm::set_config(
                env.clone(),
                admin.clone(),
                wpi.address(),
                usdc.address(),
                1_000_000,
            );
        });

        env.mock_all_auths();
        token::StellarAssetClient::new(env, &wpi.address()).mint(&trader, &100);
        token::StellarAssetClient::new(env, &usdc.address()).mint(&admin, &100);

        Pool {
            admin,
            trader,
            amm: MockAmmClient::new(env, &amm_id),
            wpi: token::Client::new(env, &wpi.address()),
            usdc: token::Client::new(env, &usdc.address()),
            amm_id,
        }
    }

    #[test]
    fn swaps_against_registered_stellar_asset_contract() {
        let env = Env::default();
        let pool = setup(&env);

        pool.amm.deposit_liquidity(&pool.admin, &100);
        assert_eq!(pool.amm.swap(&pool.trader, &40, &40), 40);

        assert_eq!(pool.wpi.balance(&pool.trader), 60);
        assert_eq!(pool.usdc.balance(&pool.trader), 40);
        assert_eq!(pool.usdc.balance(&pool.amm_id), 60);
    }

    #[test]
    fn deposit_and_withdraw_liquidity_round_trip() {
        let env = Env::default();
        let pool = setup(&env);

        pool.amm.deposit_liquidity(&pool.admin, &100);
        assert_eq!(pool.usdc.balance(&pool.amm_id), 100);
        assert_eq!(pool.usdc.balance(&pool.admin), 0);

        pool.amm.withdraw_liquidity(&pool.admin, &40);
        assert_eq!(pool.usdc.balance(&pool.amm_id), 60);
        assert_eq!(pool.usdc.balance(&pool.admin), 40);

        pool.amm.withdraw_liquidity(&pool.admin, &60);
        assert_eq!(pool.usdc.balance(&pool.amm_id), 0);
        assert_eq!(pool.usdc.balance(&pool.admin), 100);
    }

    #[test]
    fn withdraw_liquidity_recovers_the_remainder_after_a_swap() {
        let env = Env::default();
        let pool = setup(&env);
        pool.amm.deposit_liquidity(&pool.admin, &100);
        pool.amm.swap(&pool.trader, &40, &40);

        pool.amm.withdraw_liquidity(&pool.admin, &60);

        assert_eq!(pool.usdc.balance(&pool.amm_id), 0);
        assert_eq!(pool.usdc.balance(&pool.admin), 60);
    }

    #[test]
    fn withdraw_liquidity_can_send_to_an_address_other_than_the_admin() {
        let env = Env::default();
        let pool = setup(&env);
        let treasury = Address::generate(&env);
        pool.amm.deposit_liquidity(&pool.admin, &100);

        pool.amm.withdraw_liquidity(&treasury, &25);

        assert_eq!(pool.usdc.balance(&treasury), 25);
        assert_eq!(pool.usdc.balance(&pool.amm_id), 75);
    }

    #[test]
    fn withdraw_liquidity_rejects_more_than_the_pool_holds() {
        let env = Env::default();
        let pool = setup(&env);
        pool.amm.deposit_liquidity(&pool.admin, &100);

        assert_eq!(
            pool.amm.try_withdraw_liquidity(&pool.admin, &101),
            Err(Ok(Error::InsufficientLiquidity))
        );
        assert_eq!(pool.usdc.balance(&pool.amm_id), 100);
    }

    #[test]
    fn withdraw_liquidity_rejects_non_positive_amounts() {
        let env = Env::default();
        let pool = setup(&env);
        pool.amm.deposit_liquidity(&pool.admin, &100);

        assert_eq!(
            pool.amm.try_withdraw_liquidity(&pool.admin, &0),
            Err(Ok(Error::InvalidAmount))
        );
        assert_eq!(
            pool.amm.try_withdraw_liquidity(&pool.admin, &-1),
            Err(Ok(Error::InvalidAmount))
        );
        assert_eq!(pool.usdc.balance(&pool.amm_id), 100);
    }

    /// Only the address read from storage can satisfy `require_auth`, so
    /// signing with any other address cannot drain the pool.
    #[test]
    #[should_panic]
    fn non_admin_signer_cannot_authenticate_withdraw_liquidity() {
        let env = Env::default();
        let pool = setup(&env);
        pool.amm.deposit_liquidity(&pool.admin, &100);
        let attacker = Address::generate(&env);

        pool.amm
            .mock_auths(&[MockAuth {
                address: &attacker,
                invoke: &MockAuthInvoke {
                    contract: &pool.amm_id,
                    fn_name: "withdraw_liquidity",
                    args: (&attacker, &100i128).into_val(&env),
                    sub_invokes: &[],
                },
            }])
            .withdraw_liquidity(&attacker, &100);
    }
}
