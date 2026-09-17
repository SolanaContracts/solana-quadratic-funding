use anchor_lang::prelude::*;

declare_id!("MUkfDWcaTyHdYCVNnnhDnSiYmwRhAvXNmgAeCex6szC");

pub const MAX_PROJECTS: u8 = 10;

fn isqrt(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = x.div_ceil(2);
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

#[program]
pub mod solana_quadratic_funding {
    use super::*;

    pub fn create_round(ctx: Context<CreateRound>, round_id: u64, deadline_unix: i64) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        require!(deadline_unix > now, QfError::DeadlinePassed);

        let round = &mut ctx.accounts.round;
        round.creator = ctx.accounts.creator.key();
        round.round_id = round_id;
        round.deadline_unix = deadline_unix;
        round.project_owners = [Pubkey::default(); MAX_PROJECTS as usize];
        round.project_count = 0;
        round.matching_pool_total = 0;
        round.total_qf_score = 0;
        round.finalized = false;
        round.bump = ctx.bumps.round;

        Ok(())
    }

    pub fn register_project(ctx: Context<RegisterProject>, name: String) -> Result<()> {
        require!(name.len() <= 50, QfError::NameTooLong);

        let now = Clock::get()?.unix_timestamp;
        {
            let round = &ctx.accounts.round;
            require!(!round.finalized, QfError::RoundFinalized);
            require!(now < round.deadline_unix, QfError::DeadlinePassed);
            require!(
                round.project_count < MAX_PROJECTS,
                QfError::InvalidProjectCount
            );
        }

        let project = &mut ctx.accounts.project;
        project.round = ctx.accounts.round.key();
        project.owner = ctx.accounts.owner.key();
        project.name = name;
        project.total_donated = 0;
        project.sqrt_sum = 0;
        project.donor_count = 0;
        project.qf_score = 0;
        project.claimed = false;
        project.bump = ctx.bumps.project;

        let round = &mut ctx.accounts.round;
        let idx = round.project_count as usize;
        round.project_owners[idx] = ctx.accounts.owner.key();
        round.project_count += 1;

        Ok(())
    }

    pub fn fund_matching_pool(ctx: Context<FundMatchingPool>, amount: u64) -> Result<()> {
        require!(amount > 0, QfError::InvalidAmount);
        require!(!ctx.accounts.round.finalized, QfError::RoundFinalized);

        anchor_lang::system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                anchor_lang::system_program::Transfer {
                    from: ctx.accounts.sponsor.to_account_info(),
                    to: ctx.accounts.round.to_account_info(),
                },
            ),
            amount,
        )?;

        ctx.accounts.round.matching_pool_total = ctx
            .accounts
            .round
            .matching_pool_total
            .checked_add(amount)
            .ok_or(QfError::MathOverflow)?;

        Ok(())
    }

    pub fn donate(ctx: Context<Donate>, amount: u64) -> Result<()> {
        require!(amount > 0, QfError::InvalidAmount);

        let now = Clock::get()?.unix_timestamp;
        {
            let round = &ctx.accounts.round;
            require!(!round.finalized, QfError::RoundFinalized);
            require!(now < round.deadline_unix, QfError::DeadlinePassed);
        }

        anchor_lang::system_program::transfer(
            CpiContext::new(
                ctx.accounts.system_program.to_account_info(),
                anchor_lang::system_program::Transfer {
                    from: ctx.accounts.donor.to_account_info(),
                    to: ctx.accounts.round.to_account_info(),
                },
            ),
            amount,
        )?;

        let contribution = &mut ctx.accounts.contribution;
        let is_new_donor = contribution.amount == 0 && contribution.donor == Pubkey::default();
        let old_amount = contribution.amount;
        let new_amount = old_amount
            .checked_add(amount)
            .ok_or(QfError::MathOverflow)?;

        contribution.donor = ctx.accounts.donor.key();
        contribution.project = ctx.accounts.project.key();
        contribution.amount = new_amount;
        contribution.bump = ctx.bumps.contribution;

        let project = &mut ctx.accounts.project;
        let old_sqrt = isqrt(old_amount) as u128;
        let new_sqrt = isqrt(new_amount) as u128;
        project.sqrt_sum = project
            .sqrt_sum
            .checked_add(new_sqrt)
            .and_then(|v| v.checked_sub(old_sqrt))
            .ok_or(QfError::MathOverflow)?;
        project.total_donated = project
            .total_donated
            .checked_add(amount)
            .ok_or(QfError::MathOverflow)?;
        if is_new_donor {
            project.donor_count = project
                .donor_count
                .checked_add(1)
                .ok_or(QfError::MathOverflow)?;
        }

        Ok(())
    }

    pub fn finalize_round<'info>(
        ctx: Context<'_, '_, 'info, 'info, FinalizeRound<'info>>,
    ) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let project_count = {
            let round = &ctx.accounts.round;
            require!(!round.finalized, QfError::RoundFinalized);
            require!(now > round.deadline_unix, QfError::DeadlineNotReached);
            round.project_count as usize
        };

        require!(
            ctx.remaining_accounts.len() == project_count,
            QfError::InvalidProjectAccount
        );

        let mut total_score: u128 = 0;
        for i in 0..project_count {
            let owner = ctx.accounts.round.project_owners[i];
            let expected_pda = Pubkey::find_program_address(
                &[b"project", ctx.accounts.round.key().as_ref(), owner.as_ref()],
                &crate::ID,
            )
            .0;
            let account_info = &ctx.remaining_accounts[i];
            require_keys_eq!(account_info.key(), expected_pda, QfError::InvalidProjectAccount);

            let mut project: Account<Project> = Account::try_from(account_info)?;
            let score = project
                .sqrt_sum
                .checked_mul(project.sqrt_sum)
                .ok_or(QfError::MathOverflow)?;
            project.qf_score = score;
            total_score = total_score
                .checked_add(score)
                .ok_or(QfError::MathOverflow)?;
            project.exit(&crate::ID)?;
        }

        let round = &mut ctx.accounts.round;
        round.total_qf_score = total_score;
        round.finalized = true;

        Ok(())
    }

    pub fn claim_payout(ctx: Context<ClaimPayout>) -> Result<()> {
        require!(ctx.accounts.round.finalized, QfError::RoundNotFinalized);
        require!(!ctx.accounts.project.claimed, QfError::AlreadyClaimed);

        let match_amount: u64 = if ctx.accounts.round.total_qf_score == 0 {
            0
        } else {
            let raw = (ctx.accounts.round.matching_pool_total as u128)
                .checked_mul(ctx.accounts.project.qf_score)
                .ok_or(QfError::MathOverflow)?
                / ctx.accounts.round.total_qf_score;
            raw.try_into().map_err(|_| QfError::MathOverflow)?
        };

        let payout = ctx
            .accounts
            .project
            .total_donated
            .checked_add(match_amount)
            .ok_or(QfError::MathOverflow)?;

        let round_info = ctx.accounts.round.to_account_info();
        let rent_exempt_minimum = Rent::get()?.minimum_balance(round_info.data_len());
        let available = round_info.lamports().saturating_sub(rent_exempt_minimum);
        require!(payout <= available, QfError::InsufficientFunds);

        **round_info.try_borrow_mut_lamports()? -= payout;
        **ctx
            .accounts
            .owner
            .to_account_info()
            .try_borrow_mut_lamports()? += payout;

        ctx.accounts.project.claimed = true;

        Ok(())
    }
}

#[account]
pub struct Round {
    pub creator: Pubkey,
    pub round_id: u64,
    pub deadline_unix: i64,
    pub project_owners: [Pubkey; MAX_PROJECTS as usize],
    pub project_count: u8,
    pub matching_pool_total: u64,
    pub total_qf_score: u128,
    pub finalized: bool,
    pub bump: u8,
}

impl Round {
    pub const MAX_SIZE: usize = 8 // discriminator
        + 32 // creator
        + 8 // round_id
        + 8 // deadline_unix
        + 32 * MAX_PROJECTS as usize // project_owners
        + 1 // project_count
        + 8 // matching_pool_total
        + 16 // total_qf_score
        + 1 // finalized
        + 1; // bump
}

#[account]
pub struct Project {
    pub round: Pubkey,
    pub owner: Pubkey,
    pub name: String,
    pub total_donated: u64,
    pub sqrt_sum: u128,
    pub donor_count: u32,
    pub qf_score: u128,
    pub claimed: bool,
    pub bump: u8,
}

impl Project {
    pub const MAX_SIZE: usize = 8 // discriminator
        + 32 // round
        + 32 // owner
        + 4 + 50 // name
        + 8 // total_donated
        + 16 // sqrt_sum
        + 4 // donor_count
        + 16 // qf_score
        + 1 // claimed
        + 1; // bump
}

#[account]
pub struct Contribution {
    pub donor: Pubkey,
    pub project: Pubkey,
    pub amount: u64,
    pub bump: u8,
}

impl Contribution {
    pub const MAX_SIZE: usize = 8 + 32 + 32 + 8 + 1;
}

#[derive(Accounts)]
#[instruction(round_id: u64)]
pub struct CreateRound<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,

    #[account(
        init,
        payer = creator,
        space = Round::MAX_SIZE,
        seeds = [b"round", creator.key().as_ref(), round_id.to_le_bytes().as_ref()],
        bump,
    )]
    pub round: Account<'info, Round>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RegisterProject<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(mut)]
    pub round: Account<'info, Round>,

    #[account(
        init,
        payer = owner,
        space = Project::MAX_SIZE,
        seeds = [b"project", round.key().as_ref(), owner.key().as_ref()],
        bump,
    )]
    pub project: Account<'info, Project>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct FundMatchingPool<'info> {
    #[account(mut)]
    pub sponsor: Signer<'info>,

    #[account(mut)]
    pub round: Account<'info, Round>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Donate<'info> {
    #[account(mut)]
    pub donor: Signer<'info>,

    #[account(mut)]
    pub round: Account<'info, Round>,

    #[account(
        mut,
        constraint = project.round == round.key() @ QfError::InvalidProjectAccount,
    )]
    pub project: Account<'info, Project>,

    #[account(
        init_if_needed,
        payer = donor,
        space = Contribution::MAX_SIZE,
        seeds = [b"contribution", project.key().as_ref(), donor.key().as_ref()],
        bump,
    )]
    pub contribution: Account<'info, Contribution>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct FinalizeRound<'info> {
    #[account(mut)]
    pub round: Account<'info, Round>,
    // remaining_accounts: every registered Project PDA, in the same order as
    // round.project_owners.
}

#[derive(Accounts)]
pub struct ClaimPayout<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,

    #[account(mut)]
    pub round: Account<'info, Round>,

    #[account(
        mut,
        has_one = owner @ QfError::InvalidProjectAccount,
        constraint = project.round == round.key() @ QfError::InvalidProjectAccount,
    )]
    pub project: Account<'info, Project>,
}

#[error_code]
pub enum QfError {
    #[msg("Round already has the maximum number of projects")]
    InvalidProjectCount,
    #[msg("Round has already been finalized")]
    RoundFinalized,
    #[msg("Round has not been finalized yet")]
    RoundNotFinalized,
    #[msg("This round's deadline has already passed")]
    DeadlinePassed,
    #[msg("This round's deadline has not been reached yet")]
    DeadlineNotReached,
    #[msg("This project has already claimed its payout")]
    AlreadyClaimed,
    #[msg("Provided project account does not match the expected address")]
    InvalidProjectAccount,
    #[msg("Amount must be greater than zero")]
    InvalidAmount,
    #[msg("Round does not have enough funds for this payout")]
    InsufficientFunds,
    #[msg("Project name must be 50 characters or fewer")]
    NameTooLong,
    #[msg("Arithmetic overflow")]
    MathOverflow,
}
