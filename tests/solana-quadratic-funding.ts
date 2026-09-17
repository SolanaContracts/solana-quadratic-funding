import * as anchor from "@coral-xyz/anchor";
import { BN, Program } from "@coral-xyz/anchor";
import { Keypair, PublicKey, SystemProgram, LAMPORTS_PER_SOL } from "@solana/web3.js";
import { assert } from "chai";
import { SolanaQuadraticFunding } from "../target/types/solana_quadratic_funding";

describe("solana-quadratic-funding", () => {
  // "confirmed" commitment avoids a flaky confirmTransaction race against
  // solana-test-validator that "processed" (Anchor's default) can hit under
  // a burst of back-to-back transactions.
  const envProvider = anchor.AnchorProvider.env();
  const provider = new anchor.AnchorProvider(envProvider.connection, envProvider.wallet, {
    commitment: "confirmed",
    preflightCommitment: "confirmed",
  });
  anchor.setProvider(provider);
  const program = anchor.workspace
    .solanaQuadraticFunding as Program<SolanaQuadraticFunding>;

  const creator = Keypair.generate();
  const sponsor = Keypair.generate();
  const projectAOwner = Keypair.generate(); // 1 donor giving a lot
  const projectBOwner = Keypair.generate(); // many donors giving a little each
  const bigDonor = Keypair.generate();
  const smallDonors = Array.from({ length: 9 }, () => Keypair.generate());

  const ROUND_ID = new BN(1);
  // Must comfortably outlast the ~10 sequential donate() RPC round-trips in
  // the donation test below (each can take several hundred ms on a local
  // validator) plus the setup tests that run before it.
  const DEADLINE_BUFFER_SECS = 20;
  let deadline: BN;
  let roundPda: PublicKey;

  const findRoundPda = () =>
    PublicKey.findProgramAddressSync(
      [Buffer.from("round"), creator.publicKey.toBuffer(), ROUND_ID.toArrayLike(Buffer, "le", 8)],
      program.programId
    )[0];

  const findProjectPda = (owner: PublicKey) =>
    PublicKey.findProgramAddressSync(
      [Buffer.from("project"), roundPda.toBuffer(), owner.toBuffer()],
      program.programId
    )[0];

  const findContributionPda = (project: PublicKey, donor: PublicKey) =>
    PublicKey.findProgramAddressSync(
      [Buffer.from("contribution"), project.toBuffer(), donor.toBuffer()],
      program.programId
    )[0];

  before(async () => {
    const everyone = [creator, sponsor, projectAOwner, projectBOwner, bigDonor, ...smallDonors];
    for (const kp of everyone) {
      const sig = await provider.connection.requestAirdrop(kp.publicKey, 2 * LAMPORTS_PER_SOL);
      await provider.connection.confirmTransaction(sig, "confirmed");
    }

    const nowSec = Math.floor(Date.now() / 1000);
    deadline = new BN(nowSec + DEADLINE_BUFFER_SECS);
    roundPda = findRoundPda();
  });

  it("creates a round", async () => {
    await program.methods
      .createRound(ROUND_ID, deadline)
      .accounts({
        creator: creator.publicKey,
        round: roundPda,
        systemProgram: SystemProgram.programId,
      })
      .signers([creator])
      .rpc();

    const round = await program.account.round.fetch(roundPda);
    assert.equal(round.projectCount, 0);
    assert.isFalse(round.finalized);
  });

  it("registers two projects", async () => {
    for (const owner of [projectAOwner, projectBOwner]) {
      await program.methods
        .registerProject(owner === projectAOwner ? "Project A" : "Project B")
        .accounts({
          owner: owner.publicKey,
          round: roundPda,
          project: findProjectPda(owner.publicKey),
          systemProgram: SystemProgram.programId,
        })
        .signers([owner])
        .rpc();
    }

    const round = await program.account.round.fetch(roundPda);
    assert.equal(round.projectCount, 2);
  });

  it("funds the matching pool", async () => {
    await program.methods
      .fundMatchingPool(new BN(1 * LAMPORTS_PER_SOL))
      .accounts({
        sponsor: sponsor.publicKey,
        round: roundPda,
        systemProgram: SystemProgram.programId,
      })
      .signers([sponsor])
      .rpc();

    const round = await program.account.round.fetch(roundPda);
    assert.equal(round.matchingPoolTotal.toNumber(), 1 * LAMPORTS_PER_SOL);
  });

  it("lets one donor fund project A, and nine donors fund project B with the same total", async () => {
    const projectAPda = findProjectPda(projectAOwner.publicKey);
    const projectBPda = findProjectPda(projectBOwner.publicKey);
    const perDonorAmount = 0.01 * LAMPORTS_PER_SOL;

    // Project A: a single donor gives 9 * perDonorAmount in one shot.
    await program.methods
      .donate(new BN(perDonorAmount * 9))
      .accounts({
        donor: bigDonor.publicKey,
        round: roundPda,
        project: projectAPda,
        contribution: findContributionPda(projectAPda, bigDonor.publicKey),
        systemProgram: SystemProgram.programId,
      })
      .signers([bigDonor])
      .rpc();

    // Project B: nine distinct donors each give perDonorAmount -> same total as A.
    for (const donor of smallDonors) {
      await program.methods
        .donate(new BN(perDonorAmount))
        .accounts({
          donor: donor.publicKey,
          round: roundPda,
          project: projectBPda,
          contribution: findContributionPda(projectBPda, donor.publicKey),
          systemProgram: SystemProgram.programId,
        })
        .signers([donor])
        .rpc();
    }

    const projectA = await program.account.project.fetch(projectAPda);
    const projectB = await program.account.project.fetch(projectBPda);
    assert.equal(projectA.totalDonated.toString(), projectB.totalDonated.toString());
    assert.equal(projectA.donorCount, 1);
    assert.equal(projectB.donorCount, 9);
    // broad-based support must score higher despite an identical total raised
    assert.isTrue(new BN(projectB.sqrtSum).gt(new BN(projectA.sqrtSum)));
  });

  it("rejects donating after the deadline, and finalizing before it", async () => {
    try {
      await program.methods
        .finalizeRound()
        .accounts({ round: roundPda })
        .remainingAccounts([
          { pubkey: findProjectPda(projectAOwner.publicKey), isWritable: true, isSigner: false },
          { pubkey: findProjectPda(projectBOwner.publicKey), isWritable: true, isSigner: false },
        ])
        .rpc();
      assert.fail("expected finalize_round to fail before the deadline");
    } catch (err) {
      assert.include(String(err), "DeadlineNotReached");
    }

    await new Promise((resolve) => setTimeout(resolve, (DEADLINE_BUFFER_SECS + 2) * 1000));

    try {
      await program.methods
        .donate(new BN(1000))
        .accounts({
          donor: bigDonor.publicKey,
          round: roundPda,
          project: findProjectPda(projectAOwner.publicKey),
          contribution: findContributionPda(findProjectPda(projectAOwner.publicKey), bigDonor.publicKey),
          systemProgram: SystemProgram.programId,
        })
        .signers([bigDonor])
        .rpc();
      assert.fail("expected donate to fail after the deadline");
    } catch (err) {
      assert.include(String(err), "DeadlinePassed");
    }
  });

  it("rejects finalizing with a mismatched remaining_accounts list", async () => {
    try {
      await program.methods
        .finalizeRound()
        .accounts({ round: roundPda })
        .remainingAccounts([
          // wrong order
          { pubkey: findProjectPda(projectBOwner.publicKey), isWritable: true, isSigner: false },
          { pubkey: findProjectPda(projectAOwner.publicKey), isWritable: true, isSigner: false },
        ])
        .rpc();
      assert.fail("expected finalize_round to fail");
    } catch (err) {
      assert.include(String(err), "InvalidProjectAccount");
    }
  });

  it("finalizes the round and computes QF scores", async () => {
    await program.methods
      .finalizeRound()
      .accounts({ round: roundPda })
      .remainingAccounts([
        { pubkey: findProjectPda(projectAOwner.publicKey), isWritable: true, isSigner: false },
        { pubkey: findProjectPda(projectBOwner.publicKey), isWritable: true, isSigner: false },
      ])
      .rpc();

    const round = await program.account.round.fetch(roundPda);
    assert.isTrue(round.finalized);

    const projectA = await program.account.project.fetch(findProjectPda(projectAOwner.publicKey));
    const projectB = await program.account.project.fetch(findProjectPda(projectBOwner.publicKey));
    // same raw total donated, but B's broad support gives it a higher QF score
    assert.isTrue(new BN(projectB.qfScore).gt(new BN(projectA.qfScore)));
  });

  it("pays out project B a larger match than project A despite equal donations", async () => {
    const balanceBeforeA = await provider.connection.getBalance(projectAOwner.publicKey);
    const balanceBeforeB = await provider.connection.getBalance(projectBOwner.publicKey);

    await program.methods
      .claimPayout()
      .accounts({
        owner: projectAOwner.publicKey,
        round: roundPda,
        project: findProjectPda(projectAOwner.publicKey),
      })
      .signers([projectAOwner])
      .rpc();

    await program.methods
      .claimPayout()
      .accounts({
        owner: projectBOwner.publicKey,
        round: roundPda,
        project: findProjectPda(projectBOwner.publicKey),
      })
      .signers([projectBOwner])
      .rpc();

    const balanceAfterA = await provider.connection.getBalance(projectAOwner.publicKey);
    const balanceAfterB = await provider.connection.getBalance(projectBOwner.publicKey);
    const payoutA = balanceAfterA - balanceBeforeA;
    const payoutB = balanceAfterB - balanceBeforeB;

    assert.isTrue(payoutB > payoutA, "project B (broad support) should receive a bigger payout");
  });

  it("rejects a double claim", async () => {
    try {
      await program.methods
        .claimPayout()
        .accounts({
          owner: projectAOwner.publicKey,
          round: roundPda,
          project: findProjectPda(projectAOwner.publicKey),
        })
        .signers([projectAOwner])
        .rpc();
      assert.fail("expected the second claim to fail");
    } catch (err) {
      assert.include(String(err), "AlreadyClaimed");
    }
  });
});
