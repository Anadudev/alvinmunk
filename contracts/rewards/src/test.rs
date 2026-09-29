#![cfg(test)]
//! Integration tests for the Rewards -> Reputation CROSS-CONTRACT read
//! (`claim_reward` gates USDC payout on `get_earned`) + the USDC SAC transfer +
//! the on-chain reward registry (caller can never dictate the payout amount).
extern crate std;
use super::*;
use alvinmunk_quest_registry::{QuestRegistryContract, QuestRegistryContractClient};
use alvinmunk_reputation::{ReputationContract, ReputationContractClient};
use ed25519_dalek::{Signer, SigningKey};
use soroban_sdk::{testutils::Address as _, token, BytesN, Env};

struct Fixture<'a> {
    env: Env,
    rep: ReputationContractClient<'a>,
    rewards: RewardsContractClient<'a>,
    quest: QuestRegistryContractClient<'a>,
    usdc: Address,
    rewards_id: Address,
    attester: Address,
    attester_sk: SigningKey,
    attester_pub: BytesN<32>,
}

fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn setup() -> Fixture<'static> {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let attester = Address::generate(&env);
    let attester_sk = signing_key(7);
    let attester_pub = BytesN::from_array(&env, &attester_sk.verifying_key().to_bytes());

    let rep_id = env.register(ReputationContract, ());
    let rep = ReputationContractClient::new(&env, &rep_id);
    rep.init(&admin);
    rep.add_attester(&attester);

    let quest_id = env.register(QuestRegistryContract, ());
    let quest = QuestRegistryContractClient::new(&env, &quest_id);
    quest.init(&admin, &rep_id);
    quest.add_attester_key(&attester_pub);
    rep.add_attester(&quest_id);

    // USDC Stellar Asset Contract (test SAC).
    let sac = env.register_stellar_asset_contract_v2(admin.clone());
    let usdc = sac.address();

    let rewards_id = env.register(RewardsContract, ());
    let rewards = RewardsContractClient::new(&env, &rewards_id);
    rewards.init(&admin, &usdc, &rep_id);
    rewards.set_quest_registry(&quest_id);

    // Fund the rewards treasury with USDC.
    token::StellarAssetClient::new(&env, &usdc).mint(&rewards_id, &1_000);

    Fixture {
        env,
        rep,
        rewards,
        quest,
        usdc,
        rewards_id,
        attester,
        attester_sk,
        attester_pub,
    }
}

fn award_quest(f: &Fixture, quest_id: u32, recipient: &Address) {
    let payload = f.quest.quest_payload(&quest_id, recipient);
    let msg: std::vec::Vec<u8> = payload.iter().collect();
    let sig = BytesN::from_array(&f.env, &f.attester_sk.sign(&msg).to_bytes());
    f.quest.award_quest(&f.attester_pub, &sig, &quest_id, recipient);
}

#[test]
fn claim_reward_reads_earned_and_pays_stored_amount() {
    let f = setup();
    let user = Address::generate(&f.env);

    // Admin registers reward #1: needs 50 Earned XP, pays 200 USDC from treasury.
    f.rewards.add_reward(&1u32, &50u64, &200i128);

    // User earns 100 Earned XP via the attester (the cashable track).
    f.rep.award_xp(&f.attester, &user, &2u32, &100u64);

    f.rewards.claim_reward(&user, &1u32);

    let token_c = token::TokenClient::new(&f.env, &f.usdc);
    assert_eq!(token_c.balance(&user), 200);
    assert_eq!(token_c.balance(&f.rewards_id), 800);
    assert!(f.rewards.is_claimed(&1u32, &user));
}

#[test]
fn get_rewards_lists_the_table() {
    let f = setup();
    f.rewards.add_reward(&1u32, &30u64, &50i128);
    f.rewards.add_reward(&2u32, &60u64, &100i128);
    f.rewards.add_reward(&1u32, &40u64, &75i128); // update — no duplicate row

    let table = f.rewards.get_rewards();
    assert_eq!(table.len(), 2);
    let first = table.get(0).unwrap();
    assert_eq!(first.id, 1);
    assert_eq!(first.threshold, 40); // reflects the update
    assert_eq!(first.amount, 75);
    assert!(first.active);
}

#[test]
#[should_panic]
fn claim_unregistered_reward_reverts() {
    let f = setup();
    let user = Address::generate(&f.env);
    f.rep.award_xp(&f.attester, &user, &2u32, &100u64);
    // No add_reward — there is no caller-supplied amount to exploit, and an unknown
    // reward id cannot be claimed. (panics: RewardNotFound)
    f.rewards.claim_reward(&user, &999u32);
}

#[test]
#[should_panic]
fn claim_inactive_reward_reverts() {
    let f = setup();
    let user = Address::generate(&f.env);
    f.rep.award_xp(&f.attester, &user, &2u32, &100u64);
    f.rewards.add_reward(&1u32, &50u64, &200i128);
    f.rewards.set_reward_active(&1u32, &false);
    f.rewards.claim_reward(&user, &1u32); // panics: RewardInactive
}

#[test]
#[should_panic]
fn claim_below_threshold_reverts() {
    let f = setup();
    let user = Address::generate(&f.env); // 0 earned
    f.rewards.add_reward(&1u32, &50u64, &200i128);
    f.rewards.claim_reward(&user, &1u32); // panics: BelowThreshold
}

#[test]
#[should_panic]
fn social_xp_does_not_unlock_treasury() {
    let f = setup();
    let alice = Address::generate(&f.env);
    let bob = Address::generate(&f.env);
    f.rewards.add_reward(&1u32, &5u64, &100i128);
    // Bob earns SOCIAL XP from a vouch (non-cashable).
    let secret = soroban_sdk::Bytes::from_array(&f.env, &[3u8; 32]);
    let hash = f.env.crypto().sha256(&secret).to_bytes();
    let id = f
        .rep
        .mint_vouch(&alice, &hash, &soroban_sdk::String::from_str(&f.env, "ty"));
    f.rep.claim_vouch(&bob, &id, &secret);
    // starter Social XP (20) + first-pair claim XP (10) = 30 — all SOCIAL, non-cashable.
    assert_eq!(f.rep.get_score(&bob), 30); // has Social XP
                                           // ...but Social XP must NOT open the treasury (keystone). threshold 5 > earned 0.
    f.rewards.claim_reward(&bob, &1u32); // panics: BelowThreshold
}

#[test]
#[should_panic]
fn double_claim_reverts() {
    let f = setup();
    let user = Address::generate(&f.env);
    f.rewards.add_reward(&1u32, &50u64, &100i128);
    f.rep.award_xp(&f.attester, &user, &2u32, &100u64);
    f.rewards.claim_reward(&user, &1u32);
    f.rewards.claim_reward(&user, &1u32); // panics: AlreadyClaimed
}

#[test]
#[should_panic]
fn daily_cap_blocks_over_limit_payout() {
    let f = setup();
    let user = Address::generate(&f.env);
    f.rewards.add_reward(&1u32, &50u64, &200i128);
    f.rewards.add_reward(&2u32, &50u64, &200i128);
    f.rewards.set_daily_cap(&250i128); // total/day
    f.rep.award_xp(&f.attester, &user, &2u32, &100u64);
    f.rewards.claim_reward(&user, &1u32); // 200 paid, within cap
    f.rewards.claim_reward(&user, &2u32); // 400 > 250 -> panics: DailyCapExceeded
}

#[test]
fn daily_cap_allows_within_limit_and_tracks_paid() {
    let f = setup();
    let user = Address::generate(&f.env);
    f.rewards.add_reward(&1u32, &50u64, &200i128);
    f.rewards.set_daily_cap(&500i128);
    f.rep.award_xp(&f.attester, &user, &2u32, &100u64);
    f.rewards.claim_reward(&user, &1u32);
    assert_eq!(f.rewards.get_daily_paid(), 200);
    assert_eq!(f.rewards.get_daily_cap(), 500);
}

#[test]
#[should_panic]
fn frozen_account_cannot_claim() {
    let f = setup();
    let user = Address::generate(&f.env);
    f.rewards.add_reward(&1u32, &50u64, &100i128);
    f.rep.award_xp(&f.attester, &user, &2u32, &100u64);
    f.rewards.set_frozen(&user, &true);
    f.rewards.claim_reward(&user, &1u32); // panics: Frozen
}

#[test]
#[should_panic]
fn frozen_account_cannot_tip() {
    let f = setup();
    let user = Address::generate(&f.env);
    let other = Address::generate(&f.env);
    f.rewards.set_frozen(&user, &true);
    f.rewards.tip(&user, &other, &10i128); // panics: Frozen
}

#[test]
#[should_panic]
fn proof_of_funding_blocks_unfunded_claim_when_enabled() {
    let f = setup();
    let user = Address::generate(&f.env);
    f.rewards.add_reward(&1u32, &50u64, &100i128);
    f.rep.award_xp(&f.attester, &user, &2u32, &100u64);
    f.rewards.set_require_funding(&true);
    f.rewards.claim_reward(&user, &1u32); // panics: NotFunded (received no external value)
}

#[test]
fn proof_of_funding_allows_funded_claim_and_is_off_by_default() {
    let f = setup();
    let user = Address::generate(&f.env);
    f.rewards.add_reward(&1u32, &50u64, &100i128);
    f.rep.award_xp(&f.attester, &user, &2u32, &100u64);
    assert!(!f.rewards.get_require_funding()); // default off (testnet demo works)

    f.rewards.set_require_funding(&true);
    f.rewards.set_funded(&user, &true); // verifier proved external value
    f.rewards.claim_reward(&user, &1u32);
    let token_c = token::TokenClient::new(&f.env, &f.usdc);
    assert_eq!(token_c.balance(&user), 100);
}

// --- Property/fuzz tests on the claim/cap math (Green-belt AC) ---
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(30))]

    /// Invariant: a claim pays EXACTLY the admin-registered amount and the treasury
    /// decreases by exactly that — the caller can never influence the payout, for any
    /// (threshold ≤ earned, amount). Treasury is funded with 1_000 in setup().
    #[test]
    fn claim_pays_exactly_registered_amount(
        threshold in 0u64..200, extra in 0u64..300, amount in 1i128..=1000
    ) {
        let f = setup();
        let user = Address::generate(&f.env);
        f.rewards.add_reward(&1u32, &threshold, &amount);
        f.rep.award_xp(&f.attester, &user, &2u32, &(threshold + extra));
        let tok = token::TokenClient::new(&f.env, &f.usdc);
        let before = tok.balance(&f.rewards_id);
        f.rewards.claim_reward(&user, &1u32);
        prop_assert_eq!(tok.balance(&user), amount);
        prop_assert_eq!(before - tok.balance(&f.rewards_id), amount);
    }

    /// Invariant: the daily payout cap is NEVER exceeded across an arbitrary claim
    /// sequence (the treasury circuit breaker holds under fuzzed inputs).
    #[test]
    fn daily_cap_is_never_exceeded(
        cap in 1i128..=1000, amounts in prop::collection::vec(1i128..=400, 1..6)
    ) {
        let f = setup();
        f.rewards.set_daily_cap(&cap);
        let mut paid = 0i128;
        for (i, a) in amounts.iter().enumerate() {
            let id = (i as u32) + 1;
            f.rewards.add_reward(&id, &0u64, a);
            let user = Address::generate(&f.env);
            if f.rewards.try_claim_reward(&user, &id).is_ok() {
                paid += *a;
            }
            prop_assert!(paid <= cap);
        }
    }
}

/// Release build of this contract, committed so the upgrade path can be tested without a
/// wasm build step in CI. Refresh with `make upgrade-fixtures` after changing the contract.
const REWARDS_WASM: &[u8] = include_bytes!("../testdata/alvinmunk_rewards.wasm");

#[test]
fn upgrade_to_identical_wasm_preserves_reward_table_and_treasury() {
    let f = setup();
    f.rewards.add_reward(&1u32, &30u64, &50i128);

    let hash = f.env.deployer().upload_contract_wasm(REWARDS_WASM);
    f.rewards.upgrade(&hash);

    let r = f.rewards.get_reward(&1u32).unwrap();
    assert_eq!((r.threshold, r.amount, r.active), (30, 50, true));

    // The upgraded contract still pays the stored amount from the same treasury.
    let user = Address::generate(&f.env);
    f.rep.award_xp(&f.attester, &user, &2u32, &30u64);
    f.rewards.claim_reward(&user, &1u32);
    let token_c = token::TokenClient::new(&f.env, &f.usdc);
    assert_eq!(token_c.balance(&user), 50);
    assert_eq!(token_c.balance(&f.rewards_id), 950);
}

#[test]
#[should_panic(expected = "HostError: Error(Auth, InvalidAction)")]
fn non_admin_upgrade_reverts() {
    let env = Env::default();
    let admin = Address::generate(&env);
    let usdc = Address::generate(&env);
    let rep = Address::generate(&env);
    let id = env.register(RewardsContract, ());
    let client = RewardsContractClient::new(&env, &id);
    client.init(&admin, &usdc, &rep);
    let hash = soroban_sdk::BytesN::from_array(&env, &[1; 32]);
    client.upgrade(&hash);
}

/// The host error a `panic_with_error!(Error::X)` surfaces as through a `try_` call.
fn contract_err(e: Error) -> soroban_sdk::Error {
    soroban_sdk::Error::from_contract_error(e as u32)
}

/// A wallet with enough Earned XP to clear `threshold`.
fn earner(f: &Fixture, xp: u64) -> Address {
    let user = Address::generate(&f.env);
    f.rep.award_xp(&f.attester, &user, &2u32, &xp);
    user
}

#[test]
fn capped_reward_pays_the_last_claim_and_rejects_the_next() {
    let f = setup();
    f.rewards.add_reward(&1u32, &30u64, &50i128);
    f.rewards.set_reward_supply(&1u32, &2u32);

    let (a, b, c) = (earner(&f, 30), earner(&f, 30), earner(&f, 30));
    f.rewards.claim_reward(&a, &1u32);
    f.rewards.claim_reward(&b, &1u32); // the last one the pool pays

    let stats = f.rewards.get_reward_stats(&1u32);
    assert_eq!((stats.max_claims, stats.claims), (2, 2));
    assert_eq!(
        f.rewards.try_claim_reward(&c, &1u32),
        Err(Ok(contract_err(Error::RewardExhausted)))
    );
    let token_c = token::TokenClient::new(&f.env, &f.usdc);
    assert_eq!(token_c.balance(&c), 0);
    assert_eq!(token_c.balance(&f.rewards_id), 900);
}

#[test]
fn uncapped_reward_counts_claims_without_a_limit() {
    let f = setup();
    f.rewards.add_reward(&1u32, &30u64, &10i128);
    for _ in 0..5 {
        let u = earner(&f, 30);
        f.rewards.claim_reward(&u, &1u32);
    }
    let stats = f.rewards.get_reward_stats(&1u32);
    assert_eq!((stats.max_claims, stats.claims), (0, 5));
}

#[test]
fn get_rewards_reports_supply_and_claims() {
    let f = setup();
    f.rewards.add_reward(&1u32, &30u64, &50i128);
    f.rewards.add_reward(&2u32, &60u64, &100i128);
    f.rewards.set_reward_supply(&1u32, &3u32);
    let u = earner(&f, 30);
    f.rewards.claim_reward(&u, &1u32);

    let rows = f.rewards.get_rewards();
    let r1 = rows.get(0).unwrap();
    assert_eq!((r1.id, r1.max_claims, r1.claims), (1, 3, 1));
    let r2 = rows.get(1).unwrap();
    assert_eq!((r2.id, r2.max_claims, r2.claims), (2, 0, 0));
}

#[test]
fn supply_cannot_drop_below_claims_already_paid() {
    let f = setup();
    f.rewards.add_reward(&1u32, &30u64, &50i128);
    for _ in 0..2 {
        let u = earner(&f, 30);
        f.rewards.claim_reward(&u, &1u32);
    }
    assert_eq!(
        f.rewards.try_set_reward_supply(&1u32, &1u32),
        Err(Ok(contract_err(Error::InvalidSupply)))
    );
    // Capping at exactly the paid count closes the pool; 0 reopens it.
    f.rewards.set_reward_supply(&1u32, &2u32);
    let late = earner(&f, 30);
    assert_eq!(
        f.rewards.try_claim_reward(&late, &1u32),
        Err(Ok(contract_err(Error::RewardExhausted)))
    );
    f.rewards.set_reward_supply(&1u32, &0u32);
    f.rewards.claim_reward(&late, &1u32);
    assert_eq!(f.rewards.get_reward_stats(&1u32).claims, 3);
}

#[test]
fn supply_for_an_unknown_reward_reverts() {
    let f = setup();
    assert_eq!(
        f.rewards.try_set_reward_supply(&9u32, &5u32),
        Err(Ok(contract_err(Error::RewardNotFound)))
    );
}

#[test]
fn streak_gated_reward_claimed_with_live_streak() {
    let f = setup();
    let user = Address::generate(&f.env);

    // Register reward 1: 50 XP, 200 USDC, 2-week streak.
    f.rewards.add_reward(&1u32, &50u64, &200i128);
    f.rewards.set_reward_min_streak(&1u32, &2u32);
    assert_eq!(f.rewards.get_reward_min_streak(&1u32), 2);

    f.quest.create_quest(&1u32, &1u32, &25u64);
    f.quest.create_quest(&2u32, &2u32, &25u64);

    // Week 0: complete quest 1 -> streak 1, 25 XP.
    f.env.ledger().with_mut(|l| l.timestamp = 0);
    award_quest(&f, 1, &user);

    // Week 1: complete quest 2 -> streak 2, 50 XP.
    f.env.ledger().with_mut(|l| l.timestamp = 604_800);
    award_quest(&f, 2, &user);

    // Both XP (50 >= 50) and live streak (2 >= 2) met.
    f.rewards.claim_reward(&user, &1u32);

    let token_c = token::TokenClient::new(&f.env, &f.usdc);
    assert_eq!(token_c.balance(&user), 200);
    assert!(f.rewards.is_claimed(&1u32, &user));
}

#[test]
fn streak_gated_reward_rejected_below_required_streak() {
    let f = setup();
    let user = Address::generate(&f.env);

    // Register reward 1: 50 XP, 200 USDC, 3-week streak.
    f.rewards.add_reward(&1u32, &50u64, &200i128);
    f.rewards.set_reward_min_streak(&1u32, &3u32);

    f.quest.create_quest(&1u32, &1u32, &50u64);
    // Week 0: complete quest 1 -> 50 XP, but streak = 1.
    f.env.ledger().with_mut(|l| l.timestamp = 0);
    award_quest(&f, 1, &user);

    assert_eq!(
        f.rewards.try_claim_reward(&user, &1u32),
        Err(Ok(contract_err(Error::StreakTooShort)))
    );
}

#[test]
fn streak_gated_reward_rejected_when_streak_is_lapsed() {
    let f = setup();
    let user = Address::generate(&f.env);

    f.rewards.add_reward(&1u32, &50u64, &200i128);
    f.rewards.set_reward_min_streak(&1u32, &2u32);

    f.quest.create_quest(&1u32, &1u32, &25u64);
    f.quest.create_quest(&2u32, &2u32, &25u64);

    // Week 0: complete quest 1 -> streak 1.
    f.env.ledger().with_mut(|l| l.timestamp = 0);
    award_quest(&f, 1, &user);

    // Week 1: complete quest 2 -> streak 2, last_week = 1.
    f.env.ledger().with_mut(|l| l.timestamp = 604_800);
    award_quest(&f, 2, &user);

    // Advance to Week 4 (skipped weeks 2 & 3).
    // quest_registry.get_streak still returns weeks = 2, but last_week (1) < current_week (4) - 1.
    f.env.ledger().with_mut(|l| l.timestamp = 604_800 * 4);

    assert_eq!(
        f.rewards.try_claim_reward(&user, &1u32),
        Err(Ok(contract_err(Error::StreakTooShort)))
    );
}

#[test]
fn reward_without_streak_requirement_unchanged() {
    let f = setup();
    let user = Address::generate(&f.env);

    // Reward without streak requirement (min_streak == 0).
    f.rewards.add_reward(&1u32, &50u64, &200i128);

    // User has 50 XP from reputation, but 0 streak on quest_registry.
    f.rep.award_xp(&f.attester, &user, &2u32, &50u64);

    f.rewards.claim_reward(&user, &1u32);

    let token_c = token::TokenClient::new(&f.env, &f.usdc);
    assert_eq!(token_c.balance(&user), 200);
    assert!(f.rewards.is_claimed(&1u32, &user));
}

#[test]
fn get_rewards_reports_min_streak() {
    let f = setup();
    f.rewards.add_reward(&1u32, &30u64, &50i128);
    f.rewards.add_reward(&2u32, &60u64, &100i128);
    f.rewards.set_reward_min_streak(&1u32, &4u32);

    let rows = f.rewards.get_rewards();
    let r1 = rows.get(0).unwrap();
    assert_eq!((r1.id, r1.min_streak), (1, 4));
    let r2 = rows.get(1).unwrap();
    assert_eq!((r2.id, r2.min_streak), (2, 0));
}

#[test]
fn set_reward_min_streak_zero_clears_requirement() {
    let f = setup();
    f.rewards.add_reward(&1u32, &30u64, &50i128);
    f.rewards.set_reward_min_streak(&1u32, &3u32);
    assert_eq!(f.rewards.get_reward_min_streak(&1u32), 3);

    f.rewards.set_reward_min_streak(&1u32, &0u32);
    assert_eq!(f.rewards.get_reward_min_streak(&1u32), 0);
}

#[test]
fn set_reward_min_streak_unknown_reward_reverts() {
    let f = setup();
    assert_eq!(
        f.rewards.try_set_reward_min_streak(&99u32, &3u32),
        Err(Ok(contract_err(Error::RewardNotFound)))
    );
}

