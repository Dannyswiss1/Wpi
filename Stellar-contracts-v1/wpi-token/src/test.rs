use super::*;
use proptest::prelude::*;
use soroban_sdk::testutils::{
    storage::{Instance as _, Persistent as _},
    Address as _, Events as _, Ledger as _, MockAuth, MockAuthInvoke,
};
use soroban_sdk::IntoVal;

fn deposit_id(env: &Env, tag: u8) -> BytesN<32> {
    BytesN::from_array(env, &[tag; 32])
}

fn redemption_id(env: &Env, tag: u8) -> BytesN<32> {
    BytesN::from_array(env, &[tag; 32])
}

fn setup(
    env: &Env,
    mint_limit: i128,
    burn_limit: i128,
    window_seconds: u64,
) -> (Address, WpiTokenClient<'_>, Address) {
    env.mock_all_auths();
    env.ledger().set_timestamp(1_000);
    let admin = Address::generate(env);
    let user = Address::generate(env);
    let contract_id = env.register(WpiToken, ());
    let client = WpiTokenClient::new(env, &contract_id);
    client.initialize(&admin);
    client.configure_volume_limits(&mint_limit, &burn_limit, &window_seconds);
    client.configure_max_mint_per_tx(&mint_limit);
    (admin, client, user)
}

#[test]
fn bridge_operations_fail_closed_until_limits_are_configured() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let user = Address::generate(&env);
    let contract_id = env.register(WpiToken, ());
    let client = WpiTokenClient::new(&env, &contract_id);
    client.initialize(&admin);

    // Both mint gates fail closed, and neither can be satisfied by
    // configuring the other.
    assert_eq!(
        client.try_mint_from_deposit(&user, &1, &deposit_id(&env, 1)),
        Err(Ok(Error::MintTxCapNotConfigured))
    );
    assert_eq!(
        client.try_max_mint_per_tx(),
        Err(Ok(Error::MintTxCapNotConfigured))
    );

    client.configure_max_mint_per_tx(&1_000);
    assert_eq!(
        client.try_mint_from_deposit(&user, &1, &deposit_id(&env, 1)),
        Err(Ok(Error::VolumeLimitsNotConfigured))
    );
    assert_eq!(client.balance(&user), 0);
}

#[test]
fn mint_from_deposit_over_tx_cap_is_rejected_and_alert_is_committed() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 1_000, 1_000, 86_400);
    client.configure_max_mint_per_tx(&100);

    let accepted = client.mint_from_deposit(&user, &101, &deposit_id(&env, 1));

    // `env.events()` only holds the most recent invocation's events, so the
    // alert is asserted before any other contract call.
    let expected = MintTxCapExceeded {
        to: user.clone(),
        amount: 101,
        max_mint_per_tx: 100,
    };
    assert_eq!(
        env.events().all(),
        vec![
            &env,
            (
                client.address.clone(),
                expected.topics(&env),
                expected.data(&env)
            )
        ]
    );

    assert!(!accepted);
    assert_eq!(client.balance(&user), 0);
    assert_eq!(client.total_supply(), 0);
    // The rejected mint consumes no window capacity and does not burn the
    // deposit id, so it can be retried once governance raises the ceiling.
    assert_eq!(client.current_volume_window().minted, 0);
    assert!(!client.is_deposit_processed(&deposit_id(&env, 1)));
    // A per-transaction rejection is an input check, not a circuit breaker:
    // the bridge keeps running for other deposits.
    assert!(!client.paused());
    assert!(!client.circuit_breaker_active());

    client.mint_from_deposit(&user, &100, &deposit_id(&env, 2));
    assert_eq!(client.balance(&user), 100);
}

#[test]
fn mint_at_exactly_the_tx_cap_is_accepted() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 1_000, 1_000, 86_400);
    client.configure_max_mint_per_tx(&100);

    assert!(client.mint_from_deposit(&user, &100, &deposit_id(&env, 1)));

    assert_eq!(client.balance(&user), 100);
    assert_eq!(client.current_volume_window().minted, 100);
}

#[test]
fn minter_mint_cannot_bypass_the_tx_cap() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 1_000, 1_000, 86_400);
    client.configure_max_mint_per_tx(&100);

    let accepted = client.mint(&user, &i128::MAX);

    assert!(!accepted);
    assert_eq!(client.balance(&user), 0);
    assert_eq!(client.total_supply(), 0);
    assert_eq!(client.current_volume_window().minted, 0);
}

/// The per-transaction ceiling does not replace the rolling window: a
/// compromised key splitting one large mint into cap-sized mints still trips
/// the window breaker.
#[test]
fn repeated_under_cap_mints_still_trip_the_window_breaker() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 100, 1_000, 86_400);
    client.configure_max_mint_per_tx(&40);

    client.mint_from_deposit(&user, &40, &deposit_id(&env, 1));
    client.mint_from_deposit(&user, &40, &deposit_id(&env, 2));
    assert!(!client.paused());

    client.mint_from_deposit(&user, &20, &deposit_id(&env, 3));

    assert_eq!(client.balance(&user), 100);
    assert!(client.paused());
    assert!(client.circuit_breaker_active());
}

#[test]
fn tx_cap_is_enforced_after_the_pause_and_replay_gates() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 1_000, 1_000, 86_400);
    client.configure_max_mint_per_tx(&100);
    client.mint_from_deposit(&user, &50, &deposit_id(&env, 1));

    // A replayed deposit is rejected as a replay even when it is also over
    // the ceiling, and a paused contract rejects everything.
    client.configure_max_mint_per_tx(&10);
    assert_eq!(
        client.try_mint_from_deposit(&user, &50, &deposit_id(&env, 1)),
        Err(Ok(Error::DepositAlreadyProcessed))
    );
    client.set_paused(&true);
    assert_eq!(
        client.try_mint_from_deposit(&user, &50, &deposit_id(&env, 2)),
        Err(Ok(Error::Paused))
    );
}

#[test]
fn invalid_tx_cap_configuration_is_rejected() {
    let env = Env::default();
    let (_admin, client, _user) = setup(&env, 100, 100, 86_400);

    assert_eq!(
        client.try_configure_max_mint_per_tx(&0),
        Err(Ok(Error::InvalidMintTxCap))
    );
    assert_eq!(
        client.try_configure_max_mint_per_tx(&-1),
        Err(Ok(Error::InvalidMintTxCap))
    );
    assert_eq!(client.max_mint_per_tx(), 100);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn non_admin_signer_cannot_authenticate_configure_max_mint_per_tx() {
    let env = Env::default();
    let (_admin, client, _user) = setup(&env, 100, 100, 86_400);
    let attacker = Address::generate(&env);

    client
        .mock_auths(&[MockAuth {
            address: &attacker,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "configure_max_mint_per_tx",
                args: (&i128::MAX,).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .configure_max_mint_per_tx(&i128::MAX);
}

/// The mint ceiling is owned by the volume-limit admin, so the hot minter key
/// that signs bridge mints cannot raise its own ceiling once the roles are
/// delegated to separate holders.
#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn minter_cannot_raise_the_tx_cap_after_rotation() {
    let env = Env::default();
    let (_admin, client, _user) = setup(&env, 1_000, 1_000, 86_400);
    let bridge_minter = Address::generate(&env);
    let guardian = Address::generate(&env);
    client.set_minter(&bridge_minter);
    client.set_volume_limit_admin(&guardian);

    client
        .mock_auths(&[MockAuth {
            address: &bridge_minter,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "configure_max_mint_per_tx",
                args: (&i128::MAX,).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .configure_max_mint_per_tx(&i128::MAX);
}

#[test]
fn delegated_volume_limit_admin_can_raise_the_tx_cap() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 10_000, 10_000, 86_400);
    client.configure_max_mint_per_tx(&100);
    let guardian = Address::generate(&env);
    client.set_volume_limit_admin(&guardian);

    assert!(!client.mint_from_deposit(&user, &500, &deposit_id(&env, 1)));

    client
        .mock_auths(&[MockAuth {
            address: &guardian,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "configure_max_mint_per_tx",
                args: (&500i128,).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .configure_max_mint_per_tx(&500);

    assert_eq!(client.max_mint_per_tx(), 500);
    assert!(client.mint_from_deposit(&user, &500, &deposit_id(&env, 1)));
    assert_eq!(client.balance(&user), 500);
}

#[test]
fn mint_limit_trips_breaker_emits_event_and_halts_activity() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 100, 1_000, 86_400);

    client.mint_from_deposit(&user, &60, &deposit_id(&env, 1));
    client.mint_from_deposit(&user, &40, &deposit_id(&env, 2));
    // The triggering invocation emits both VolumeLimitTriggered and DepositMinted.
    assert_eq!(env.events().all().len(), 2);

    assert_eq!(client.balance(&user), 100);
    assert!(client.paused());
    assert!(client.circuit_breaker_active());

    let blocked = client.try_mint_from_deposit(&user, &1, &deposit_id(&env, 3));
    assert_eq!(blocked, Err(Ok(Error::Paused)));
    assert!(!client.is_deposit_processed(&deposit_id(&env, 3)));
    assert_eq!(client.balance(&user), 100);
}

#[test]
fn mint_that_would_exceed_limit_is_rejected_but_alert_is_committed() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 100, 1_000, 86_400);
    client.mint_from_deposit(&user, &60, &deposit_id(&env, 1));

    let accepted = client.mint_from_deposit(&user, &41, &deposit_id(&env, 2));

    assert!(!accepted);
    assert_eq!(env.events().all().len(), 1);
    assert_eq!(client.balance(&user), 60);
    assert_eq!(client.total_supply(), 60);
    assert_eq!(client.current_volume_window().minted, 60);
    assert!(!client.is_deposit_processed(&deposit_id(&env, 2)));
    assert!(client.circuit_breaker_active());
    assert!(client.paused());
}

#[test]
fn burn_limit_is_tracked_independently_and_halts_activity() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 1_000, 100, 86_400);
    let destination = BytesN::from_array(&env, &[9; 32]);
    client.mint_from_deposit(&user, &200, &deposit_id(&env, 1));

    client.burn(&user, &60, &destination, &redemption_id(&env, 1));
    client.burn(&user, &40, &destination, &redemption_id(&env, 2));

    assert_eq!(client.balance(&user), 100);
    assert!(client.paused());
    assert_eq!(client.current_volume_window().burned, 100);
    assert_eq!(client.current_volume_window().minted, 200);

    let blocked = client.try_burn(&user, &1, &destination, &redemption_id(&env, 3));
    assert_eq!(blocked, Err(Ok(Error::Paused)));
    assert_eq!(client.balance(&user), 100);
}

#[test]
fn burn_replay_is_rejected_for_the_same_redemption_id() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 1_000, 1_000, 86_400);
    let destination = BytesN::from_array(&env, &[9; 32]);
    let redemption = redemption_id(&env, 1);
    client.mint_from_deposit(&user, &200, &deposit_id(&env, 1));

    client.burn(&user, &60, &destination, &redemption);

    let replay = client.try_burn(&user, &40, &destination, &redemption);
    assert_eq!(replay, Err(Ok(Error::RedemptionAlreadyProcessed)));
    assert_eq!(client.balance(&user), 140);
    assert_eq!(client.total_supply(), 140);
}

#[test]
fn expired_window_resets_volume_before_next_operation() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 100, 100, 10);
    client.mint_from_deposit(&user, &60, &deposit_id(&env, 1));

    env.ledger().set_timestamp(1_011);
    client.mint_from_deposit(&user, &60, &deposit_id(&env, 2));

    let window = client.current_volume_window();
    assert_eq!(window.started_at, 1_001);
    assert_eq!(window.minted, 60);
    assert!(!client.paused());
    assert_eq!(client.balance(&user), 120);
}

#[test]
fn rolling_window_counts_volume_across_time_buckets() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 100, 100, 10);
    client.mint_from_deposit(&user, &60, &deposit_id(&env, 1));

    env.ledger().set_timestamp(1_009);
    client.mint_from_deposit(&user, &40, &deposit_id(&env, 2));

    assert_eq!(client.current_volume_window().minted, 100);
    assert!(client.circuit_breaker_active());
    assert!(client.paused());
}

#[test]
fn rolling_window_does_not_expire_volume_early_at_bucket_boundary() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 100, 100, 86_400);
    env.ledger().set_timestamp(3_599);
    client.mint_from_deposit(&user, &60, &deposit_id(&env, 1));

    // This is only 82,801 seconds later, even though it is 24 bucket indexes
    // ahead. The safety bucket must keep the first mint in the rolling total.
    env.ledger().set_timestamp(86_400);
    client.mint_from_deposit(&user, &40, &deposit_id(&env, 2));

    assert_eq!(client.current_volume_window().minted, 100);
    assert!(client.circuit_breaker_active());
}

#[test]
fn user_state_is_persistent_and_gets_its_own_ttl() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 1_000, 1_000, 86_400);
    let spender = Address::generate(&env);

    client.mint_from_deposit(&user, &25, &deposit_id(&env, 1));
    client.approve(&user, &spender, &10, &(env.ledger().sequence() + 500));

    env.as_contract(&client.address, || {
        assert_eq!(
            env.storage()
                .persistent()
                .get_ttl(&DataKey::Balance(user.clone())),
            PERSISTENT_ENTRY_TTL_EXTEND_TO
        );
        assert_eq!(
            env.storage()
                .persistent()
                .get_ttl(&DataKey::Allowance(user.clone(), spender.clone())),
            PERSISTENT_ENTRY_TTL_EXTEND_TO
        );
    });
}

#[test]
fn admin_can_refresh_instance_ttl_for_idle_periods() {
    let env = Env::default();
    let (_admin, client, _user) = setup(&env, 1_000, 1_000, 86_400);

    env.ledger()
        .set_sequence_number(env.ledger().sequence() + INSTANCE_TTL_EXTEND_TO - 25);

    env.as_contract(&client.address, || {
        assert_eq!(env.storage().instance().get_ttl(), 25);
    });

    client.bump_instance_ttl();

    env.as_contract(&client.address, || {
        assert_eq!(env.storage().instance().get_ttl(), INSTANCE_TTL_EXTEND_TO);
    });
}

#[test]
fn an_older_balance_entry_expiring_does_not_wipe_newer_accounts() {
    let env = Env::default();
    let (_admin, client, user_a) = setup(&env, 1_000, 1_000, 86_400);
    let user_b = Address::generate(&env);

    client.mint_from_deposit(&user_a, &7, &deposit_id(&env, 1));

    env.ledger()
        .set_sequence_number(env.ledger().sequence() + PERSISTENT_ENTRY_TTL_EXTEND_TO - 10);
    client.mint_from_deposit(&user_b, &11, &deposit_id(&env, 2));

    env.as_contract(&client.address, || {
        assert_eq!(
            env.storage()
                .persistent()
                .get_ttl(&DataKey::Balance(user_a.clone())),
            10
        );
        assert_eq!(
            env.storage()
                .persistent()
                .get_ttl(&DataKey::Balance(user_b.clone())),
            PERSISTENT_ENTRY_TTL_EXTEND_TO
        );
    });

    env.ledger()
        .set_sequence_number(env.ledger().sequence() + 11);

    assert_eq!(client.balance(&user_b), 11);

    env.as_contract(&client.address, || {
        assert!(
            env.storage()
                .persistent()
                .get_ttl(&DataKey::Balance(user_b.clone()))
                > PERSISTENT_ENTRY_TTL_THRESHOLD
        );
    });
}

#[test]
fn only_override_can_lift_a_tripped_circuit_breaker() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 50, 100, 86_400);
    client.mint_from_deposit(&user, &50, &deposit_id(&env, 1));

    let ordinary_unpause = client.try_set_paused(&false);
    assert_eq!(ordinary_unpause, Err(Ok(Error::CircuitBreakerActive)));

    client.override_volume_limit();
    assert!(!client.paused());
    assert!(!client.circuit_breaker_active());
    assert_eq!(client.current_volume_window().minted, 0);
    assert_eq!(client.current_volume_window().burned, 0);

    client.mint_from_deposit(&user, &10, &deposit_id(&env, 2));
    assert_eq!(client.balance(&user), 60);
}

/// Regardless of which address signs the transaction, only the address read
/// from storage (`read_admin`/`read_volume_limit_admin`) can ever satisfy
/// `require_auth`. Since these functions no longer accept an admin argument,
/// there is nothing left for a caller to "pass" that could stand in for the
/// real admin -- the only way to reach the privileged branch is to be the
/// stored admin.
#[test]
#[should_panic]
fn non_admin_signer_cannot_authenticate_mint() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 10, 10, 10);
    let attacker = Address::generate(&env);

    client
        .mock_auths(&[MockAuth {
            address: &attacker,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "mint_from_deposit",
                args: (&user, &1i128, &deposit_id(&env, 1)).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .mint_from_deposit(&user, &1, &deposit_id(&env, 1));
}

#[test]
#[should_panic]
fn non_admin_signer_cannot_authenticate_configure_volume_limits() {
    let env = Env::default();
    let (_admin, client, _user) = setup(&env, 10, 10, 10);
    let attacker = Address::generate(&env);

    client
        .mock_auths(&[MockAuth {
            address: &attacker,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "configure_volume_limits",
                args: (&20i128, &20i128, &20u64).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .configure_volume_limits(&20, &20, &20);
}

#[test]
#[should_panic]
fn non_admin_signer_cannot_authenticate_override_volume_limit() {
    let env = Env::default();
    let (_admin, client, _user) = setup(&env, 10, 10, 10);
    let attacker = Address::generate(&env);

    client
        .mock_auths(&[MockAuth {
            address: &attacker,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "override_volume_limit",
                args: ().into_val(&env),
                sub_invokes: &[],
            },
        }])
        .override_volume_limit();
}

#[test]
fn volume_limit_admin_is_independent_from_bridge_admin() {
    let env = Env::default();
    let (bridge_admin, client, user) = setup(&env, 50, 100, 86_400);
    let guardian = Address::generate(&env);
    client.set_volume_limit_admin(&guardian);

    assert_eq!(client.volume_limit_admin(), guardian);
    assert_eq!(client.admin(), bridge_admin);

    client.mint_from_deposit(&user, &50, &deposit_id(&env, 1));
    assert!(client.circuit_breaker_active());

    // Only the volume-limit admin (guardian), not the bridge admin, can lift
    // the circuit breaker.
    client.override_volume_limit();
    assert!(!client.circuit_breaker_active());
    assert!(!client.paused());
}

/// Demonstrates that the bridge admin role and the volume-limit admin role
/// are enforced independently from stored state: after the volume-limit role
/// is rotated to `guardian`, the (still valid, still-a-real-admin)
/// `bridge_admin` address can no longer authenticate volume-limit-gated
/// calls, even though it could before the rotation.
#[test]
#[should_panic]
fn bridge_admin_cannot_authenticate_as_volume_limit_admin_after_rotation() {
    let env = Env::default();
    let (bridge_admin, client, _user) = setup(&env, 50, 100, 86_400);
    let guardian = Address::generate(&env);
    client.set_volume_limit_admin(&guardian);

    client
        .mock_auths(&[MockAuth {
            address: &bridge_admin,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "override_volume_limit",
                args: ().into_val(&env),
                sub_invokes: &[],
            },
        }])
        .override_volume_limit();
}

#[test]
fn minter_role_is_independent_from_bridge_admin() {
    let env = Env::default();
    let (admin, client, user) = setup(&env, 50, 100, 86_400);
    let bridge_ops = Address::generate(&env);
    client.set_minter(&bridge_ops);

    assert_eq!(client.minter(), bridge_ops);
    assert_eq!(client.admin(), admin);

    client.mint_from_deposit(&user, &1, &deposit_id(&env, 1));
    assert_eq!(client.balance(&user), 1);
}

/// After the minter role is rotated away, the top-level admin can no longer
/// authenticate mint calls, even though it still holds the Admin role.
#[test]
#[should_panic]
fn admin_cannot_authenticate_mint_after_minter_rotation() {
    let env = Env::default();
    let (admin, client, user) = setup(&env, 50, 100, 86_400);
    let bridge_ops = Address::generate(&env);
    client.set_minter(&bridge_ops);

    client
        .mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "mint_from_deposit",
                args: (&user, &1i128, &deposit_id(&env, 1)).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .mint_from_deposit(&user, &1, &deposit_id(&env, 1));
}

/// Mirrors `bridge_admin_cannot_authenticate_as_volume_limit_admin_after_rotation`:
/// once the minter role is handed to a dedicated address, the admin cannot
/// reclaim it by calling `set_minter` itself, so a compromised admin key
/// alone cannot re-seize mint power.
#[test]
#[should_panic]
fn admin_cannot_reclaim_minter_role() {
    let env = Env::default();
    let (admin, client, _user) = setup(&env, 50, 100, 86_400);
    let bridge_ops = Address::generate(&env);
    client.set_minter(&bridge_ops);
    let takeover = Address::generate(&env);

    client
        .mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "set_minter",
                args: (&takeover,).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .set_minter(&takeover);
}

#[test]
fn pauser_role_is_independent_from_bridge_admin_and_minter() {
    let env = Env::default();
    let (admin, client, _user) = setup(&env, 50, 100, 86_400);
    let bridge_ops = Address::generate(&env);
    let guardian = Address::generate(&env);
    client.set_minter(&bridge_ops);
    client.set_pauser(&guardian);

    assert_eq!(client.pauser(), guardian);
    assert_eq!(client.admin(), admin);
    assert_eq!(client.minter(), bridge_ops);

    client.set_paused(&true);
    assert!(client.paused());
    client.set_paused(&false);
    assert!(!client.paused());
}

/// Once the pauser role is rotated away, neither the admin nor the minter
/// can authenticate `set_paused` — only the independent pauser can, so a
/// compromised minter cannot also silence the emergency stop.
#[test]
#[should_panic]
fn admin_cannot_authenticate_set_paused_after_pauser_rotation() {
    let env = Env::default();
    let (admin, client, _user) = setup(&env, 50, 100, 86_400);
    let guardian = Address::generate(&env);
    client.set_pauser(&guardian);

    client
        .mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "set_paused",
                args: (true,).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .set_paused(&true);
}

/// Contracts initialized before this role split (Issue #5) never wrote a
/// `Minter`/`Pauser` key. `read_minter`/`read_pauser` must keep tracking the
/// live admin for those deployments until they explicitly rotate, exactly
/// like the pre-existing `VolumeLimitAdmin` migration fallback.
#[test]
fn upgraded_deployment_without_stored_minter_or_pauser_falls_back_to_admin() {
    let env = Env::default();
    let (admin, client, user) = setup(&env, 100, 100, 10);
    env.as_contract(&client.address, || {
        env.storage().instance().remove(&DataKey::Minter);
        env.storage().instance().remove(&DataKey::Pauser);
    });

    assert_eq!(client.minter(), admin);
    assert_eq!(client.pauser(), admin);

    client.mint_from_deposit(&user, &10, &deposit_id(&env, 1));
    assert_eq!(client.balance(&user), 10);
    client.set_paused(&true);
    assert!(client.paused());
}

/// Every deployed contract has a valid Soroban `Address`, the same type
/// used for every role here. Registering a second contract instance and
/// using its address as the admin stands in for a real multisig/policy
/// contract (e.g. one built on CAP-0071 / `SOROBAN_CREDENTIALS_ADDRESS_V2`
/// account delegation): this does not re-verify Soroban's own
/// auth-delegation host logic (out of scope for this contract's test
/// suite), it verifies that wpi-token never special-cases a role's
/// `Address` as an externally-owned account -- every role can be a
/// contract address end to end.
#[test]
fn every_role_can_be_a_contract_address_not_only_an_eoa() {
    let env = Env::default();
    env.mock_all_auths();
    let policy_contract = env.register(WpiToken, ());
    let contract_id = env.register(WpiToken, ());
    let client = WpiTokenClient::new(&env, &contract_id);
    client.initialize(&policy_contract);
    client.configure_volume_limits(&i128::MAX, &i128::MAX, &86_400);
    client.configure_max_mint_per_tx(&i128::MAX);

    assert_eq!(client.admin(), policy_contract);
    assert_eq!(client.minter(), policy_contract);
    assert_eq!(client.pauser(), policy_contract);
    assert_eq!(client.volume_limit_admin(), policy_contract);

    let user = Address::generate(&env);
    client.mint_from_deposit(&user, &10, &deposit_id(&env, 1));
    assert_eq!(client.balance(&user), 10);
}

#[test]
fn invalid_limit_configuration_is_rejected() {
    let env = Env::default();
    let (_admin, client, _user) = setup(&env, 10, 10, 10);

    assert_eq!(
        client.try_configure_volume_limits(&0, &10, &10),
        Err(Ok(Error::InvalidVolumeLimit))
    );
    assert_eq!(
        client.try_configure_volume_limits(&10, &10, &0),
        Err(Ok(Error::InvalidVolumeLimit))
    );
}

#[test]
fn deposit_idempotency_is_preserved() {
    let env = Env::default();
    let (_admin, client, user) = setup(&env, 1_000, 1_000, 86_400);
    let deposit = deposit_id(&env, 1);
    client.mint_from_deposit(&user, &100, &deposit);

    let retry = client.try_mint_from_deposit(&user, &100, &deposit);

    assert_eq!(retry, Err(Ok(Error::DepositAlreadyProcessed)));
    assert_eq!(client.balance(&user), 100);
    assert_eq!(client.current_volume_window().minted, 100);
}

const NUM_USERS: u8 = 4;

#[derive(Clone, Debug)]
enum Op {
    Mint(u8, i128),
    Burn(u8, i128),
    Transfer(u8, u8, i128),
}

fn user_index() -> impl Strategy<Value = u8> {
    0..NUM_USERS
}

fn amount_strategy() -> impl Strategy<Value = i128> {
    prop_oneof![
        3 => 0i128..=1_000_000i128,
        2 => (i128::MAX - 1_000_000)..=i128::MAX,
        1 => i128::MIN..=-1i128,
    ]
}

fn operation_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (user_index(), amount_strategy()).prop_map(|(user, amount)| Op::Mint(user, amount)),
        (user_index(), amount_strategy()).prop_map(|(user, amount)| Op::Burn(user, amount)),
        (user_index(), user_index(), amount_strategy())
            .prop_map(|(from, to, amount)| Op::Transfer(from, to, amount)),
    ]
}

fn property_setup(
    env: &Env,
) -> (
    WpiTokenClient<'_>,
    Address,
    [Address; NUM_USERS as usize],
    BytesN<32>,
) {
    env.mock_all_auths();
    let admin = Address::generate(env);
    let users = core::array::from_fn(|_| Address::generate(env));
    let destination = BytesN::from_array(env, &[9; 32]);
    let contract_id = env.register(WpiToken, ());
    let client = WpiTokenClient::new(env, &contract_id);
    client.initialize(&admin);
    client.configure_volume_limits(&i128::MAX, &i128::MAX, &86_400);
    client.configure_max_mint_per_tx(&i128::MAX);
    (client, admin, users, destination)
}

fn assert_supply_invariant(client: &WpiTokenClient<'_>, users: &[Address; NUM_USERS as usize]) {
    let sum = users
        .iter()
        .fold(0i128, |total, user| total + client.balance(user));
    assert_eq!(sum, client.total_supply());
}

proptest! {
    #[test]
    fn arbitrary_operations_preserve_total_supply(
        operations in prop::collection::vec(operation_strategy(), 0..30)
    ) {
        let env = Env::default();
        let (client, _admin, users, destination) = property_setup(&env);
        let mut mint_tag = 1u8;

        for operation in operations {
            match operation {
                Op::Mint(user, amount) => {
                    let deposit = deposit_id(&env, mint_tag);
                    mint_tag = mint_tag.wrapping_add(1);
                    let _ = client.try_mint_from_deposit(&users[user as usize], &amount, &deposit);
                }
                Op::Burn(user, amount) => {
                    let redemption = redemption_id(&env, mint_tag);
                    mint_tag = mint_tag.wrapping_add(1);
                    let _ = client.try_burn(&users[user as usize], &amount, &destination, &redemption);
                }
                Op::Transfer(from, to, amount) => {
                    let owner = users[from as usize].clone();
                    let _ = client.try_transfer(&owner, &users[to as usize], &amount);
                }
            }
            assert_supply_invariant(&client, &users);
        }
    }

    #[test]
    fn self_transfer_never_changes_balance(
        user_index in user_index(),
        mint_amount in 1i128..=1_000_000_000i128,
        transfer_amount in amount_strategy(),
    ) {
        let env = Env::default();
        let (client, _admin, users, _destination) = property_setup(&env);
        let user = users[user_index as usize].clone();
        client.mint_from_deposit(&user, &mint_amount, &deposit_id(&env, 1));
        let before = client.balance(&user);

        let _ = client.try_transfer(&user, &user, &transfer_amount);

        prop_assert_eq!(client.balance(&user), before);
        assert_supply_invariant(&client, &users);
    }
}

#[test]
fn test_propose_admin_does_not_immediately_change_admin() {
    let env = Env::default();
    let (admin, client, _user) = setup(&env, 100, 100, 10);
    let new_admin = Address::generate(&env);
    client.propose_admin(&new_admin);
    assert_eq!(client.admin(), admin);
    assert_eq!(client.proposed_admin(), Some(new_admin));
}

#[test]
fn test_accept_admin_transfers_ownership() {
    let env = Env::default();
    let (_admin, client, _user) = setup(&env, 100, 100, 10);
    let new_admin = Address::generate(&env);
    client.propose_admin(&new_admin);
    assert_eq!(client.proposed_admin(), Some(new_admin.clone()));
    client.accept_admin();
    assert_eq!(client.admin(), new_admin);
    assert_eq!(client.proposed_admin(), None);
}

#[test]
#[should_panic]
fn test_unauthorized_acceptance_fails() {
    let env = Env::default();
    let (_admin, client, _user) = setup(&env, 100, 100, 10);
    let new_admin = Address::generate(&env);
    let attacker = Address::generate(&env);
    client.propose_admin(&new_admin);
    client
        .mock_auths(&[MockAuth {
            address: &attacker,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "accept_admin",
                args: ().into_val(&env),
                sub_invokes: &[],
            },
        }])
        .accept_admin();
}

#[test]
fn test_accept_admin_fails_if_no_proposal() {
    let env = Env::default();
    let (_admin, client, _user) = setup(&env, 100, 100, 10);
    let result = client.try_accept_admin();
    assert_eq!(result, Err(Ok(Error::NoProposedAdmin)));
}

#[test]
fn test_existing_admin_retains_privileges_until_acceptance() {
    let env = Env::default();
    let (admin, client, _user) = setup(&env, 100, 100, 10);
    let new_admin = Address::generate(&env);

    // Propose transfer
    client.propose_admin(&new_admin);

    // Existing admin can still configure volume limits
    client.configure_volume_limits(&200, &200, &20);
    assert_eq!(
        client.volume_limit_config(),
        VolumeLimitConfig {
            mint_limit: 200,
            burn_limit: 200,
            window_seconds: 20,
        }
    );

    // Accept transfer
    client.accept_admin();

    // Now, old admin is no longer admin, so they cannot propose another
    // admin transfer (pause/mint are gated by the separate pauser/minter
    // roles, not the Admin role -- see role-separation tests above).
    let result = client
        .mock_auths(&[MockAuth {
            address: &admin,
            invoke: &MockAuthInvoke {
                contract: &client.address,
                fn_name: "propose_admin",
                args: (&admin,).into_val(&env),
                sub_invokes: &[],
            },
        }])
        .try_propose_admin(&admin);
    assert!(result.is_err());
}

#[test]
fn decimals_matches_pi_network_native_precision() {
    let env = Env::default();
    let (_admin, client, _user) = setup(&env, 100, 100, 10);

    assert_eq!(DECIMALS, 7);
    assert_eq!(client.decimals(), DECIMALS);

    // 1 Pi expressed in stroops must equal 10^DECIMALS.
    let stroops_per_pi: i128 = 10i128.pow(DECIMALS);
    assert_eq!(stroops_per_pi, 10_000_000);

    // A representative Pi Horizon deposit of "3.1415926" Pi should produce
    // exactly 31_415_926 stroops when converted with 7 decimal places.
    let whole: i128 = 3;
    let fraction: i128 = 1_415_926;
    let expected_stroops = whole * stroops_per_pi + fraction;
    assert_eq!(expected_stroops, 31_415_926);
}
