use std::path::PathBuf;
use std::rc::Rc;

use anchor_client::{
    solana_sdk::{
        commitment_config::CommitmentConfig,
        instruction::AccountMeta,
        pubkey::Pubkey,
        signature::{read_keypair_file, Keypair, Signer},
    },
    Client, Cluster,
};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use solana_quadratic_funding::{accounts, instruction, Project, Round};

const LAMPORTS_PER_SOL: f64 = 1_000_000_000.0;

fn sol_to_lamports(sol: f64) -> u64 {
    (sol * LAMPORTS_PER_SOL) as u64
}

fn lamports_to_sol(lamports: u64) -> f64 {
    lamports as f64 / LAMPORTS_PER_SOL
}

#[derive(Parser)]
#[command(name = "qf-cli", about = "CLI client for the solana-quadratic-funding Anchor program")]
struct Cli {
    /// JSON-RPC URL of the cluster to talk to
    #[arg(long, global = true, default_value = "http://127.0.0.1:8899")]
    url: String,

    /// WebSocket URL of the cluster (used for transaction confirmation)
    #[arg(long, global = true, default_value = "ws://127.0.0.1:8900")]
    ws_url: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new funding round
    CreateRound {
        /// Keypair file for the round creator
        #[arg(long)]
        keypair: PathBuf,
        /// Arbitrary id so one creator can start multiple rounds
        #[arg(long)]
        round_id: u64,
        /// Unix timestamp after which donations/registration close
        #[arg(long)]
        deadline_unix: i64,
    },
    /// Register a project in a round
    RegisterProject {
        /// Keypair file for the project owner
        #[arg(long)]
        keypair: PathBuf,
        /// Round creator's public key
        #[arg(long)]
        creator: Pubkey,
        #[arg(long)]
        round_id: u64,
        #[arg(long)]
        name: String,
    },
    /// Add SOL to a round's shared matching pool
    FundMatchingPool {
        /// Keypair file for the sponsor
        #[arg(long)]
        keypair: PathBuf,
        #[arg(long)]
        creator: Pubkey,
        #[arg(long)]
        round_id: u64,
        #[arg(long)]
        amount_sol: f64,
    },
    /// Donate SOL to a project in a round
    Donate {
        /// Keypair file for the donor
        #[arg(long)]
        keypair: PathBuf,
        #[arg(long)]
        creator: Pubkey,
        #[arg(long)]
        round_id: u64,
        /// Public key of the project owner to donate to
        #[arg(long)]
        project_owner: Pubkey,
        #[arg(long)]
        amount_sol: f64,
    },
    /// Compute QF scores for every registered project (permissionless, after the deadline)
    FinalizeRound {
        /// Keypair file to pay the transaction fee (anyone can call this)
        #[arg(long)]
        keypair: PathBuf,
        #[arg(long)]
        creator: Pubkey,
        #[arg(long)]
        round_id: u64,
    },
    /// Claim a project's donations + matching payout
    ClaimPayout {
        /// Keypair file for the project owner
        #[arg(long)]
        keypair: PathBuf,
        #[arg(long)]
        creator: Pubkey,
        #[arg(long)]
        round_id: u64,
    },
    /// Print a round's state, and optionally one project's state
    Show {
        #[arg(long)]
        creator: Pubkey,
        #[arg(long)]
        round_id: u64,
        /// Public key of a project owner to also show project-level detail for
        #[arg(long)]
        project_owner: Option<Pubkey>,
    },
}

fn round_pda(creator: &Pubkey, round_id: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[b"round", creator.as_ref(), &round_id.to_le_bytes()],
        &solana_quadratic_funding::ID,
    )
    .0
}

fn project_pda(round: &Pubkey, owner: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"project", round.as_ref(), owner.as_ref()],
        &solana_quadratic_funding::ID,
    )
    .0
}

fn contribution_pda(project: &Pubkey, donor: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"contribution", project.as_ref(), donor.as_ref()],
        &solana_quadratic_funding::ID,
    )
    .0
}

fn load_keypair(path: &PathBuf) -> Result<Keypair> {
    read_keypair_file(path)
        .map_err(|e| anyhow::anyhow!("failed to read keypair at {}: {e}", path.display()))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cluster = Cluster::Custom(cli.url.clone(), cli.ws_url.clone());

    match cli.command {
        Command::CreateRound {
            keypair,
            round_id,
            deadline_unix,
        } => {
            let creator = Rc::new(load_keypair(&keypair)?);
            let client = Client::new_with_options(cluster, creator.clone(), CommitmentConfig::confirmed());
            let program = client.program(solana_quadratic_funding::ID)?;
            let round = round_pda(&creator.pubkey(), round_id);

            let sig = program
                .request()
                .accounts(accounts::CreateRound {
                    creator: creator.pubkey(),
                    round,
                    system_program: anchor_client::solana_sdk::system_program::ID,
                })
                .args(instruction::CreateRound {
                    round_id,
                    deadline_unix,
                })
                .send()
                .context("create_round transaction failed")?;

            println!("Round created at {round}");
            println!("Signature: {sig}");
        }

        Command::RegisterProject {
            keypair,
            creator,
            round_id,
            name,
        } => {
            let owner = Rc::new(load_keypair(&keypair)?);
            let client = Client::new_with_options(cluster, owner.clone(), CommitmentConfig::confirmed());
            let program = client.program(solana_quadratic_funding::ID)?;
            let round = round_pda(&creator, round_id);
            let project = project_pda(&round, &owner.pubkey());

            let sig = program
                .request()
                .accounts(accounts::RegisterProject {
                    owner: owner.pubkey(),
                    round,
                    project,
                    system_program: anchor_client::solana_sdk::system_program::ID,
                })
                .args(instruction::RegisterProject { name: name.clone() })
                .send()
                .context("register_project transaction failed")?;

            println!("Registered \"{name}\" at {project}");
            println!("Signature: {sig}");
        }

        Command::FundMatchingPool {
            keypair,
            creator,
            round_id,
            amount_sol,
        } => {
            let sponsor = Rc::new(load_keypair(&keypair)?);
            let client = Client::new_with_options(cluster, sponsor.clone(), CommitmentConfig::confirmed());
            let program = client.program(solana_quadratic_funding::ID)?;
            let round = round_pda(&creator, round_id);

            let sig = program
                .request()
                .accounts(accounts::FundMatchingPool {
                    sponsor: sponsor.pubkey(),
                    round,
                    system_program: anchor_client::solana_sdk::system_program::ID,
                })
                .args(instruction::FundMatchingPool {
                    amount: sol_to_lamports(amount_sol),
                })
                .send()
                .context("fund_matching_pool transaction failed")?;

            println!("Added {amount_sol} SOL to the matching pool for round {round}");
            println!("Signature: {sig}");
        }

        Command::Donate {
            keypair,
            creator,
            round_id,
            project_owner,
            amount_sol,
        } => {
            let donor = Rc::new(load_keypair(&keypair)?);
            let client = Client::new_with_options(cluster, donor.clone(), CommitmentConfig::confirmed());
            let program = client.program(solana_quadratic_funding::ID)?;
            let round = round_pda(&creator, round_id);
            let project = project_pda(&round, &project_owner);
            let contribution = contribution_pda(&project, &donor.pubkey());

            let sig = program
                .request()
                .accounts(accounts::Donate {
                    donor: donor.pubkey(),
                    round,
                    project,
                    contribution,
                    system_program: anchor_client::solana_sdk::system_program::ID,
                })
                .args(instruction::Donate {
                    amount: sol_to_lamports(amount_sol),
                })
                .send()
                .context("donate transaction failed")?;

            println!("Donated {amount_sol} SOL to project {project}");
            println!("Signature: {sig}");
        }

        Command::FinalizeRound {
            keypair,
            creator,
            round_id,
        } => {
            let payer = Rc::new(load_keypair(&keypair)?);
            let client = Client::new_with_options(cluster, payer.clone(), CommitmentConfig::confirmed());
            let program = client.program(solana_quadratic_funding::ID)?;
            let round = round_pda(&creator, round_id);
            let round_state: Round = program
                .account(round)
                .context("failed to fetch round (does it exist?)")?;

            let remaining: Vec<AccountMeta> = round_state.project_owners
                [..round_state.project_count as usize]
                .iter()
                .map(|owner| AccountMeta::new(project_pda(&round, owner), false))
                .collect();

            let sig = program
                .request()
                .accounts(accounts::FinalizeRound { round })
                .accounts(remaining)
                .args(instruction::FinalizeRound {})
                .send()
                .context("finalize_round transaction failed")?;

            println!("Finalized round {round}");
            println!("Signature: {sig}");
        }

        Command::ClaimPayout {
            keypair,
            creator,
            round_id,
        } => {
            let owner = Rc::new(load_keypair(&keypair)?);
            let client = Client::new_with_options(cluster, owner.clone(), CommitmentConfig::confirmed());
            let program = client.program(solana_quadratic_funding::ID)?;
            let round = round_pda(&creator, round_id);
            let project = project_pda(&round, &owner.pubkey());

            let sig = program
                .request()
                .accounts(accounts::ClaimPayout {
                    owner: owner.pubkey(),
                    round,
                    project,
                })
                .args(instruction::ClaimPayout {})
                .send()
                .context("claim_payout transaction failed")?;

            println!("Claimed payout for project {project}");
            println!("Signature: {sig}");
        }

        Command::Show {
            creator,
            round_id,
            project_owner,
        } => {
            let dummy_payer = Rc::new(Keypair::new());
            let client = Client::new_with_options(cluster, dummy_payer, CommitmentConfig::confirmed());
            let program = client.program(solana_quadratic_funding::ID)?;
            let round = round_pda(&creator, round_id);
            let round_state: Round = program
                .account(round)
                .context("failed to fetch round (does it exist?)")?;

            println!("Round: {round}");
            println!("  creator:            {}", round_state.creator);
            println!("  deadline (unix):    {}", round_state.deadline_unix);
            println!(
                "  projects:           {}/10",
                round_state.project_count
            );
            for i in 0..round_state.project_count as usize {
                println!("    [{i}] {}", round_state.project_owners[i]);
            }
            println!(
                "  matching pool:      {} SOL",
                lamports_to_sol(round_state.matching_pool_total)
            );
            println!("  finalized:          {}", round_state.finalized);
            println!("  total QF score:     {}", round_state.total_qf_score);

            if let Some(owner) = project_owner {
                let project_key = project_pda(&round, &owner);
                let project: Project = program
                    .account(project_key)
                    .context("failed to fetch project (is it registered?)")?;
                println!("Project: {}", project.name);
                println!("  address:            {project_key}");
                println!("  owner:              {}", project.owner);
                println!(
                    "  total donated:      {} SOL",
                    lamports_to_sol(project.total_donated)
                );
                println!("  donor count:        {}", project.donor_count);
                println!("  sqrt_sum:           {}", project.sqrt_sum);
                println!("  qf_score:           {}", project.qf_score);
                println!("  claimed:            {}", project.claimed);
            }
        }
    }

    Ok(())
}
