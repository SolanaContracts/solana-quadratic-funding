# solana-quadratic-funding

An Anchor program for a matching donation pool: many donors give SOL to many different projects in a round, and a shared bonus pool tops up whichever projects got support from the *most different people* — not just the most money. This is the quadratic funding (QF) mechanism popularized by Gitcoin Grants.

## Why

A flat matching pool (e.g. "we'll match every dollar 1:1") just rewards whoever raises the most. QF instead rewards *broad* support: a project with 100 donors giving $1 each scores higher than one donor giving $100, even though both raised the same amount. That's a better proxy for "the community actually wants this" than raw dollars.

## What's new here vs. other repos in this org

- **Many-to-many aggregation** — donors × projects, unlike `solana-charity-donations` (many-to-one) or `solana-savings-circle` (a fixed rotating group).
- **Deterministic integer square root** — QF's formula needs `sqrt`. Solana programs avoid floating point for determinism, so this uses a small Newton's-method `isqrt(u64) -> u64` helper instead.
- **Per-donor bookkeeping to prevent an obvious gaming vector** — QF must sum `sqrt(total from each unique donor)`, not `sqrt` of each individual transaction. Otherwise one donor could inflate a project's score just by splitting a single donation into many small ones. A `Contribution` PDA per (project, donor) tracks cumulative amount so each `donate()` call can back out the old `isqrt` and add the new one.
- **Mutating accounts passed via `remaining_accounts`** — `finalize_round` aggregates every project's score in one instruction using `Account::<Project>::try_from(&account_info)` + manual `.exit()` to persist changes, a step beyond `solana-savings-circle`'s remaining_accounts use (which only read them).

## Instructions

| Instruction | Signer | Description |
|---|---|---|
| `create_round(round_id, deadline_unix)` | creator | Creates a `Round`. |
| `register_project(name)` | project owner | Joins the round (max 10 projects), before the deadline. |
| `fund_matching_pool(amount)` | any sponsor | Adds SOL to the shared bonus pool, before finalize. |
| `donate(amount)` | donor | Gives SOL to one project. Updates that project's per-donor-sqrt bookkeeping. |
| `finalize_round()` | anyone, after the deadline | Computes every project's QF score (`sqrt_sum²`) and the round total. Permissionless — no one has to be trusted to trigger it. |
| `claim_payout()` | project owner | Pays out `total_donated + proportional match` from the round's pooled SOL. |

## Accounts

**`Round`** — PDA at `["round", creator, round_id]`
- `creator`, `round_id`, `deadline_unix`, `project_owners: [Pubkey; 10]`, `project_count`
- `matching_pool_total` — frozen once finalized, the fixed base every project's match is computed against
- `total_qf_score: u128`, `finalized: bool`
- Holds SOL directly (donations and matching-pool funding are commingled here, same "PDA holds lamports directly" style as `Campaign` in `solana-charity-donations`)

**`Project`** — PDA at `["project", round, owner]`
- `round`, `owner`, `name`, `total_donated`, `sqrt_sum: u128` (running Σ isqrt of each donor's cumulative amount), `donor_count`, `qf_score: u128`, `claimed`

**`Contribution`** — PDA at `["contribution", project, donor]`
- `donor`, `project`, `amount` (this donor's cumulative raw amount to this project)

## Limitation: this doesn't stop sybil attacks

The per-donor bookkeeping stops one donor from gaming QF by splitting *their own* contribution into many transactions. It does **not** stop someone from creating many separate wallets to fake broad support — that's an identity problem real QF platforms solve with off-chain verification (BrightID, Gitcoin Passport, etc.), out of scope for this program.

## A note on a confusing error you might see

There's a real version-mismatch bug between `@coral-xyz/anchor@0.32.x` and recent `@solana/web3.js` 1.x releases: anchor's provider constructs `SendTransactionError` with the old 2-argument form (`message, logs`), but the installed web3.js expects a single options object. When a transaction genuinely fails with logs attached, this mismatch swallows the real error and surfaces as `Error: Unknown action 'undefined'` instead. If you see that, the actual problem is almost always a legitimate on-chain rejection (e.g. a deadline that already passed) — reproduce with `connection.simulateTransaction()` or manual `sendRawTransaction` + `getSignatureStatus` polling to see the real logs.

## Building and testing

Requires `solana-cli`, `anchor-cli`, and Rust already installed. This machine needed `platform-tools` v1.57 to avoid an `edition2024` build error (same issue as the other two repos in this org — see `solana-charity-donations`'s README for the one-time fix).

```bash
anchor build --no-idl -- --tools-version v1.57
anchor idl build -o target/idl/solana_quadratic_funding.json -t target/types/solana_quadratic_funding.ts
anchor test --skip-build --no-idl
```

`cargo clippy` (run from `programs/solana-quadratic-funding`) is clean.

## CLI client

`cli/` is a Rust CLI (`qf-cli`, built with `anchor-client` + `clap`) covering every instruction, plus a `show` command to read a round's (and optionally a project's) on-chain state. Defaults to a local validator (`http://127.0.0.1:8899` / `ws://127.0.0.1:8900`) — override with `--url`/`--ws-url` for devnet or mainnet.

```bash
cargo build -p qf-cli
BIN=./target/debug/qf-cli

# local validator + program deploy:
solana-test-validator --reset --quiet &
solana program deploy target/deploy/solana_quadratic_funding.so \
  --program-id target/deploy/solana_quadratic_funding-keypair.json

$BIN create-round --keypair ~/creator.json --round-id 1 --deadline-unix <UNIX_TS>
$BIN register-project --keypair ~/project-owner.json --creator <CREATOR_PUBKEY> --round-id 1 --name "My Project"
$BIN fund-matching-pool --keypair ~/sponsor.json --creator <CREATOR_PUBKEY> --round-id 1 --amount-sol 1.0
$BIN donate --keypair ~/donor.json --creator <CREATOR_PUBKEY> --round-id 1 --project-owner <PROJECT_OWNER_PUBKEY> --amount-sol 0.1

$BIN show --creator <CREATOR_PUBKEY> --round-id 1 --project-owner <PROJECT_OWNER_PUBKEY>

# after the deadline passes:
$BIN finalize-round --keypair ~/anyone.json --creator <CREATOR_PUBKEY> --round-id 1
$BIN claim-payout --keypair ~/project-owner.json --creator <CREATOR_PUBKEY> --round-id 1
```

`finalize-round` is permissionless — the `--keypair` there just pays the transaction fee, it doesn't need to be the creator. It fetches the round's registered project list on-chain and builds the `remaining_accounts` list automatically.

Run `$BIN --help` or `$BIN <command> --help` for the full flag list.
